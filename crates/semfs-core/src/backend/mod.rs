//! Backend search abstraction. The `SemanticIndex` trait is the seam that lets
//! `grep` query either the cloud (`CloudIndex`) or a future local store without
//! knowing which.

pub(crate) mod cloud;
pub mod chunk;
pub mod community;
pub mod graph;
pub mod graph_ast;
#[cfg(feature = "pg")]
pub mod pgvector;
pub mod rank;
pub mod sqlite_vec;
pub use cloud::CloudIndex;
pub use sqlite_vec::SqliteVecStore;

use async_trait::async_trait;

/// One search result, backend-agnostic. Mirrors the fields `grep` renders.
/// Serializable so it can cross the daemon IPC boundary (grep-over-IPC).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchHit {
    pub filepath: Option<String>,
    pub memory: Option<String>,
    pub chunk: Option<String>,
    pub similarity: f64,
    /// Cross-turn dedup (SEM-19): set by the daemon when this file's content was
    /// already returned with content earlier this session. When `Some(turn)`, the
    /// daemon has stripped `memory`/`chunk` and `grep` renders a pointer line
    /// ("already in your context (turn N)") instead of re-sending the excerpt.
    /// `#[serde(default)]` → absent (`None`) on the daemonless/cloud path.
    #[serde(default)]
    pub seen_at_turn: Option<u64>,
}

/// The semantic-search substrate behind `grep`. Any backend that answers a semantic query.
#[async_trait]
pub trait SemanticIndex: Send + Sync {
    /// Search by meaning. `filepath` optionally scopes to a prefix.
    async fn search(&self, query: &str, filepath: Option<&str>)
        -> anyhow::Result<Vec<SearchHit>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeIndex;

    #[async_trait]
    impl SemanticIndex for FakeIndex {
        async fn search(&self, query: &str, _filepath: Option<&str>)
            -> anyhow::Result<Vec<SearchHit>> {
            Ok(vec![SearchHit {
                filepath: Some("/notes/a.md".into()),
                memory: None,
                chunk: Some(format!("matched: {query}")),
                similarity: 0.9,
                seen_at_turn: None,
            }])
        }
    }

    #[tokio::test]
    async fn trait_object_is_usable_without_cloud() {
        let idx: std::sync::Arc<dyn SemanticIndex> = std::sync::Arc::new(FakeIndex);
        let hits = idx.search("hello", None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].filepath.as_deref(), Some("/notes/a.md"));
        assert_eq!(hits[0].chunk.as_deref(), Some("matched: hello"));
    }
}
