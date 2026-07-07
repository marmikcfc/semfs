//! Prose compression for the latest message in a request. Code safety rule:
//! if the text contains a fenced ``` code block anywhere, skip compression for
//! that message entirely rather than try to carve out and reassemble the code
//! and prose separately — extractive compression can corrupt code, and
//! reassembling compressed-prose-around-untouched-code correctly for
//! arbitrarily interleaved content is not worth the risk for what's usually a
//! single short user message. Routing still runs on the original text either way.

use std::sync::LazyLock;

use regex::Regex;
use tracing::warn;

use crate::config::BackendConfig;
use crate::optimize;

static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)```.*?```").unwrap());

pub fn contains_code(text: &str) -> bool {
    FENCE_RE.is_match(text)
}

/// BYO compressor path — calls a `/optimize`-speaking service with `route:
/// false` (a compression-only backend doesn't need to answer a routing
/// question). Returns `(compressed_text_or_none, what_happened)`.
pub async fn compress_byo(
    client: &reqwest::Client,
    cfg: &BackendConfig,
    text: &str,
    threshold: f64,
) -> (Option<String>, &'static str) {
    if text.trim().is_empty() {
        return (None, "empty");
    }
    let Some(base_url) = cfg.base_url.as_deref().filter(|u| !u.is_empty()) else {
        return (None, "unconfigured");
    };
    match optimize::call_optimize(client, base_url, cfg.api_key.as_deref(), text, true, false, threshold).await {
        Some(r) => match r.compressed_context {
            Some(c) if !c.trim().is_empty() => (Some(c), "backend"),
            _ => {
                warn!("compressor returned an empty reply -> passthrough");
                (None, "backend-empty-reply")
            }
        },
        None => (None, "backend-error"),
    }
}
