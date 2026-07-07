//! Tee the upstream response's byte stream so it still forwards to the client
//! in real time (zero added latency — every chunk is relayed the instant it
//! arrives) while a background task also parses Anthropic's SSE frames for
//! the real `input_tokens`/`output_tokens` the actual API call used. Handles
//! both streaming (`message_start`/`message_delta` events) and non-streaming
//! (one JSON body with a top-level `usage` field) response shapes.

use bytes::Bytes;
use futures::{Stream, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::UnboundedReceiverStream;

#[derive(Debug, Default, Clone, Copy)]
pub struct TokenUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

fn scan_usage(value: &serde_json::Value, out: &mut TokenUsage) {
    // Streaming `message_start`: {"message": {"usage": {"input_tokens": N, "output_tokens": N}}}
    if let Some(u) = value.get("message").and_then(|m| m.get("usage")) {
        if let Some(t) = u.get("input_tokens").and_then(|x| x.as_i64()) {
            out.input_tokens = Some(t);
        }
        if let Some(t) = u.get("output_tokens").and_then(|x| x.as_i64()) {
            out.output_tokens = Some(t);
        }
    }
    // Streaming `message_delta` (cumulative so far) and non-streaming top-level:
    // {"usage": {"output_tokens": N}} / {"usage": {"input_tokens": N, "output_tokens": N}}
    if let Some(u) = value.get("usage") {
        if let Some(t) = u.get("input_tokens").and_then(|x| x.as_i64()) {
            out.input_tokens = Some(t);
        }
        if let Some(t) = u.get("output_tokens").and_then(|x| x.as_i64()) {
            out.output_tokens = Some(t);
        }
    }
}

fn find_newline(buf: &[u8]) -> Option<usize> {
    buf.iter().position(|&b| b == b'\n')
}

/// Wraps an upstream byte stream. Returns a stream to hand to `Body::from_stream`
/// (forwards every chunk immediately, unmodified) plus a receiver that resolves
/// once the stream ends with whatever token usage was found in it.
pub fn tee_for_token_usage<S>(mut source: S) -> (impl Stream<Item = reqwest::Result<Bytes>>, oneshot::Receiver<TokenUsage>)
where
    S: Stream<Item = reqwest::Result<Bytes>> + Unpin + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel();
    let (usage_tx, usage_rx) = oneshot::channel();

    tokio::spawn(async move {
        let mut buf: Vec<u8> = Vec::new();
        let mut usage = TokenUsage::default();
        while let Some(item) = source.next().await {
            if let Ok(bytes) = &item {
                buf.extend_from_slice(bytes);
                while let Some(idx) = find_newline(&buf) {
                    let line: Vec<u8> = buf.drain(..=idx).collect();
                    let text = String::from_utf8_lossy(&line);
                    let text = text.trim();
                    if let Some(json_str) = text.strip_prefix("data:") {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str.trim()) {
                            scan_usage(&v, &mut usage);
                        }
                    }
                }
            }
            // Forward to the client immediately regardless of parse outcome —
            // this tee must never delay or alter what the client receives.
            if tx.send(item).is_err() {
                break; // client disconnected; stop pulling from upstream
            }
        }
        // Non-streaming response: the whole thing may be one JSON doc with no
        // "data:" line prefix at all (leftover in buf since no newline was hit,
        // or hit but wasn't SSE-shaped).
        if usage.input_tokens.is_none() && usage.output_tokens.is_none() && !buf.is_empty() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf) {
                scan_usage(&v, &mut usage);
            }
        }
        let _ = usage_tx.send(usage);
    });

    (UnboundedReceiverStream::new(rx), usage_rx)
}
