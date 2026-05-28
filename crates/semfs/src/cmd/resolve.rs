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
        RerankChoice::Local => Some(Arc::new(LocalReranker::from_registry(RERANK_MODEL, None)?)),
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
}
