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
use semfs_core::embed::{Embedder, EmbeddingModel, HashEmbedder, LocalEmbedder, OpenAiEmbedder};
use semfs_core::rerank::{CohereReranker, LocalReranker, RelaceReranker, Reranker, RerankerModel};

/// The fastembed-rs registry models we standardize on (project goal).
const TEXT_EMBED_MODEL: EmbeddingModel = EmbeddingModel::SnowflakeArcticEmbedS; // 384d
const CODE_EMBED_MODEL: EmbeddingModel = EmbeddingModel::JinaEmbeddingsV2BaseCode; // 768d
const RERANK_MODEL: RerankerModel = RerankerModel::JINARerankerV2BaseMultiligual;
/// The int8-quantized ONNX of the reranker (smaller/faster than the registry's
/// pinned full-precision `onnx/model.onnx`), fetched from the model's HF repo.
const RERANK_ONNX: &str = "onnx/model_int8.onnx";
/// Pinned commit of `jinaai/jina-reranker-v2-base-multilingual` so the int8 ONNX
/// + tokenizer are reproducible (an HF HEAD update can't swap them underneath us).
const RERANK_REV: &str = "9cfeff2df7d40d1b78e75e5e9cebec92a99813c9";
/// Cloud OpenAI embedding fallback dims (text-embedding-3-small) + hash floor dims.
const CLOUD_OPENAI_DIMS: usize = 1536;
const HASH_DIMS: usize = 384;

/// Signals the resolver reads from the environment.
#[derive(Debug, Clone, Default)]
pub struct ResolveEnv {
    /// `SEMFS_EMBED_BACKEND`: `local` (default) | `openai` | `openrouter` | `hash`.
    pub embed_backend: Option<String>,
    /// `SEMFS_RERANK_BACKEND`: `local` (default) | `cohere` | `relace` | `none`.
    pub rerank_backend: Option<String>,
    /// `SEMFS_STORAGE_BACKEND`: `sqlite` (default) | `pgvector`. Where vectors are
    /// stored + searched. `pgvector` requires the binary to be built with the
    /// `pg` feature and `SEMFS_PG_URL` to be set.
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
            rerank_backend: var("SEMFS_RERANK_BACKEND"),
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
    Hash,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RerankChoice {
    Local,
    Cohere,
    Relace,
    None,
}

/// Embedder choice — local fastembed registry by default; cloud/hash opt-in.
pub fn choose_embed(env: &ResolveEnv) -> EmbedChoice {
    match env.embed_backend.as_deref() {
        Some("hash") => EmbedChoice::Hash,
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

/// True when a real (non-hash) embedder is configured — i.e. local indexing is
/// worth enabling.
pub fn local_indexing_enabled(env: &ResolveEnv) -> bool {
    choose_embed(env) != EmbedChoice::Hash
}

/// Build the resolved TEXT embedder.
pub fn build_embedder(env: &ResolveEnv) -> Result<Arc<dyn Embedder>> {
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
        EmbedChoice::Local => Arc::new(LocalEmbedder::from_registry(TEXT_EMBED_MODEL, None)?),
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
        EmbedChoice::Hash => Arc::new(HashEmbedder::new(HASH_DIMS)),
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
        RerankChoice::Cohere => Some(Arc::new(CohereReranker::openrouter(
            env.openrouter_key.clone().ok_or_else(|| {
                anyhow::anyhow!("SEMFS_RERANK_BACKEND=cohere but OPENROUTER_API_KEY not set")
            })?,
        ))),
        RerankChoice::Relace => Some(Arc::new(RelaceReranker::new(
            env.relace_key.clone().ok_or_else(|| {
                anyhow::anyhow!("SEMFS_RERANK_BACKEND=relace but RELACE_API_KEY not set")
            })?,
        ))),
        RerankChoice::None => None,
    })
}

/// Where the local vector index is stored + searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageChoice {
    Sqlite,
    /// External Postgres (you run it), via `SEMFS_PG_URL`.
    Pgvector,
    /// Embedded pglite (shipped in-box), persisting under the cache dir.
    Pglite,
}

impl StorageChoice {
    /// Canonical token persisted in the `.semfs` marker so `grep` can recover the
    /// mounted backend without depending on its own (possibly-drifted) env.
    pub fn as_str(self) -> &'static str {
        match self {
            StorageChoice::Sqlite => "sqlite",
            StorageChoice::Pgvector => "pgvector",
            StorageChoice::Pglite => "pglite",
        }
    }
}

