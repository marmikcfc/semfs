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
    },
    SyncDone {
        pulled: usize,
        pushed_pending: usize,
    },
    UnmountAck,
    /// Ranked hits from a `Search` request. `searchable=false` means the daemon
    /// has no usable local index (so the client should fall back to cloud).
    SearchHits {
        hits: Vec<crate::backend::SearchHit>,
        searchable: bool,
    },
    Error {
        message: String,
    },
}
