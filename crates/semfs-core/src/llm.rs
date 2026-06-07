//! Minimal OpenRouter chat client for LLM-assisted layers.
//!
//! Used by L4 (query rewrite) and, later, L7 Tier-1 (entity/fact extraction).
//! Blocking via `ureq` so it composes with the sync search path; the API key is
//! never logged.

use serde::{Deserialize, Serialize};

/// OpenAI-compatible chat client (defaults to OpenRouter `gpt-4.1-nano`).
pub struct LlmClient {
    api_key: String,
    base_url: String,
    model: String,
}

impl LlmClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
        }
    }

    /// OpenRouter convenience: a small, cheap, capable model for rewrite/extract.
    pub fn openrouter(api_key: String) -> Self {
        Self::new(
            api_key,
            "https://openrouter.ai/api/v1".to_string(),
            "openai/gpt-4.1-nano".to_string(),
        )
    }

    /// One-shot free-text completion (`system` + `user` → assistant text).
    pub fn complete(&self, system: &str, user: &str) -> anyhow::Result<String> {
        self.chat(system, user, None, 256)
    }

    /// One-shot completion constrained to a JSON Schema via structured outputs
    /// (`response_format: json_schema`, `strict: true`). The provider constrains
    /// decoding to the schema, so the returned string is guaranteed valid JSON of
    /// the given shape — no fences, no prose, no out-of-enum values.
    pub fn complete_structured(
        &self,
        system: &str,
        user: &str,
        schema: serde_json::Value,
    ) -> anyhow::Result<String> {
        self.complete_structured_n(system, user, schema, 512)
    }

    /// Same as [`complete_structured`] but with an explicit output token budget —
    /// graph extraction (entities + typed relations) needs more than the 512
    /// default or the JSON truncates and fails to parse.
    pub fn complete_structured_n(
        &self,
        system: &str,
        user: &str,
        schema: serde_json::Value,
        max_tokens: u32,
    ) -> anyhow::Result<String> {
        let response_format = serde_json::json!({
            "type": "json_schema",
            "json_schema": { "name": "extraction", "strict": true, "schema": schema }
        });
        self.chat(system, user, Some(response_format), max_tokens)
    }

    fn chat(
        &self,
        system: &str,
        user: &str,
        response_format: Option<serde_json::Value>,
        max_tokens: u32,
    ) -> anyhow::Result<String> {
        let body = ChatRequest {
            model: &self.model,
            temperature: 0.0,
            max_tokens,
            messages: vec![
                Message {
                    role: "system",
                    content: system,
                },
                Message {
                    role: "user",
                    content: user,
                },
            ],
            response_format,
        };
        let resp: ChatResponse = crate::http::timeout_agent()
            .post(&format!("{}/chat/completions", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("chat request failed: {e}"))?
            .into_json()
            .map_err(|e| anyhow::anyhow!("decode chat response: {e}"))?;
        resp.choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow::anyhow!("no choices in chat response"))
    }
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    temperature: f32,
    max_tokens: u32,
    messages: Vec<Message<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: String,
}

/// L4: rewrite a search query for better semantic retrieval (expand
/// abbreviations, add synonyms/related terms). Output is the rewritten query
/// only. Callers use this **opt-in** and fail-open to the original on error.
pub fn rewrite_query(client: &LlmClient, query: &str) -> anyhow::Result<String> {
    let system = "You rewrite a user's search query to maximize semantic document retrieval over a \
        possibly MULTILINGUAL corpus (documents may be in Chinese, English, or other languages). \
        Expand abbreviations and add closely-related terms and synonyms. CRITICALLY: if the query \
        is in one language but documents may be in another, also append the key search terms \
        TRANSLATED into the other likely document language(s) — especially Chinese (e.g. \
        'conversion rate'→'转化率', 'transaction amount'→'成交金额', 'best-selling'→'畅销'). \
        Keep it to one concise line. \
        Output ONLY the rewritten query — no quotes, no preamble, no explanation.";
    let out = client.complete(system, query)?;
    let out = out.trim().trim_matches('"').trim().to_string();
    if out.is_empty() {
        anyhow::bail!("empty rewrite");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gated live test: OPENROUTER_API_KEY. Rewrite returns a non-empty, expanded
    /// query (typically longer than the terse original).
    #[test]
    fn rewrite_query_expands_via_openrouter() {
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("skipping: OPENROUTER_API_KEY not set");
            return;
        };
        let client = LlmClient::openrouter(key);
        let out = rewrite_query(&client, "auth renewal").unwrap();
        assert!(!out.is_empty());
        eprintln!("rewrite: {out}");
    }
}