/// Map a backend token (from `SEMFS_STORAGE_BACKEND` or the persisted marker) to a
/// `StorageChoice`. Unknown/absent → SQLite (the default + historical behavior).
pub fn storage_choice_from(s: Option<&str>) -> StorageChoice {
    match s {
        Some("pgvector") | Some("pg") | Some("postgres") => StorageChoice::Pgvector,
        Some("pglite") | Some("embedded") => StorageChoice::Pglite,
        _ => StorageChoice::Sqlite,
    }
}

/// Storage backend selection — SQLite (default); external Postgres or embedded
/// pglite opt-in via `SEMFS_STORAGE_BACKEND`.
pub fn choose_storage(env: &ResolveEnv) -> StorageChoice {
    storage_choice_from(env.storage_backend.as_deref())
}

/// An EXPLICITLY-selected external/embedded backend (pgvector/pglite) is the sole
/// search path for its mount and is meaningless with the `hash` floor embedder
/// (no semantic vectors). If the config pairs such a backend with `hash`, return
/// the backend name so the caller can FAIL the mount with a clear error rather
/// than silently mounting with no usable index (which would degrade search to
/// cloud and omit local writes). `None` = the config is fine.
pub fn explicit_backend_without_embedder(env: &ResolveEnv) -> Option<&'static str> {
    if local_indexing_enabled(env) {
        return None; // a real (non-hash) embedder is configured
    }
    match choose_storage(env) {
        StorageChoice::Pgvector => Some("pgvector"),
        StorageChoice::Pglite => Some("pglite"),
        StorageChoice::Sqlite => None,
    }
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
/// Lifecycle mirrors the SQLite cache:
/// - **persistent** (default): persists at `cache_dir()/pglite/{org_id}/{container}`,
///   matching `cache_db_path`, so two orgs reusing the SAME tag on one machine get
///   PHYSICALLY separate pglite databases. The per-org directory IS the isolation
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
        assert!(local_indexing_enabled(&e));
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

    #[test]
    fn embed_backend_overrides_select_cloud_or_hash() {
        assert_eq!(choose_embed(&with_embed("hash")), EmbedChoice::Hash);
        assert!(!local_indexing_enabled(&with_embed("hash")));
        assert_eq!(choose_embed(&with_embed("openrouter")), EmbedChoice::CloudOpenRouter);
        assert_eq!(choose_embed(&with_embed("openai")), EmbedChoice::CloudOpenAi);
    }

    #[test]
    fn rerank_backend_overrides() {
        assert_eq!(choose_rerank(&ResolveEnv::default()), RerankChoice::Local);
        assert_eq!(choose_rerank(&with_rerank("none")), RerankChoice::None);
        assert_eq!(choose_rerank(&with_rerank("cohere")), RerankChoice::Cohere);
        assert_eq!(choose_rerank(&with_rerank("relace")), RerankChoice::Relace);
    }

    fn with_storage_embed(storage: &str, embed: &str) -> ResolveEnv {
        ResolveEnv {
            storage_backend: Some(storage.into()),
            embed_backend: Some(embed.into()),
            ..Default::default()
        }
    }

    #[test]
    fn explicit_backend_with_hash_is_a_config_error() {
        // pgvector/pglite + hash floor → contradiction, surface the backend name.
        assert_eq!(
            explicit_backend_without_embedder(&with_storage_embed("pgvector", "hash")),
            Some("pgvector")
        );
        assert_eq!(
            explicit_backend_without_embedder(&with_storage_embed("pglite", "hash")),
            Some("pglite")
        );
    }

    #[test]
    fn explicit_backend_with_real_embedder_is_ok() {
        assert_eq!(
            explicit_backend_without_embedder(&with_storage_embed("pgvector", "local")),
            None
        );
        assert_eq!(
            explicit_backend_without_embedder(&with_storage_embed("pglite", "openai")),
            None
        );
    }

    #[test]
    fn sqlite_with_hash_is_not_a_config_error() {
        // Default SQLite + hash is the long-standing "no local index" path, not an
        // error — it stays fail-open.
        assert_eq!(
            explicit_backend_without_embedder(&with_storage_embed("sqlite", "hash")),
            None
        );
        assert_eq!(explicit_backend_without_embedder(&with_embed("hash")), None);
    }
}
