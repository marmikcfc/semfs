//! Model tier routing for BYO mode: a cheap OpenAI-compatible chat-completion
//! call picks a tier from `routes.json`, mapped to a real `claude-*` model id.
//! A keyword heuristic is the fail-open backstop. (Default-hosted routing goes
//! through `optimize::call_optimize` instead — the backend owns its own copy
//! of this same tier table so it can return a ready-to-use model id directly.)

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::warn;

use crate::config::BackendConfig;

/// Default route table, embedded at compile time so the binary is
/// self-contained. Override with `ROUTES_FILE=<path to a routes.json>`.
const DEFAULT_ROUTES_JSON: &str = include_str!("../routes.json");

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Route {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub model: String,
}

#[derive(Debug, Deserialize)]
struct RoutesFile {
    routing_preferences: Vec<Route>,
}

pub fn load_routes() -> Vec<Route> {
    if let Ok(path) = std::env::var("ROUTES_FILE") {
        match std::fs::read_to_string(&path).and_then(|s| {
            serde_json::from_str::<RoutesFile>(&s).map_err(std::io::Error::other)
        }) {
            Ok(rf) => return rf.routing_preferences,
            Err(e) => warn!(error = %e, path, "ROUTES_FILE unreadable, using embedded default"),
        }
    }
    serde_json::from_str::<RoutesFile>(DEFAULT_ROUTES_JSON)
        .expect("embedded routes.json is valid")
        .routing_preferences
}

/// Fail-open backstop: keyword match, same heuristic used since v1.
pub fn heuristic_route(task: &str) -> &'static str {
    let t = task.to_lowercase();
    const STRONG: &[&str] = &[
        "architecture", "refactor", "debug", "root cause", "design", "multi-file",
        "concurrency", "race condition", "migrate", "security", "optimize", "why",
        "investigate",
    ];
    const CHEAP: &[&str] = &[
        "rename", "typo", "format", "comment", "docstring", "small", "simple",
        "list ", "read ", "print", "add a test", "boilerplate",
    ];
    if STRONG.iter().any(|w| t.contains(w)) {
        return "strong";
    }
    if CHEAP.iter().any(|w| t.contains(w)) || t.len() < 80 {
        return "cheap";
    }
    "mid"
}

pub struct RouteResult {
    pub tier: String,
    pub model: String,
    /// "llm" | "heuristic" — which mechanism actually picked the route.
    pub source: &'static str,
}

/// Route via an OpenAI-compatible `/chat/completions` endpoint using Structured
/// Outputs (an enum-locked JSON schema, so the model can only reply with one of
/// the real tier names — no regex-parsing a free-form reply). Returns `None`
/// on any failure so the caller falls back to the heuristic; never panics,
/// never blocks longer than the request timeout. BYO only — this is the shape
/// needed to interop with an arbitrary real third-party provider.
async fn llm_route_byo(
    client: &reqwest::Client,
    cfg: &BackendConfig,
    task: &str,
    routes: &[Route],
) -> Option<String> {
    let base_url = cfg.base_url.as_deref()?;
    if base_url.is_empty() {
        return None;
    }
    let names: Vec<&str> = routes.iter().map(|r| r.name.as_str()).collect();
    let spec = routes
        .iter()
        .map(|r| format!("- {}: {}", r.name, r.description))
        .collect::<Vec<_>>()
        .join("\n");
    let sys_prompt = "You are a routing orchestrator. Given the tiers and a task, pick the single best tier.";
    let usr_prompt = format!("Tiers:\n{spec}\n\nTask:\n{task}");
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": { "tier": { "type": "string", "enum": names } },
        "required": ["tier"],
    });
    let body = json!({
        "model": cfg.model,
        "max_tokens": 20,
        "temperature": 0.0,
        "messages": [
            {"role": "system", "content": sys_prompt},
            {"role": "user", "content": usr_prompt},
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "tier", "strict": true, "schema": schema},
        },
    });

    let mut req = client
        .post(format!("{}/chat/completions", base_url.trim_end_matches('/')))
        .json(&body);
    if let Some(key) = &cfg.api_key {
        req = req.bearer_auth(key);
    }

    let resp = match req
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "router request failed -> heuristic");
            return None;
        }
    };
    let value: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "router response not JSON -> heuristic");
            return None;
        }
    };
    let content = value["choices"][0]["message"]["content"].as_str()?;
    let parsed: serde_json::Value = serde_json::from_str(content).ok()?;
    let tier = parsed["tier"].as_str()?.to_string();
    if names.contains(&tier.as_str()) {
        Some(tier)
    } else {
        None
    }
}

/// BYO-only entry point (default-hosted routing goes through `optimize::call_optimize`).
pub async fn route_task_byo(client: &reqwest::Client, cfg: &BackendConfig, task: &str) -> RouteResult {
    let routes = load_routes();
    let (tier, source) = match llm_route_byo(client, cfg, task, &routes).await {
        Some(tier) => (tier, "llm"),
        None => (heuristic_route(task).to_string(), "heuristic"),
    };
    let model = routes
        .iter()
        .find(|r| r.name == tier)
        .map(|r| r.model.clone())
        .unwrap_or_else(|| {
            routes.iter().find(|r| r.name == "strong").map(|r| r.model.clone())
                .unwrap_or_else(|| "claude-opus-4-8".to_string())
        });
    RouteResult { tier, model, source }
}

/// Local (no-network) fallback used when routing can't reach anything at all —
/// heuristic tier, mapped through the same embedded table.
pub fn route_task_heuristic(task: &str) -> RouteResult {
    let routes = load_routes();
    let tier = heuristic_route(task).to_string();
    let model = routes
        .iter()
        .find(|r| r.name == tier)
        .map(|r| r.model.clone())
        .unwrap_or_else(|| "claude-opus-4-8".to_string());
    RouteResult { tier, model, source: "heuristic" }
}
