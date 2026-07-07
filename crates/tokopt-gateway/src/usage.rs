//! Local usage tracking. Lives at `${TOKOPT_HOME:-~/.tokopt}/usage.db` — the
//! same state directory `plugin/hooks/_tokopt.py` already uses, so the Python
//! CLI commands (`/tokopt-status`, `/tokopt-metrics`) and this binary agree on
//! where state lives without needing to talk to each other about it.
//!
//! Recording is deferred: the response streams back to the client immediately
//! (see `tokens.rs`), and once the real token counts are known — from the
//! actual response plus a concurrent `count_tokens` call on the pre-compression
//! original — one row is written here AND (fire-and-forget) POSTed to the
//! backend's `/usage` ingestion endpoint, so aggregate stats aren't limited to
//! requests that happen to hit the default-hosted backend directly.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;
use serde_json::json;
use tracing::warn;

pub struct UsageEvent {
    pub model: String,
    /// What Claude Code originally asked for, before routing overrode it.
    /// `None` if routing didn't run (e.g. tokopt disabled) or model was unchanged.
    pub original_model: Option<String>,
    pub tier: Option<String>,
    pub router_source: String,
    pub compressor_source: String,
    pub chars_in: i64,
    pub chars_out: i64,
    /// Real input tokens for the pre-compression original message, from a
    /// concurrent `count_tokens` call — `None` if that call failed/skipped.
    pub tokens_before_compression: Option<i64>,
    /// Real input tokens the actual (compressed, routed) request used, read
    /// off the real response — `None` if parsing the response didn't find one.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    /// USD estimate: what `original_model` would have cost for this call's
    /// real token counts, vs. what `model` (the routed one) actually cost.
    /// `None` when either model isn't in the pricing table.
    pub cost_original_usd: Option<f64>,
    pub cost_routed_usd: Option<f64>,
    pub latency_ms: i64,
    pub status: String,
}

pub struct UsageLog {
    conn: Mutex<Connection>,
}

pub fn tokopt_home() -> PathBuf {
    if let Ok(dir) = std::env::var("TOKOPT_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".tokopt")
}

/// Read `${TOKOPT_HOME}/state.json`'s `{"enabled": bool}`, same file
/// `plugin/hooks/_tokopt.py`'s `/tokopt-on`/`/tokopt-off` commands write.
/// Defaults to true (enabled) on any error — a missing/corrupt state file
/// must never silently disable routing and compression.
pub fn is_enabled() -> bool {
    let path = tokopt_home().join("state.json");
    match std::fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str::<serde_json::Value>(&contents)
            .ok()
            .and_then(|v| v.get("enabled").and_then(|e| e.as_bool()))
            .unwrap_or(true),
        Err(_) => true,
    }
}

impl UsageLog {
    pub fn open_default() -> anyhow::Result<Self> {
        let dir = tokopt_home();
        std::fs::create_dir_all(&dir)?;
        Self::open(&dir.join("usage.db"))
    }

    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                model TEXT NOT NULL,
                original_model TEXT,
                tier TEXT,
                router_source TEXT NOT NULL,
                compressor_source TEXT NOT NULL,
                chars_in INTEGER NOT NULL,
                chars_out INTEGER NOT NULL,
                tokens_before_compression INTEGER,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cost_original_usd REAL,
                cost_routed_usd REAL,
                latency_ms INTEGER NOT NULL,
                status TEXT NOT NULL
             );",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Fail-open — a usage-log write must never affect the actual response.
    pub fn record(&self, event: &UsageEvent) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let result = (|| -> rusqlite::Result<()> {
            let conn = self.conn.lock().map_err(|_| {
                rusqlite::Error::InvalidParameterName("usage log mutex poisoned".into())
            })?;
            conn.execute(
                "INSERT INTO usage (ts, model, original_model, tier, router_source, compressor_source, \
                 chars_in, chars_out, tokens_before_compression, input_tokens, output_tokens, \
                 cost_original_usd, cost_routed_usd, latency_ms, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                rusqlite::params![
                    now, event.model, event.original_model, event.tier, event.router_source,
                    event.compressor_source, event.chars_in, event.chars_out,
                    event.tokens_before_compression, event.input_tokens, event.output_tokens,
                    event.cost_original_usd, event.cost_routed_usd, event.latency_ms, event.status,
                ],
            )?;
            Ok(())
        })();
        if let Err(e) = result {
            warn!(error = %e, "usage log write failed (non-fatal)");
        }
    }

    pub fn summary(&self) -> serde_json::Value {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return json!({ "error": "usage log unavailable" }),
        };
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM usage", [], |r| r.get(0)).unwrap_or(0);

        // Real-token compression savings: only counted where we actually got a
        // count_tokens comparison AND a real input_tokens back from the response.
        let tokens_removed: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(tokens_before_compression - input_tokens), 0) FROM usage \
                 WHERE status = 'ok' AND tokens_before_compression IS NOT NULL AND input_tokens IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let chars_saved: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(chars_in - chars_out), 0) FROM usage WHERE status = 'ok'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let cost_saved_usd: f64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_original_usd - cost_routed_usd), 0.0) FROM usage \
                 WHERE status = 'ok' AND cost_original_usd IS NOT NULL AND cost_routed_usd IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0.0);
        let routed_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM usage WHERE original_model IS NOT NULL AND original_model != model",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let mut by_model_stmt = match conn.prepare(
            "SELECT model, COUNT(*) FROM usage GROUP BY model ORDER BY COUNT(*) DESC",
        ) {
            Ok(s) => s,
            Err(_) => return json!({ "total_requests": total }),
        };
        let by_model: Vec<(String, i64)> = by_model_stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default();

        json!({
            "total_requests": total,
            "requests_rerouted_to_a_different_model": routed_count,
            "chars_saved_by_compression": chars_saved,
            "tokens_removed_by_compression": tokens_removed,
            "cost_saved_usd_from_routing": cost_saved_usd,
            "cost_note": "estimated from TOKOPT_PRICING_JSON or built-in example prices — verify against real current Anthropic pricing",
            "by_model": by_model,
        })
    }
}

/// Fire-and-forget: report this event to the backend so central aggregates
/// cover BYO users too, not just requests that hit the backend directly.
/// Never awaited by the response path — spawned and forgotten.
pub fn sync_to_backend(client: reqwest::Client, backend_url: Option<String>, event_json: serde_json::Value) {
    let Some(base_url) = backend_url.filter(|u| !u.is_empty()) else {
        return;
    };
    tokio::spawn(async move {
        let url = format!("{}/usage", base_url.trim_end_matches('/'));
        if let Err(e) = client
            .post(&url)
            .json(&event_json)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            warn!(error = %e, %url, "usage sync to backend failed (non-fatal)");
        }
    });
}
