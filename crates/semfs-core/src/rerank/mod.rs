//! Reranking backends (L5). After hybrid RRF produces candidate hits, a
//! cross-encoder rescoring step reorders them by judging (query, document)
//! pairs jointly — sharper relevance than the bi-encoder embeddings alone.
//!
//! Like [`crate::embed`], this is a pluggable seam: [`LocalReranker`] (fastembed
//! cross-encoder, offline) today; cloud rerankers (OpenRouter/Relace) plug in
//! behind the same trait next.

pub mod cloud;
pub mod local;

pub use cloud::{CohereReranker, RelaceReranker};
pub use local::LocalReranker;

/// Re-exported so callers can name registry rerankers without a direct fastembed dep.
pub use fastembed::RerankerModel;

/// Scores documents against a query. Higher = more relevant.
pub trait Reranker: Send + Sync + std::fmt::Debug {
    /// Relevance score per document, **aligned to the input order** (so callers
    /// can zip scores back onto their candidates and re-sort).
    fn rerank(&self, query: &str, docs: &[String]) -> anyhow::Result<Vec<f32>>;
}
