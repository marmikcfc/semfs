//! Capability-based backend resolution (Phase 5).
//!
//! The DEFAULT embedder and reranker are local **fastembed-rs registry** models
//! (auto-downloaded + cached); cloud providers are opt-in via the
//! `SEMFS_EMBED_BACKEND` / `SEMFS_RERANK_BACKEND` env vars. The `choose_*`
//! functions are pure (no I/O) and table-tested; `build_*` turn a choice into a
//! live backend. Both the daemon (indexing) and `grep` (search) resolve through
//! here so they always agree on the embedder (and thus the vector space).

use std::sync::Arc;

use anyhow::Result;
use semfs_core::embed::{Embedder, EmbeddingModel, LocalEmbedder, OpenAiEmbedder};
use semfs_core::rerank::{CohereReranker, LocalReranker, RelaceReranker, Reranker, RerankerModel};

/// The fastembed-rs registry models we standardize on (project goal).
// Multilingual text embedder (384d — same dim as the prior English-only
// arctic-embed-s, so it's a drop-in for the vec0 schema; but vectors are
// model-specific, so a swap REQUIRES a fresh seed). The corpus is heavily
// non-English (Chinese) and arctic-embed-s gave poor cross-lingual recall —
// English code/roadmap files outranked Chinese sales data. The reranker below is
// already multilingual, so retrieval was the weak link. See
// tickets/local-ranking-precision-vs-supermemory.
const TEXT_EMBED_MODEL: EmbeddingModel = EmbeddingModel::MultilingualE5Small; // 384d, multilingual
const CODE_EMBED_MODEL: EmbeddingModel = EmbeddingModel::JinaEmbeddingsV2BaseCode; // 768d

/// Map `SEMFS_EMBED_MODEL` → a fastembed registry text model. Unknown/absent →
/// the default (`TEXT_EMBED_MODEL`). Keep names short + stable; dims are read
/// from the registry, so a swap requires a re-seed (different index identity).
fn text_embed_model(name: Option<&str>) -> EmbeddingModel {
    match name {
        Some("embeddinggemma") | Some("gemma") | Some("embeddinggemma-300m") => {
            EmbeddingModel::EmbeddingGemma300M // 768d, multilingual
        }
        Some("e5-small") | Some("multilingual-e5-small") => EmbeddingModel::MultilingualE5Small,
        Some("arctic-s") | Some("snowflake-arctic-embed-s") => EmbeddingModel::SnowflakeArcticEmbedS,
        _ => TEXT_EMBED_MODEL,
    }
}
const RERANK_MODEL: RerankerModel = RerankerModel::JINARerankerV2BaseMultiligual;
/// The int8-quantized ONNX of the reranker (smaller/faster than the registry's
/// pinned full-precision `onnx/model.onnx`), fetched from the model's HF repo.
const RERANK_ONNX: &str = "onnx/model_int8.onnx";
/// Pinned commit of `jinaai/jina-reranker-v2-base-multilingual` so the int8 ONNX
/// + tokenizer are reproducible (an HF HEAD update can't swap them underneath us).
const RERANK_REV: &str = "9cfeff2df7d40d1b78e75e5e9cebec92a99813c9";
/// Cloud OpenAI embedding fallback dims (text-embedding-3-small).
const CLOUD_OPENAI_DIMS: usize = 1536;

