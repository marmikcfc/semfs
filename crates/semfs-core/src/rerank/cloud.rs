//! Cloud HTTP rerankers (L5). Blocking via `ureq` so they satisfy the sync
//! [`Reranker`] trait and run inside async search without nesting tokio.
//!
//! - [`CohereReranker`] — Cohere `/rerank` schema (works via OpenRouter:
//!   `cohere/rerank-v3.5`). Generic (query, documents) → relevance scores.
//! - [`RelaceReranker`] — Relace `/v2/code/rank`, a code-tuned reranker keyed on
//!   filename; we use the document index as the filename.

use serde::{Deserialize, Serialize};

use super::Reranker;

// ── Cohere schema (OpenRouter) ───────────────────────────────────────────────

/// Cohere-schema reranker. Defaults target OpenRouter's `/rerank`.
pub struct CohereReranker {
    api_key: String,
    base_url: String,
    model: String,
}

impl CohereReranker {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
        }
    }

    /// OpenRouter convenience: `cohere/rerank-v3.5`.
    pub fn openrouter(api_key: String) -> Self {
        Self::new(
            api_key,
            "https://openrouter.ai/api/v1".to_string(),
            "cohere/rerank-v3.5".to_string(),
        )
    }
}

impl std::fmt::Debug for CohereReranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CohereReranker")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct CohereRequest<'a> {
    model: &'a str,
    query: &'a str,
    documents: &'a [String],
}

#[derive(Deserialize)]
struct CohereResponse {
    results: Vec<CohereResult>,
}

#[derive(Deserialize)]
struct CohereResult {
    index: usize,
    relevance_score: f32,
}

impl Reranker for CohereReranker {
    fn rerank(&self, query: &str, docs: &[String]) -> anyhow::Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(vec![]);
        }
        let body = CohereRequest {
            model: &self.model,
            query,
            documents: docs,
        };
        let resp: CohereResponse = ureq::post(&format!("{}/rerank", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("rerank request failed: {e}"))?
            .into_json()
            .map_err(|e| anyhow::anyhow!("decode rerank response: {e}"))?;
        let mut scores = vec![0f32; docs.len()];
        for r in resp.results {
            if r.index < scores.len() {
                scores[r.index] = r.relevance_score;
            }
        }
        Ok(scores)
    }
}

// ── Relace code reranker ─────────────────────────────────────────────────────

/// Relace `/v2/code/rank`. Code-tuned; keyed on filename, so we pass the
/// document index as the filename and map scores back by position.
pub struct RelaceReranker {
    api_key: String,
    base_url: String,
    token_limit: usize,
}

impl RelaceReranker {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://ranker.endpoint.relace.run".to_string(),
            token_limit: 100_000,
        }
    }
}

impl std::fmt::Debug for RelaceReranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelaceReranker")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct RelaceRequest {
    query: String,
    codebase: Vec<RelaceDoc>,
    token_limit: usize,
}

#[derive(Serialize)]
struct RelaceDoc {
    filename: String,
    content: String,
}

#[derive(Deserialize)]
struct RelaceResponse {
    results: Vec<RelaceResult>,
}

#[derive(Deserialize)]
struct RelaceResult {
    filename: String,
    score: f32,
}

impl Reranker for RelaceReranker {
    fn rerank(&self, query: &str, docs: &[String]) -> anyhow::Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(vec![]);
        }
        let codebase = docs
            .iter()
            .enumerate()
            .map(|(i, d)| RelaceDoc {
                filename: i.to_string(),
                content: d.clone(),
            })
            .collect();
        let body = RelaceRequest {
            query: query.to_string(),
            codebase,
            token_limit: self.token_limit,
        };
        let resp: RelaceResponse = ureq::post(&format!("{}/v2/code/rank", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("relace rerank request failed: {e}"))?
            .into_json()
            .map_err(|e| anyhow::anyhow!("decode relace response: {e}"))?;
        let mut scores = vec![0f32; docs.len()];
        for r in resp.results {
            if let Ok(i) = r.filename.parse::<usize>() {
                if i < scores.len() {
                    scores[i] = r.score;
                }
            }
        }
        Ok(scores)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs() -> Vec<String> {
        vec![
            "To reset your password, click 'forgot password' and follow the email link.".to_string(),
            "Bananas are a good source of potassium and dietary fiber.".to_string(),
        ]
    }

    /// Gated live test: OPENROUTER_API_KEY. The on-topic doc must outscore the
    /// unrelated one.
    #[test]
    fn cohere_openrouter_scores_relevant_above_irrelevant() {
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        };
        let scores = CohereReranker::openrouter(key)
            .rerank("how do I reset my account password", &docs())
            .unwrap();
        assert_eq!(scores.len(), 2);
        assert!(scores[0] > scores[1], "password {} vs banana {}", scores[0], scores[1]);
    }

    /// Gated live test: RELACE_API_KEY.
    #[test]
    fn relace_scores_relevant_above_irrelevant() {
        let Ok(key) = std::env::var("RELACE_API_KEY") else {
            eprintln!("skipping: RELACE_API_KEY not set");
            return;
        };
        let scores = RelaceReranker::new(key)
            .rerank("how do I reset my account password", &docs())
            .unwrap();
        assert_eq!(scores.len(), 2);
        assert!(scores[0] > scores[1], "password {} vs banana {}", scores[0], scores[1]);
    }
}
