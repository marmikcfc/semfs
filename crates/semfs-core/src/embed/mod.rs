//! Local embedding backends (Phase 3).
//!
//! The [`Embedder`] trait abstracts text → vector so the local SQLite index
//! (Phase 4) can run offline. Impls: the dependency-free [`HashEmbedder`]
//! (deterministic, used for tests), the fastembed-backed [`LocalEmbedder`]
//! (real local semantic quality), and [`OpenAiEmbedder`] (cloud HTTP).

pub mod cloud;
pub mod hash;
pub mod local;

pub use cloud::OpenAiEmbedder;
pub use hash::HashEmbedder;
pub use local::LocalEmbedder;

/// Turns text into fixed-width vectors.
///
/// Synchronous on purpose: embedding is CPU-bound and the fastembed-backed impl
/// is blocking, so async callers wrap calls in `spawn_blocking` rather than the
/// trait pretending to be async.
pub trait Embedder: Send + Sync + std::fmt::Debug {
    /// Embed a batch of texts. Each output vector has [`Embedder::dimensions`] elements.
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Output vector width — determines the vec0 table's `float[N]`.
    fn dimensions(&self) -> usize;

    /// Stable identity of the model + vector space, persisted with a local index
    /// so a reader (`grep`) can refuse to search vectors produced by a DIFFERENT
    /// model that merely happens to share the same width (which would silently
    /// corrupt relevance). The default is dimension-only; real embedders override
    /// it with their model identity.
    fn identity(&self) -> String {
        format!("embedder:{}", self.dimensions())
    }
}

/// Cosine similarity helper, shared by tests and the salience/rank steps.
#[cfg(test)]
pub(crate) fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}