/// Signals the resolver reads from the environment.
#[derive(Debug, Clone, Default)]
pub struct ResolveEnv {
    /// `SEMFS_EMBED_BACKEND`: `local` (default) | `openai` | `openrouter`.
    pub embed_backend: Option<String>,
    /// `SEMFS_EMBED_MODEL`: override the LOCAL text-embed registry model
    /// (default `multilingual-e5-small`). e.g. `embeddinggemma` | `e5-small` |
    /// `arctic-s`. Lets us evaluate stronger multilingual embedders.
    pub embed_model: Option<String>,
    /// `SEMFS_RERANK_BACKEND`: `local` (default) | `cohere` | `relace` | `none`.
    pub rerank_backend: Option<String>,
    /// `SEMFS_RERANK_MODEL`: override the model slug for the `cohere`-schema cloud
    /// reranker (default `cohere/rerank-v3.5`). Any Cohere `/rerank`-compatible
    /// slug on the chosen base URL works, e.g. `cohere/rerank-4-pro`.
    pub rerank_model: Option<String>,
    /// `SEMFS_RERANK_BASE_URL`: override the base URL for the `cohere`-schema cloud
    /// reranker (default OpenRouter `https://openrouter.ai/api/v1`). Lets any
    /// Cohere-`/rerank`-schema endpoint be used without new per-vendor code.
    pub rerank_base_url: Option<String>,
    /// `SEMFS_STORAGE_BACKEND`: `sqlite` (default) | `pgvector` | `pglite` | `cloud`.
    /// Where search happens. `cloud` = no local index (Supermemory embeds + searches);
    /// `pgvector` requires the `pg` feature and `SEMFS_PG_URL`; `pglite` the `pglite`
    /// feature. This axis — not the embedder — decides local vs. cloud search.
    pub storage_backend: Option<String>,
    /// `SEMFS_PG_URL`: Postgres connection string for the pgvector backend.
    /// Only read under the `pg` feature (the pgvector builder).
    #[cfg_attr(not(feature = "pg"), allow(dead_code))]
    pub pg_url: Option<String>,
    pub openrouter_key: Option<String>,
    pub relace_key: Option<String>,
    pub openai_key: Option<String>,
}

