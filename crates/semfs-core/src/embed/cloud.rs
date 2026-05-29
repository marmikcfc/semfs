//! Cloud HTTP embedders (Phase 3). OpenAI-compatible `/embeddings` — works with
//! OpenAI directly or any compatible gateway (OpenRouter, etc.). Blocking via
//! `ureq` so it satisfies the synchronous [`Embedder`] trait and runs inside the
//! async search path without nesting a tokio runtime.

use serde::{Deserialize, Serialize};

use super::Embedder;

/// An OpenAI-compatible embeddings client. The same wire format serves OpenAI,
/// OpenRouter, and other gateways — only `base_url`/`model`/`dims` differ.
pub struct OpenAiEmbedder {
    api_key: String,
    base_url: String,
    model: String,
    dims: usize,
}

impl OpenAiEmbedder {
    pub fn new(api_key: String, base_url: String, model: String, dims: usize) -> Self {
        Self {
            api_key,
            base_url,
            model,
            dims,
        }
    }

    /// OpenRouter convenience: `text-embedding-3-small` (1536d) via the
    /// OpenAI-compatible gateway.
    pub fn openrouter(api_key: String) -> Self {
        Self::new(
            api_key,
            "https://openrouter.ai/api/v1".to_string(),
            "openai/text-embedding-3-small".to_string(),
            1536,
        )
    }
}

// Manual Debug so the API key never lands in logs.
impl std::fmt::Debug for OpenAiEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiEmbedder")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("dims", &self.dims)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a [String],
    dimensions: usize,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedDatum>,
}

#[derive(Deserialize)]
struct EmbedDatum {
    embedding: Vec<f32>,
}

impl Embedder for OpenAiEmbedder {
    fn dimensions(&self) -> usize {
        self.dims
    }

    fn identity(&self) -> String {
        format!("openai:{}:{}", self.model, self.dims)
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let url = format!("{}/embeddings", self.base_url);
        let body = EmbedRequest {
            model: &self.model,
            input: texts,
            dimensions: self.dims,
        };
        let resp: EmbedResponse = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("embeddings request failed: {e}"))?
            .into_json()
            .map_err(|e| anyhow::anyhow!("decode embeddings response: {e}"))?;
        Ok(resp.data.into_iter().map(|d| d.embedding).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gated live test: needs OPENROUTER_API_KEY in the env. Skips otherwise.
    #[test]
    fn openrouter_embedder_returns_expected_dims() {
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        };
        let e = OpenAiEmbedder::openrouter(key);
        let out = e.embed(&["authentication and login".to_string()]).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 1536);
    }
}
