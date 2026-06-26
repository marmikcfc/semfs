//! Shared blocking HTTP agent with bounded timeouts.
//!
//! The cloud embedder/reranker satisfy the SYNCHRONOUS `Embedder`/`Reranker`
//! traits via `ureq`, and the daemon runs them inside `spawn_blocking` on the
//! IPC search path. Tokio cannot abort a started `spawn_blocking` job, so if an
//! HTTP call hangs (server stops responding mid-stream), the blocking task would
//! run forever even after the daemon's 25s search timeout fired — orphaned tasks
//! could pile up and starve the daemon. `ureq`'s default global agent has NO
//! timeouts, so every cloud call MUST go through this bounded agent instead, which
//! caps each call's lifetime and keeps orphaned blocking work finite.

use std::sync::LazyLock;
use std::time::Duration;

/// Process-wide `ureq` agent with connect/read/write timeouts. Cheap to clone
/// (internally reference-counted) and reuses a connection pool across calls.
static TIMEOUT_AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .timeout_write(Duration::from_secs(30))
        .build()
});

/// A blocking HTTP agent whose calls are time-bounded. Use this for ALL cloud
/// embed/rerank/LLM requests so a hung remote can't pin a daemon blocking thread.
pub fn timeout_agent() -> ureq::Agent {
    TIMEOUT_AGENT.clone()
}

/// LLM chat agent with a LONGER read timeout than [`timeout_agent`]. KG
/// entity-extraction generations emit up to several thousand tokens and, under
/// concurrent vLLM batching, routinely take 30-90s — the 30s embed/rerank
/// timeout would abort them mid-stream (the doc is then silently dropped from the
/// KG). Default 240s; `SEMFS_LLM_READ_TIMEOUT` (seconds) overrides. The search
/// path still uses the short [`timeout_agent`], so this never slows search.
static LLM_AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    // Default 240s suits KG extraction (long offline generations). The hot-path
    // grep-compress / rewrite calls want a TIGHT bound so a slow OpenRouter call
    // fails fast → fail-open to the raw excerpt (no retry-storm). Floor 5s so the
    // run can set e.g. SEMFS_LLM_READ_TIMEOUT=12 for compress without touching KG.
    let read = std::env::var("SEMFS_LLM_READ_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n >= 5)
        .unwrap_or(240);
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(read))
        .timeout_write(Duration::from_secs(60))
        .build()
});

/// Blocking HTTP agent for LLM chat calls (long read timeout). See [`LLM_AGENT`].
pub fn llm_agent() -> ureq::Agent {
    LLM_AGENT.clone()
}