impl ResolveEnv {
    /// Read signals from the process environment.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Self {
            embed_backend: var("SEMFS_EMBED_BACKEND"),
            embed_model: var("SEMFS_EMBED_MODEL"),
            rerank_backend: var("SEMFS_RERANK_BACKEND"),
            rerank_model: var("SEMFS_RERANK_MODEL"),
            rerank_base_url: var("SEMFS_RERANK_BASE_URL"),
            storage_backend: var("SEMFS_STORAGE_BACKEND"),
            pg_url: var("SEMFS_PG_URL"),
            openrouter_key: var("OPENROUTER_API_KEY"),
            relace_key: var("RELACE_API_KEY"),
            openai_key: var("OPENAI_API_KEY"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EmbedChoice {
    Local,
    CloudOpenAi,
    CloudOpenRouter,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RerankChoice {
    Local,
    Cohere,
    Relace,
    None,
}

/// Embedder choice — local fastembed registry by default; cloud providers opt-in.
pub fn choose_embed(env: &ResolveEnv) -> EmbedChoice {
    match env.embed_backend.as_deref() {
        Some("openai") => EmbedChoice::CloudOpenAi,
        Some("openrouter") => EmbedChoice::CloudOpenRouter,
        _ => EmbedChoice::Local,
    }
}

/// Reranker choice — local fastembed registry by default; cloud/none opt-in.
pub fn choose_rerank(env: &ResolveEnv) -> RerankChoice {
    match env.rerank_backend.as_deref() {
        Some("none") => RerankChoice::None,
        Some("cohere") => RerankChoice::Cohere,
        Some("relace") => RerankChoice::Relace,
        _ => RerankChoice::Local,
    }
}

/// Build the resolved TEXT embedder.
pub fn build_embedder(env: &ResolveEnv) -> Result<Arc<dyn Embedder>> {
    // `hash` was a fake embedder used as a cloud-routing hack — removed. Reject it
    // explicitly (rather than silently defaulting to local, which would change a
    // `hash` user's runtime backend with no signal) and point at the replacement.
    // This fires only where a real local embedder is actually needed: a `hash`
    // mount on the default SQLite backend fails open to cloud (the old behavior,
    // now with a clear log); `cloud` storage never reaches here.
    if env.embed_backend.as_deref() == Some("hash") {
        anyhow::bail!(
            "SEMFS_EMBED_BACKEND=hash was removed (it was a fake, no-semantics embedder \
             used only to route search to the cloud). For cloud search set \
             SEMFS_STORAGE_BACKEND=cloud; otherwise choose a real embedder \
             (local|openai|openrouter)."
        );
    }
    // Non-silent default: if we're defaulting to local registry models while a
    // cloud key is present (and no backend was explicitly chosen), say so — local
    // is the intended default but the switch shouldn't surprise a key-only config.
    if env.embed_backend.is_none()
        && choose_embed(env) == EmbedChoice::Local
        && (env.openai_key.is_some() || env.openrouter_key.is_some())
    {
        tracing::info!(
            "defaulting to local fastembed models (downloads on first use); \
             set SEMFS_EMBED_BACKEND=openrouter|openai to use a cloud embedder"
        );
    }
    Ok(match choose_embed(env) {
        // BYO-ONNX route: `SEMFS_EMBED_MODEL=gemma-q4` loads a custom Q4 gemma ONNX
        // from `SEMFS_EMBED_ONNX_DIR` (default `$HOME/gemma_q4`), since the fastembed
        // registry exposes only fp32 gemma and has no Q4 mode. Everything else uses
        // the registry. The q4 embedder's identity (`byo:gemma-q4-onnx:768`) differs
        // from the registry's, so a q4 seed and an fp32 seed can't be cross-read.
        EmbedChoice::Local if env.embed_model.as_deref() == Some("gemma-q4") => {
            let dir = std::env::var("SEMFS_EMBED_ONNX_DIR")
                .unwrap_or_else(|_| format!("{}/gemma_q4", std::env::var("HOME").unwrap_or_default()));
            Arc::new(LocalEmbedder::from_onnx_dir(
                std::path::Path::new(&dir),
                768,
                "model_q4",
                "gemma-q4-onnx",
            )?)
        }
        EmbedChoice::Local => Arc::new(LocalEmbedder::from_registry(
            text_embed_model(env.embed_model.as_deref()),
            None,
        )?),
        EmbedChoice::CloudOpenAi => Arc::new(OpenAiEmbedder::new(
            env.openai_key.clone().ok_or_else(|| {
                anyhow::anyhow!("SEMFS_EMBED_BACKEND=openai but OPENAI_API_KEY not set")
            })?,
            "https://api.openai.com/v1".to_string(),
            "text-embedding-3-small".to_string(),
            CLOUD_OPENAI_DIMS,
        )),
        EmbedChoice::CloudOpenRouter => Arc::new(OpenAiEmbedder::openrouter(
            env.openrouter_key.clone().ok_or_else(|| {
                anyhow::anyhow!("SEMFS_EMBED_BACKEND=openrouter but OPENROUTER_API_KEY not set")
            })?,
        )),
    })
}

/// Build the resolved CODE embedder, or `None` when the active embed backend has
/// no separate code lane (only the local fastembed registry routes code files to
/// a dedicated model; cloud/hash embed everything with the single text embedder).
pub fn build_code_embedder(env: &ResolveEnv) -> Result<Option<Arc<dyn Embedder>>> {
    Ok(match choose_embed(env) {
        EmbedChoice::Local => Some(Arc::new(LocalEmbedder::from_registry(CODE_EMBED_MODEL, None)?)),
        _ => None,
    })
}

/// Build an LLM client (OpenRouter `gpt-4.1-nano`) for L4 rewrite / L7 extraction,
/// or `None` when no key is available.
pub fn build_llm(env: &ResolveEnv) -> Option<semfs_core::llm::LlmClient> {
    env.openrouter_key
        .as_ref()
        .map(|k| semfs_core::llm::LlmClient::openrouter(k.clone()))
}

/// Build the resolved reranker, or `None` to skip L5.
pub fn build_reranker(env: &ResolveEnv) -> Result<Option<Arc<dyn Reranker>>> {
    Ok(match choose_rerank(env) {
        RerankChoice::Local => Some(Arc::new(LocalReranker::from_registry_onnx(
            RERANK_MODEL,
            RERANK_ONNX,
            RERANK_REV,
            None,
        )?)),
        RerankChoice::Cohere => {
            let key = env.openrouter_key.clone().ok_or_else(|| {
                anyhow::anyhow!("SEMFS_RERANK_BACKEND=cohere but OPENROUTER_API_KEY not set")
            })?;
            // Defaults preserve prior behavior (OpenRouter + rerank-v3.5); both are
            // overridable so any Cohere-`/rerank`-schema model/endpoint works
            // (e.g. SEMFS_RERANK_MODEL=cohere/rerank-4-pro).
            let base_url = env
                .rerank_base_url
                .clone()
                .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
            let model = env
                .rerank_model
                .clone()
                .unwrap_or_else(|| "cohere/rerank-v3.5".to_string());
            Some(Arc::new(CohereReranker::new(key, base_url, model)))
        }
        RerankChoice::Relace => Some(Arc::new(RelaceReranker::new(
            env.relace_key.clone().ok_or_else(|| {
                anyhow::anyhow!("SEMFS_RERANK_BACKEND=relace but RELACE_API_KEY not set")
            })?,
        ))),
        RerankChoice::None => None,
    })
}

/// Where search happens. This — NOT the embedder — is the local-vs-cloud router.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageChoice {
    Sqlite,
    /// External Postgres (you run it), via `SEMFS_PG_URL`.
    Pgvector,
    /// Embedded pglite (shipped in-box), persisting under the cache dir.
    Pglite,
    /// No local index — Supermemory stores + searches (it embeds query + docs).
    /// The mount builds no local index; `grep` routes to `CloudIndex`.
    Cloud,
}

impl StorageChoice {
    /// Canonical token persisted in the `.semfs` marker so `grep` can recover the
    /// mounted backend without depending on its own (possibly-drifted) env.
    pub fn as_str(self) -> &'static str {
        match self {
            StorageChoice::Sqlite => "sqlite",
            StorageChoice::Pgvector => "pgvector",
            StorageChoice::Pglite => "pglite",
            StorageChoice::Cloud => "cloud",
        }
    }

    /// True for every backend that builds + searches a LOCAL index (so it needs a
    /// real local embedder). False only for `Cloud`, which has no local index. This
    /// is the routing predicate that replaced the old embedder-sniffing
    /// `local_indexing_enabled` (see tickets/remove-hash-embedder).
    pub fn is_local(self) -> bool {
        !matches!(self, StorageChoice::Cloud)
    }
}

/// Map a backend token (from `SEMFS_STORAGE_BACKEND` or the persisted marker) to a
/// `StorageChoice`. Unknown/absent → SQLite (the default + historical behavior).
pub fn storage_choice_from(s: Option<&str>) -> StorageChoice {
    match s {
        Some("pgvector") | Some("pg") | Some("postgres") => StorageChoice::Pgvector,
        Some("pglite") | Some("embedded") => StorageChoice::Pglite,
        Some("cloud") | Some("supermemory") => StorageChoice::Cloud,
        _ => StorageChoice::Sqlite,
    }
}

/// Storage backend selection — SQLite (default); external Postgres or embedded
/// pglite opt-in via `SEMFS_STORAGE_BACKEND`.
pub fn choose_storage(env: &ResolveEnv) -> StorageChoice {
    storage_choice_from(env.storage_backend.as_deref())
}

/// Build a `PgVectorStore` from the resolved embedder + reranker + LLM graph
/// extractor. Async (sqlx connect). Only compiled with the `pg` feature.
///
/// Note: pgvector is a single-embedder backend — there's no code lane, so the
/// text embedder handles all files (code routing is a SQLite-only feature today).
#[cfg(feature = "pg")]
pub async fn build_pg_store(
    env: &ResolveEnv,
    container: &str,
    embedder: Arc<dyn Embedder>,
) -> Result<semfs_core::backend::pgvector::PgVectorStore> {
    let url = env
        .pg_url
        .clone()
        .ok_or_else(|| anyhow::anyhow!("SEMFS_STORAGE_BACKEND=pgvector but SEMFS_PG_URL not set"))?;
    let mut store =
        semfs_core::backend::pgvector::PgVectorStore::connect(&url, container, embedder).await?;
    if let Some(reranker) = build_reranker(env)? {
        store = store.with_reranker(reranker);
    }
    if let Some(llm) = build_llm(env) {
        store = store.with_graph_extractor(Arc::new(llm));
    }
    Ok(store)
}

/// Removes a directory tree when dropped — used to give an EPHEMERAL pglite mount
/// true throwaway semantics. Kept alive inside the store (`push_keepalive`), so
/// the pglite server (also held there, pushed first) shuts down before the dir is
/// wiped at unmount.
#[cfg(feature = "pglite")]
struct DirCleanup(std::path::PathBuf);

#[cfg(feature = "pglite")]
impl Drop for DirCleanup {
    fn drop(&mut self) {
        // Can't propagate from Drop — but a cleanup failure leaves ephemeral state
        // on disk that a later same-path mount could resurrect, so log it loudly
        // rather than swallow it.
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    dir = %self.0.display(),
                    error = %e,
                    "failed to remove ephemeral pglite dir at unmount; stale data may remain on disk"
                );
            }
        }
    }
}

