//! IPC wire protocol — JSON line-based.
//!
//! One request per connection. Simple enough to drive with `nc` for
//! debugging:
//!
//! ```text
//! $ echo '{"cmd":"ping"}' | nc -U <sockets/tag.sock>
//! {"type":"pong"}
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    Sync,
    Unmount,
    /// Semantic search over the daemon's local index. This is how `grep` reaches
    /// the index without opening its own DB connection — essential for embedded
    /// single-connection backends (pglite) where the daemon owns the connection.
    Search {
        query: String,
        #[serde(default)]
        filepath: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Status {
        tag: String,
        mount_path: String,
        pid: u32,
        uptime_secs: u64,
        queue_len: usize,
        pull_enabled: bool,
        #[serde(default)]
        user_id: Option<String>,
        #[serde(default)]
        user_name: Option<String>,
        #[serde(default)]
        org_name: Option<String>,
        /// Storage backend the daemon actually mounted with (`sqlite`/`pgvector`/
        /// `pglite`). Lets a client (`grep`) learn the AUTHORITATIVE backend from
        /// the live daemon when its local marker doesn't carry it (e.g. an
        /// explicit `--tag` run from outside the mount), so it can apply the right
        /// fail-closed policy. `#[serde(default)]` → `None` from older daemons.
        #[serde(default)]
        backend: Option<String>,
    },
    SyncDone {
        pulled: usize,
        pushed_pending: usize,
    },
    UnmountAck,
    /// Ranked hits from a `Search` request. `searchable=false` means the daemon
    /// has no usable local index (so the client should fall back to cloud).
    /// `backend` is the daemon's AUTHORITATIVE storage backend, carried in the
    /// SAME response so the client's fallback policy needs no second RPC.
    SearchHits {
        hits: Vec<crate::backend::SearchHit>,
        searchable: bool,
        #[serde(default)]
        backend: Option<String>,
    },
    /// A `Search` that REACHED the daemon's index but FAILED (backend fault,
    /// embedder outage, timeout). Distinct from the generic `Error` (which also
    /// covers unparseable requests from an older daemon) so the client can tell a
    /// genuine search fault from version skew — and it carries the authoritative
    /// `backend` so a daemon-only backend (pglite) stays fail-closed without a
    /// separate Status lookup that could flake independently.
    SearchError {
        message: String,
        #[serde(default)]
        backend: Option<String>,
    },
    Error {
        message: String,
    },
}
