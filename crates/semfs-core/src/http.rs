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
