//! Capability-based backend resolution (Phase 5).
//!
//! Picks the embedder and reranker for each stage from available signals — local
//! model dirs and API keys — rather than a monolithic `--offline` flag. Each
//! stage resolves independently, so "local embedder + cloud reranker" is just
//! what you get when a model dir is present and an OpenRouter key is set.
//!
//! The `choose_*` functions are pure (no I/O) and table-tested; `build_*` turn a
//! choice into a live backend. Both the daemon (indexing) and `grep` (search)
//! resolve through here so they always agree on the embedder (and thus dims).

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use semfs_core::embed::{Embedder, HashEmbedder, LocalEmbedder, OpenAiEmbedder};
use semfs_core::rerank::{CohereReranker, LocalReranker, RelaceReranker, Reranker};

/// Signals the resolver reads: local model dirs + API keys.
#[derive(Debug, Clone)]
pub struct ResolveEnv {
    pub embed_model_dir: Option<String>,
    pub embed_dims: usize,
    pub rerank_model_dir: Option<String>,
    pub openrouter_key: Option<String>,
    pub relace_key: Option<String>,
    pub openai_key: Option<String>,
}

impl ResolveEnv {
    /// Read signals from the process environment.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Self {
            embed_model_dir: var("SEMFS_EMBED_MODEL_DIR"),
            embed_dims: var("SEMFS_EMBED_DIMS")
                .and_then(|s| s.parse().ok())
                .unwrap_or(384),
            rerank_model_dir: var("SEMFS_RERANK_MODEL_DIR"),
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
    Cohere,
    Relace,
    Local,
    None,
}

/// Embedder precedence: a local model dir wins (offline, free), then a cloud key,
/// else the deterministic hash floor (dev only — no real semantics).
pub fn choose_embed(env: &ResolveEnv) -> EmbedChoice {
    if env.embed_model_dir.is_some() {
        EmbedChoice::Local
    } else if env.openai_key.is_some() {
        EmbedChoice::CloudOpenAi
    } else if env.openrouter_key.is_some() {
        EmbedChoice::CloudOpenRouter
    } else {
        EmbedChoice::Hash
    }
}

/// Reranker precedence: a cloud cross-encoder (higher quality) when a key is
/// present, then a local model dir, else skip L5.
pub fn choose_rerank(env: &ResolveEnv) -> RerankChoice {
    if env.openrouter_key.is_some() {
        RerankChoice::Cohere
    } else if env.relace_key.is_some() {
        RerankChoice::Relace
    } else if env.rerank_model_dir.is_some() {
        RerankChoice::Local
    } else {
        RerankChoice::None
    }
}

/// True when a real (non-hash) embedder is configured — i.e. local indexing is
/// worth enabling.
pub fn local_indexing_enabled(env: &ResolveEnv) -> bool {
    choose_embed(env) != EmbedChoice::Hash
}

/// Build the resolved embedder.
pub fn build_embedder(env: &ResolveEnv) -> Result<Arc<dyn Embedder>> {
    Ok(match choose_embed(env) {
        EmbedChoice::Local => {
            let dir = env.embed_model_dir.as_deref().unwrap();
            Arc::new(LocalEmbedder::from_dir(Path::new(dir), env.embed_dims)?)
        }
        EmbedChoice::CloudOpenAi => Arc::new(OpenAiEmbedder::new(
            env.openai_key.clone().unwrap(),
            "https://api.openai.com/v1".to_string(),
            "text-embedding-3-small".to_string(),
            1536,
        )),
        EmbedChoice::CloudOpenRouter => {
            Arc::new(OpenAiEmbedder::openrouter(env.openrouter_key.clone().unwrap()))
        }
        EmbedChoice::Hash => Arc::new(HashEmbedder::new(env.embed_dims)),
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
        RerankChoice::Cohere => Some(Arc::new(CohereReranker::openrouter(
            env.openrouter_key.clone().unwrap(),
        ))),
        RerankChoice::Relace => Some(Arc::new(RelaceReranker::new(env.relace_key.clone().unwrap()))),
        RerankChoice::Local => {
            let dir = env.rerank_model_dir.as_deref().unwrap();
            Some(Arc::new(LocalReranker::from_dir(Path::new(dir))?))
        }
        RerankChoice::None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> ResolveEnv {
        ResolveEnv {
            embed_model_dir: None,
            embed_dims: 384,
            rerank_model_dir: None,
            openrouter_key: None,
            relace_key: None,
            openai_key: None,
        }
    }

    #[test]
    fn local_dir_wins_for_embed() {
        let mut e = env();
        e.embed_model_dir = Some("/m".into());
        e.openai_key = Some("k".into());
        assert_eq!(choose_embed(&e), EmbedChoice::Local);
    }

    #[test]
    fn cloud_embed_when_only_key() {
        let mut e = env();
        e.openrouter_key = Some("k".into());
        assert_eq!(choose_embed(&e), EmbedChoice::CloudOpenRouter);
        e.openai_key = Some("k".into());
        assert_eq!(choose_embed(&e), EmbedChoice::CloudOpenAi); // openai precedence
    }

    #[test]
    fn hash_floor_when_nothing() {
        assert_eq!(choose_embed(&env()), EmbedChoice::Hash);
        assert!(!local_indexing_enabled(&env()));
    }

    #[test]
    fn the_target_config_local_embed_cloud_rerank() {
        // SEMFS_EMBED_MODEL_DIR set + OPENROUTER_API_KEY → local embed, Cohere rerank.
        let mut e = env();
        e.embed_model_dir = Some("/m".into());
        e.openrouter_key = Some("k".into());
        assert_eq!(choose_embed(&e), EmbedChoice::Local);
        assert_eq!(choose_rerank(&e), RerankChoice::Cohere);
        assert!(local_indexing_enabled(&e));
    }

    #[test]
    fn rerank_precedence_and_none() {
        assert_eq!(choose_rerank(&env()), RerankChoice::None);
        let mut e = env();
        e.relace_key = Some("k".into());
        assert_eq!(choose_rerank(&e), RerankChoice::Relace);
        e.openrouter_key = Some("k".into());
        assert_eq!(choose_rerank(&e), RerankChoice::Cohere); // cohere over relace
    }
}
