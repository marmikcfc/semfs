//! tokopt-gateway — local proxy behind ANTHROPIC_BASE_URL.
//!
//! Every request to a `/messages` path (the Anthropic Messages API) is
//! intercepted: the latest message's text is compressed and a real Claude
//! model is chosen for it, then the (possibly rewritten) request is forwarded
//! to the real API and the response streamed back untouched — the client sees
//! zero added latency from the streaming itself. In the background, the real
//! response's token usage is read off the stream and compared against a
//! concurrent `count_tokens` call on the pre-compression original, so usage
//! numbers are real token counts, not char-based estimates. After every
//! response, that usage event is written to a local SQLite db and (fire-and-
//! forget) reported to the backend.
//!
//! No subagent hooks, no PreToolUse/PostToolUse wiring — this is the only door,
//! deliberately: a hook can never see the main turn's own API call, only a
//! proxy sitting where the CLI's traffic already flows.

mod compressor;
mod config;
mod configweb;
mod optimize;
mod pricing;
mod router;
mod tokens;
mod usage;

use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{
        header::{CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING},
        HeaderMap, HeaderName, HeaderValue, StatusCode,
    },
    response::{Html, IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use bytes::Bytes;
use clap::Parser;
use serde_json::json;
use tokens::TokenUsage;
use tracing::{error, info};

const SKIP_HEADERS: &[HeaderName] = &[HOST, CONTENT_LENGTH, TRANSFER_ENCODING, CONNECTION];

#[derive(Parser, Debug)]
#[command(name = "tokopt-gateway")]
struct Args {
    #[arg(long, env = "TOKOPT_GATEWAY_PORT", default_value_t = 8787)]
    port: u16,
    #[arg(long, env = "TOKOPT_UPSTREAM", default_value = "https://api.anthropic.com")]
    upstream: String,
}

struct Config {
    upstream: String,
    client: reqwest::Client,
    kompress_threshold: f64,
    usage: usage::UsageLog,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let kompress_threshold = std::env::var("KOMPRESS_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.5);
    let cfg = Arc::new(Config {
        upstream: args.upstream.trim_end_matches('/').to_string(),
        client: reqwest::Client::builder().build()?,
        kompress_threshold,
        usage: usage::UsageLog::open_default()?,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/usage", get(usage_summary))
        .route("/config", get(config_page).post(config_post))
        .route("/config.json", get(config_get_json))
        .fallback(any(smart_proxy))
        .with_state(cfg.clone());

    let addr = format!("127.0.0.1:{}", args.port);
    // Resolve once here purely to log the startup picture; the live config is
    // re-resolved per request so the config page takes effect without a restart.
    let r0 = config::resolve_router_config();
    let c0 = config::resolve_compressor_config();
    info!(
        upstream = %cfg.upstream, %addr,
        router_model = %r0.model, router_source = %r0.source,
        compressor_model = %c0.model, compressor_source = %c0.source,
        "tokopt-gateway starting"
    );
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health(State(cfg): State<Arc<Config>>) -> impl IntoResponse {
    let router = config::resolve_router_config();
    let compressor = config::resolve_compressor_config();
    Json(json!({
        "status": "ok",
        "mode": "gateway",
        "upstream": cfg.upstream,
        "router": {
            "model": router.model,
            "base_url": router.base_url,
            "source": router.source,
            "key_loaded": router.api_key.is_some(),
            "reachable": !router.is_unreachable(),
        },
        "compressor": {
            "model": compressor.model,
            "base_url": compressor.base_url,
            "source": compressor.source,
            "key_loaded": compressor.api_key.is_some(),
            "reachable": !compressor.is_unreachable(),
        },
        "kompress_threshold": cfg.kompress_threshold,
    }))
}

async fn usage_summary(State(cfg): State<Arc<Config>>) -> impl IntoResponse {
    Json(cfg.usage.summary())
}

/// The built-in config page (HTML form). Reachable at http://127.0.0.1:8787/config.
async fn config_page() -> impl IntoResponse {
    Html(configweb::PAGE)
}

/// Current saved config, for the page to prefill. The api key is never returned
/// in the clear — just a boolean flag so the form can show "set" vs empty.
async fn config_get_json() -> impl IntoResponse {
    let c = config::load_app_config();
    Json(json!({
        "backend_url": c.backend_url,
        "backend_api_key": c.backend_api_key.as_deref().map(|_| true).unwrap_or(false),
        "router_model": c.router_model,
        "compressor_model": c.compressor_model,
    }))
}

/// Save config from the page. A missing `backend_api_key` key leaves the stored
/// one untouched (so the form can omit it to keep the existing key); an empty
/// string clears it. Requires `Content-Type: application/json`, which browsers
/// can't send cross-origin without a preflight we don't answer — basic CSRF
/// guard for a loopback service that can rewrite where prompts are sent.
async fn config_post(headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    // CSRF guard: require an application/json content-type. A "simple" cross-
    // origin POST (the kind a stray webpage can make to loopback without our
    // cooperation) can't set this without triggering a preflight we never
    // answer — so this blocks a malicious page from silently rewriting where
    // prompts are sent, while our own same-origin fetch (which sets it) works.
    let ct_ok = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
        .unwrap_or(false);
    if !ct_ok {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Content-Type must be application/json").into_response();
    }
    let incoming: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid json: {e}")).into_response(),
    };
    let mut cfg = config::load_app_config();
    let take = |v: &serde_json::Value, k: &str| -> Option<Option<String>> {
        // None = key absent (leave unchanged); Some(None) = null/empty (clear);
        // Some(Some(s)) = set to s.
        v.get(k).map(|x| x.as_str().map(str::to_string).filter(|s| !s.is_empty()))
    };
    if let Some(val) = take(&incoming, "backend_url") { cfg.backend_url = val; }
    if let Some(val) = take(&incoming, "backend_api_key") { cfg.backend_api_key = val; }
    if let Some(val) = take(&incoming, "router_model") { cfg.router_model = val; }
    if let Some(val) = take(&incoming, "compressor_model") { cfg.compressor_model = val; }
    match config::save_app_config(&cfg) {
        Ok(()) => (StatusCode::OK, Json(json!({"saved": true}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("save failed: {e}")).into_response(),
    }
}

/// Which shape the last message's `content` was in, so we can write a
/// compressed replacement back in the same shape.
enum ContentShape {
    Str,
    ArrayText,
}

/// Only returns text when the last message is plain text or an array of ONLY
/// text blocks. Anything else (tool_use, tool_result, image, mixed) -> None,
/// so that turn passes through completely untouched rather than risk mangling
/// structured content this proxy doesn't need to understand.
fn extract_last_message_text(body: &serde_json::Value) -> Option<(String, ContentShape)> {
    let messages = body.get("messages")?.as_array()?;
    let last = messages.last()?;
    match last.get("content")? {
        serde_json::Value::String(s) => Some((s.clone(), ContentShape::Str)),
        serde_json::Value::Array(blocks) => {
            if blocks.is_empty() {
                return None;
            }
            let mut texts = Vec::with_capacity(blocks.len());
            for b in blocks {
                if b.get("type").and_then(|t| t.as_str()) != Some("text") {
                    return None;
                }
                texts.push(b.get("text")?.as_str()?.to_string());
            }
            Some((texts.join("\n\n"), ContentShape::ArrayText))
        }
        _ => None,
    }
}

fn set_last_message_text(body: &mut serde_json::Value, text: &str, shape: &ContentShape) {
    let Some(last) = body
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
        .and_then(|arr| arr.last_mut())
    else {
        return;
    };
    last["content"] = match shape {
        ContentShape::Str => serde_json::Value::String(text.to_string()),
        ContentShape::ArrayText => json!([{"type": "text", "text": text}]),
    };
}

/// Real input tokens for the ORIGINAL (pre-compression) request, via
/// Anthropic's `count_tokens` endpoint — fired concurrently with the real
/// forwarded call, purely for measurement, never on the response critical
/// path. `None` on any failure (older API version without the endpoint,
/// network error, etc.) — callers must treat that as "not measured", not zero.
async fn count_tokens(
    client: &reqwest::Client,
    upstream: &str,
    headers: &HeaderMap,
    body: &serde_json::Value,
) -> Option<i64> {
    let url = format!("{upstream}/v1/messages/count_tokens");
    let mut req = client.post(&url).json(body);
    for (name, value) in headers.iter() {
        if SKIP_HEADERS.contains(name) {
            continue;
        }
        req = req.header(name.clone(), value.clone());
    }
    let resp = req.timeout(std::time::Duration::from_secs(15)).send().await.ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("input_tokens").and_then(|t| t.as_i64())
}

async fn smart_proxy(State(cfg): State<Arc<Config>>, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let body_bytes = match to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "failed reading request body");
            return (StatusCode::BAD_GATEWAY, "failed to read request body").into_response();
        }
    };

    if !parts.uri.path().ends_with("/messages") || !usage::is_enabled() {
        return forward(&cfg, &parts, body_bytes).await.0;
    }

    let Ok(mut json_body) = serde_json::from_slice::<serde_json::Value>(&body_bytes) else {
        return forward(&cfg, &parts, body_bytes).await.0;
    };

    // Snapshot BEFORE any mutation — this is what count_tokens measures against,
    // and original_model is what Claude Code actually asked for.
    let original_json_for_counting = json_body.clone();
    let original_model = json_body.get("model").and_then(|m| m.as_str()).map(String::from);

    // Resolve backends fresh here (not at startup) so a save in the config page
    // takes effect on the very next request — no gateway restart.
    let router_cfg = config::resolve_router_config();
    let compressor_cfg = config::resolve_compressor_config();

    let start = Instant::now();
    let mut relevant_model: Option<String> = None;
    let mut tier: Option<String> = None;
    let mut chars_in: i64 = 0;
    let mut chars_out: i64 = 0;

    if let Some((text, shape)) = extract_last_message_text(&json_body) {
        chars_in = text.chars().count() as i64;
        let code_present = compressor::contains_code(&text);

        // "byo" changes call shape (own provider); config/dev-localhost/
        // default-hosted are all `/optimize`-style, so gate on `!= "byo"`.
        let need_compress_via_optimize = compressor_cfg.source != "byo" && !code_present;
        let need_route_via_optimize = router_cfg.source != "byo";

        let optimize_result = if need_compress_via_optimize || need_route_via_optimize {
            let base_url = router_cfg.base_url.clone().or_else(|| compressor_cfg.base_url.clone());
            let key = router_cfg.api_key.clone().or_else(|| compressor_cfg.api_key.clone());
            match base_url.filter(|u| !u.is_empty()) {
                Some(url) => {
                    optimize::call_optimize(
                        &cfg.client, &url, key.as_deref(), &text,
                        need_compress_via_optimize, need_route_via_optimize, cfg.kompress_threshold,
                    ).await
                }
                None => None,
            }
        } else {
            None
        };

        // Routing: BYO calls its own real provider; default-hosted reads the
        // combined call above; either way, a local heuristic is the final
        // fail-open backstop so a turn is never silently left unrouted.
        if router_cfg.source == "byo" {
            let r = router::route_task_byo(&cfg.client, &router_cfg, &text).await;
            tracing::debug!(tier = %r.tier, model = %r.model, decided_by = r.source, "byo route");
            relevant_model = Some(r.model);
            tier = Some(r.tier);
        } else if let Some(model) = optimize_result.as_ref().and_then(|r| r.relevant_model.clone()) {
            relevant_model = Some(model);
            tier = optimize_result.as_ref().and_then(|r| r.tier.clone());
        }
        if relevant_model.is_none() {
            let r = router::route_task_heuristic(&text);
            relevant_model = Some(r.model);
            tier = Some(r.tier);
        }

        // Compression: never touches code. BYO calls its own service; default
        // -hosted reads the combined call above.
        let compressed_text = if code_present {
            None
        } else if compressor_cfg.source == "byo" {
            compressor::compress_byo(&cfg.client, &compressor_cfg, &text, cfg.kompress_threshold).await.0
        } else {
            optimize_result.and_then(|r| r.compressed_context)
        };

        if let Some(model) = &relevant_model {
            json_body["model"] = serde_json::Value::String(model.clone());
        }
        if let Some(ct) = &compressed_text {
            chars_out = ct.chars().count() as i64;
            set_last_message_text(&mut json_body, ct, &shape);
        } else {
            chars_out = chars_in;
        }
    }

    let decision_latency_ms = start.elapsed().as_millis() as i64;
    let model = relevant_model.clone().unwrap_or_else(|| {
        json_body.get("model").and_then(|m| m.as_str()).unwrap_or("unknown").to_string()
    });

    // Fire count_tokens on the ORIGINAL text concurrently — pure measurement,
    // never awaited before the real response is forwarded.
    let count_tokens_fut = {
        let client = cfg.client.clone();
        let upstream = cfg.upstream.clone();
        let headers = parts.headers.clone();
        let body = original_json_for_counting;
        tokio::spawn(async move { count_tokens(&client, &upstream, &headers, &body).await })
    };

    let new_body = match serde_json::to_vec(&json_body) {
        Ok(b) => Bytes::from(b),
        Err(_) => body_bytes,
    };
    let (response, token_usage_rx) = forward(&cfg, &parts, new_body).await;

    // Finalize usage in the background — never delays the response above,
    // which has already been returned to axum for streaming to the client.
    let cfg2 = cfg.clone();
    let sync_target = router_cfg.base_url.clone().or_else(|| compressor_cfg.base_url.clone());
    let router_source = router_cfg.source.to_string();
    let compressor_source = compressor_cfg.source.to_string();
    tokio::spawn(async move {
        let tokens_before_compression = count_tokens_fut.await.ok().flatten();
        let token_usage: TokenUsage = token_usage_rx.await.unwrap_or_default();

        let cost_original_usd = original_model.as_deref().and_then(|m| {
            pricing::cost_usd(m, token_usage.input_tokens.unwrap_or(0), token_usage.output_tokens.unwrap_or(0))
        });
        let cost_routed_usd = pricing::cost_usd(
            &model, token_usage.input_tokens.unwrap_or(0), token_usage.output_tokens.unwrap_or(0),
        );

        let event = usage::UsageEvent {
            model: model.clone(),
            original_model: original_model.clone(),
            tier,
            router_source,
            compressor_source,
            chars_in,
            chars_out,
            tokens_before_compression,
            input_tokens: token_usage.input_tokens,
            output_tokens: token_usage.output_tokens,
            cost_original_usd,
            cost_routed_usd,
            latency_ms: decision_latency_ms,
            status: "ok".to_string(),
        };
        cfg2.usage.record(&event);
        usage::sync_to_backend(cfg2.client.clone(), sync_target, json!({
            "endpoint": "gateway_local", "model": event.model, "status": event.status,
            "latency_ms": event.latency_ms, "chars_in": event.chars_in, "chars_out": event.chars_out,
        }));
    });

    response
}

/// Forward (possibly rewritten) body bytes to the real upstream, streaming
/// the response back untouched. Returns the response AND a receiver that
/// resolves with real token usage once the stream finishes (see `tokens.rs`)
/// — reading it never delays what the client receives.
async fn forward(
    cfg: &Config,
    parts: &axum::http::request::Parts,
    body_bytes: Bytes,
) -> (Response, tokio::sync::oneshot::Receiver<TokenUsage>) {
    let path_and_query = parts.uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let target = format!("{}{}", cfg.upstream, path_and_query);

    let empty_usage = || {
        let (tx, rx) = tokio::sync::oneshot::channel();
        drop(tx);
        rx
    };

    let mut req_builder = cfg.client.request(parts.method.clone(), &target);
    for (name, value) in parts.headers.iter() {
        if SKIP_HEADERS.contains(name) {
            continue;
        }
        req_builder = req_builder.header(name.clone(), value.clone());
    }
    req_builder = req_builder.body(body_bytes);

    let upstream_resp = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, %target, "upstream request failed");
            return (
                (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
                empty_usage(),
            );
        }
    };

    let status = upstream_resp.status();
    let mut resp_builder = Response::builder().status(status);
    for (name, value) in upstream_resp.headers().iter() {
        let value: &HeaderValue = value;
        if SKIP_HEADERS.contains(name) {
            continue;
        }
        resp_builder = resp_builder.header(name.clone(), value.clone());
    }

    let (tracked_stream, usage_rx) = tokens::tee_for_token_usage(upstream_resp.bytes_stream());
    match resp_builder.body(Body::from_stream(tracked_stream)) {
        Ok(resp) => (resp, usage_rx),
        Err(e) => {
            error!(error = %e, "failed to build response");
            (
                (StatusCode::BAD_GATEWAY, "failed to build response").into_response(),
                empty_usage(),
            )
        }
    }
}