/// Remove a pglite data dir, treating "already gone" as success but FAILING on
/// any other error — a `--clean`/ephemeral cleanup that silently failed would
/// reopen the previous on-disk index, the opposite of the requested semantics.
#[cfg(feature = "pglite")]
fn clear_pglite_dir(data_dir: &std::path::Path, why: &str) -> Result<()> {
    match std::fs::remove_dir_all(data_dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::anyhow!(
            "{why}: failed to clear pglite dir {}: {e}",
            data_dir.display()
        )),
    }
}

/// Build an EMBEDDED pglite store (shipped in-box, no external Postgres). Only
/// compiled with the `pglite` feature.
///
/// Lifecycle:
/// - **persistent** (default): persists at `cache_dir()/pglite/{org_id}/{container}`,
///   so two orgs reusing the SAME tag on one machine get PHYSICALLY separate pglite
///   databases. (The SQLite cache is org-independent — `~/.semfs/<tag>.db` — but
///   pglite retains its per-org dir.) The per-org directory IS the isolation
///   boundary, so the in-DB `container` namespace stays the bare tag (daemon writes
///   and IPC search agree; pglite has no direct grep path to disagree). When
///   `clean` is set, the directory is wiped before reopening (a "clean" remount
///   must not resurrect old vectors).
/// - **ephemeral** (`--ephemeral`): uses a unique throwaway directory under the OS
///   temp dir, off the persistent per-org tree entirely, and registers a cleanup
///   guard so it's removed at unmount — nothing survives the daemon, matching the
///   in-memory metadata cache.
#[cfg(feature = "pglite")]
pub async fn build_pglite_store(
    env: &ResolveEnv,
    org_id: &str,
    container: &str,
    ephemeral: bool,
    clean: bool,
    embedder: Arc<dyn Embedder>,
) -> Result<semfs_core::backend::pgvector::PgVectorStore> {
    use semfs_core::backend::pgvector::PgVectorStore;

    let mut store = if ephemeral {
        // Throwaway dir, unique per daemon process (one daemon per tag per pid),
        // under the OS temp dir — never the persistent per-org cache tree.
        let data_dir = std::env::temp_dir()
            .join("semfs-pglite-ephemeral")
            .join(format!("{container}-pid{}", std::process::id()));
        // Fail closed: a leftover dir we can't clear could resurrect stale data.
        clear_pglite_dir(&data_dir, "ephemeral pglite startup")?;
        let mut store = PgVectorStore::embedded(data_dir.clone(), container, embedder).await?;
        // Wipe the dir when the store drops (pushed AFTER the server, so the
        // server shuts down first).
        store.push_keepalive(Box::new(DirCleanup(data_dir)));
        store
    } else {
        let data_dir = semfs_core::config::cache_dir()
            .join("pglite")
            .join(org_id)
            .join(container);
        if clean {
            // Fail closed: a clean remount that couldn't delete must NOT silently
            // reopen the old index.
            clear_pglite_dir(&data_dir, "--clean")?;
        }
        PgVectorStore::embedded(data_dir, container, embedder).await?
    };

    if let Some(reranker) = build_reranker(env)? {
        store = store.with_reranker(reranker);
    }
    if let Some(llm) = build_llm(env) {
        store = store.with_graph_extractor(Arc::new(llm));
    }
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_local_registry() {
        let e = ResolveEnv::default();
        assert_eq!(choose_embed(&e), EmbedChoice::Local);
        assert_eq!(choose_rerank(&e), RerankChoice::Local);
        // Default storage is local (SQLite) → a local embedder is needed.
        assert_eq!(choose_storage(&e), StorageChoice::Sqlite);
        assert!(choose_storage(&e).is_local());
    }

    fn with_embed(backend: &str) -> ResolveEnv {
        ResolveEnv {
            embed_backend: Some(backend.into()),
            ..Default::default()
        }
    }
    fn with_rerank(backend: &str) -> ResolveEnv {
        ResolveEnv {
            rerank_backend: Some(backend.into()),
            ..Default::default()
        }
    }
    fn with_storage(backend: &str) -> ResolveEnv {
        ResolveEnv {
            storage_backend: Some(backend.into()),
            ..Default::default()
        }
    }

    #[test]
    fn embed_backend_overrides_select_cloud_providers() {
        // `hash` is gone — only real embedders remain.
        assert_eq!(choose_embed(&with_embed("openrouter")), EmbedChoice::CloudOpenRouter);
        assert_eq!(choose_embed(&with_embed("openai")), EmbedChoice::CloudOpenAi);
    }

    #[test]
    fn removed_hash_embedder_is_rejected_with_migration_message() {
        // The removed `hash` token must NOT silently fall back to a local embedder —
        // building one fails fast and names the replacement (SEMFS_STORAGE_BACKEND=cloud).
        let err = build_embedder(&with_embed("hash")).unwrap_err().to_string();
        assert!(err.contains("SEMFS_STORAGE_BACKEND=cloud"), "got: {err}");
        assert!(err.contains("was removed"), "got: {err}");
    }

    #[test]
    fn rerank_backend_overrides() {
        assert_eq!(choose_rerank(&ResolveEnv::default()), RerankChoice::Local);
        assert_eq!(choose_rerank(&with_rerank("none")), RerankChoice::None);
        assert_eq!(choose_rerank(&with_rerank("cohere")), RerankChoice::Cohere);
        assert_eq!(choose_rerank(&with_rerank("relace")), RerankChoice::Relace);
    }

    #[test]
    fn storage_choice_parses_all_backends() {
        assert_eq!(choose_storage(&ResolveEnv::default()), StorageChoice::Sqlite);
        assert_eq!(choose_storage(&with_storage("sqlite")), StorageChoice::Sqlite);
        assert_eq!(choose_storage(&with_storage("pgvector")), StorageChoice::Pgvector);
        assert_eq!(choose_storage(&with_storage("pglite")), StorageChoice::Pglite);
        assert_eq!(choose_storage(&with_storage("cloud")), StorageChoice::Cloud);
        // Unknown token falls back to the historical default.
        assert_eq!(choose_storage(&with_storage("bogus")), StorageChoice::Sqlite);
    }

    #[test]
    fn cloud_is_the_only_non_local_storage() {
        // The routing predicate: every backend builds a local index EXCEPT cloud.
        assert!(StorageChoice::Sqlite.is_local());
        assert!(StorageChoice::Pgvector.is_local());
        assert!(StorageChoice::Pglite.is_local());
        assert!(!StorageChoice::Cloud.is_local());
    }

    #[test]
    fn storage_token_round_trips_through_marker() {
        // The daemon persists `as_str()` in the `.semfs` marker; `grep` recovers it
        // via `storage_choice_from`. Cloud must survive the round-trip so grep routes
        // a cloud mount to the cloud index.
        for c in [
            StorageChoice::Sqlite,
            StorageChoice::Pgvector,
            StorageChoice::Pglite,
            StorageChoice::Cloud,
        ] {
            assert_eq!(storage_choice_from(Some(c.as_str())), c, "round-trip {c:?}");
        }
    }
}
