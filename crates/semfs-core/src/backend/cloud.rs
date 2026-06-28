//! Cloud `SemanticIndex` — adapts the Supermemory `ApiClient`.

use std::sync::Arc;

use async_trait::async_trait;

use super::{SearchHit, SemanticIndex};
use crate::api::ApiClient;

/// Convert a Supermemory `SearchResult` DTO into a backend-agnostic `SearchHit`.
fn to_hit(r: crate::api::dto::SearchResult) -> SearchHit {
    SearchHit {
        filepath: r.filepath,
        memory: r.memory,
        chunk: r.chunk,
        similarity: r.similarity,
        seen_at_turn: None,
    }
}

/// Wraps an `ApiClient` and exposes it as a `SemanticIndex`.
#[derive(Debug)]
pub struct CloudIndex {
    api: Arc<ApiClient>,
}

impl CloudIndex {
    pub fn new(api: Arc<ApiClient>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl SemanticIndex for CloudIndex {
    async fn search(&self, query: &str, filepath: Option<&str>) -> anyhow::Result<Vec<SearchHit>> {
        let resp = self.api.search(query, filepath).await?;
        Ok(resp.results.into_iter().map(to_hit).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::dto::{SearchResp, SearchResult};

    // Pure mapping test — no network. Verifies the DTO -> SearchHit translation.
    fn map(resp: SearchResp) -> Vec<SearchHit> {
        resp.results.into_iter().map(to_hit).collect()
    }

    #[test]
    fn maps_search_result_fields() {
        let resp = SearchResp {
            results: vec![SearchResult {
                memory: Some("m".into()),
                chunk: Some("c".into()),
                similarity: 0.42,
                filepath: Some("/x.md".into()),
                ..Default::default()
            }],
            timing: None,
            total: None,
        };
        let hits = map(resp);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].filepath.as_deref(), Some("/x.md"));
        assert_eq!(hits[0].memory.as_deref(), Some("m"));
        assert_eq!(hits[0].chunk.as_deref(), Some("c"));
        assert!((hits[0].similarity - 0.42).abs() < 1e-9);
    }
}
