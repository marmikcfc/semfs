//! Client for the ONE non-OpenAI-standard contract in this system: `/optimize`.
//!
//! Used two ways:
//!   - the combined default-hosted call (compression + routing in one request)
//!   - a BYO compressor pointed at another `/optimize`-speaking instance
//!     (`route: false`, since a compression-only service doesn't need to answer
//!     a routing question)
//!
//! BYO *router* does NOT use this — it must speak real OpenAI-compatible
//! `/chat/completions` to interop with an arbitrary third-party provider, which
//! is what `router::llm_route` is for.

use serde_json::json;
use tracing::warn;

pub struct OptimizeResult {
    pub compressed_context: Option<String>,
    pub relevant_model: Option<String>,
    pub tier: Option<String>,
}

pub async fn call_optimize(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Option<&str>,
    text: &str,
    want_compress: bool,
    want_route: bool,
    threshold: f64,
) -> Option<OptimizeResult> {
    if base_url.is_empty() {
        return None;
    }
    let body = json!({
        "text": text,
        "compress": want_compress,
        "route": want_route,
        "threshold": threshold,
    });
    let mut req = client
        .post(format!("{}/optimize", base_url.trim_end_matches('/')))
        .json(&body);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = match req.timeout(std::time::Duration::from_secs(25)).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "optimize request failed");
            return None;
        }
    };
    let value: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "optimize response not JSON");
            return None;
        }
    };
    Some(OptimizeResult {
        compressed_context: value.get("compressed_context").and_then(|v| v.as_str()).map(String::from),
        relevant_model: value.get("relevant_model").and_then(|v| v.as_str()).map(String::from),
        tier: value.get("stats").and_then(|s| s.get("tier")).and_then(|v| v.as_str()).map(String::from),
    })
}
