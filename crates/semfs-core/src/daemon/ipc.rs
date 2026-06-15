//! IPC server. Runs inside the daemon as a tokio task.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{watch, Notify};

use super::protocol::{Request, Response};
use crate::cache::CacheFs;

/// Server-side bound on a single IPC search. Must stay below the client's 60s
/// response timeout (see `daemon::client::send_request`) so the daemon returns a
/// typed error BEFORE the client gives up — otherwise a timed-out search would
/// keep holding the single backend connection (pgvector/pglite) in the dark.
///
/// Raised 25s → 50s (2026-06-05): under the post-mount indexing burst a search
/// blocks on the single `Mutex<Connection>` (cache::Db) behind the indexer's
/// write txns; 25s was too tight and the first searches failed-over to an
/// empty cloud result, so the agent abandoned semantic search and brute-forced
/// the FS (24 tool calls / ~4x tokens — see ticket `explore-agent-search-behavior`
/// and `rcas/2026-06-05-agent-search-token-blowup-turn-multiplication.md`). This
/// is a HEADROOM mitigation, not the throughput fix (dedicated read connection —
/// see ticket `search-throughput-readpath-isolation`).
/// Default 120s (raised 50s→120s, 2026-06-15): under heavy post-mount Mutex
/// contention 50s still timed out → cloud fallback → agent retry-storm
/// (rcas/2026-06-15-grep-timeout-cloud-fallback-panic.md). Override with
/// SEMFS_SEARCH_TIMEOUT_SECS (no rebuild needed). Must stay BELOW the client's
/// wait (SEMFS_GREP_CLIENT_WAIT_SECS, default 140s).
fn search_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("SEMFS_SEARCH_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120),
    )
}

/// State the IPC handler reads to answer requests.
#[allow(missing_debug_implementations)] // CacheFs doesn't implement Debug in full
pub struct IpcState {
    pub tag: String,
    pub mount_path: String,
    pub fs: Arc<CacheFs>,
    /// The daemon's local semantic index — the SOLE owner of the backend
    /// connection. `grep` searches through here via IPC instead of opening its
    /// own connection. `None` when local indexing is disabled (hash embedder).
    pub index: Option<Arc<dyn crate::backend::SemanticIndex>>,
    pub started_at: Instant,
    pub pull_enabled: bool,
    /// Storage backend this daemon mounted with (`sqlite`/`pgvector`/`pglite`),
    /// surfaced in `Status` so a client can learn the authoritative backend.
    pub backend: String,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    pub org_name: Option<String>,
    /// L7 entity-graph extraction queue, if a graph extractor is attached. Its
    /// depth (queued + in-flight) is surfaced in `Status` so a client/warm can
    /// wait for the background graph worker to fully drain before unmounting —
    /// store/file size is NOT a reliable drain signal (it's dominated by the
    /// already-written vectors). `None` → no graph work.
    pub graph_queue: Option<Arc<crate::cache::GraphQueue>>,
    /// Fired when an `Unmount` request arrives — daemon main loop awaits this
    /// and treats it the same as SIGTERM.
    pub shutdown_notify: Arc<Notify>,
}

/// Bind the IPC socket SYNCHRONOUSLY (clearing any stale socket first). Kept
/// separate from [`serve`] so the daemon can confirm bind success — and surface a
/// bind failure via `?` — BEFORE it publishes mount state (pid file, `.semfs`
/// marker, `ready`). Binding inside the spawned `serve` task would only log a bind
/// failure, leaving a marker advertising a control plane that never came up.
/// `tokio::net::UnixListener::bind` is synchronous, so this needs no runtime.
pub fn bind(socket_path: &std::path::Path) -> anyhow::Result<UnixListener> {
    // Clean any leftover socket from a crashed prior run.
    let _ = std::fs::remove_file(socket_path);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(socket_path)?;
    tracing::info!(socket = %socket_path.display(), "IPC socket ready");
    Ok(listener)
}

/// Accept connections on a PRE-BOUND listener (see [`bind`]), dispatching one
/// request per connection. Exits when the shutdown watch channel flips true.
pub async fn serve(
    state: Arc<IpcState>,
    listener: UnixListener,
    socket_path: std::path::PathBuf,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() { break; }
            }
            res = listener.accept() => {
                match res {
                    Ok((stream, _addr)) => {
                        let s = state.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_conn(stream, s).await {
                                tracing::debug!(error = %e, "ipc handler error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ipc accept failed");
                    }
                }
            }
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

async fn handle_conn(stream: UnixStream, state: Arc<IpcState>) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let Some(line) = lines.next_line().await? else {
        return Ok(());
    };

    let resp = match serde_json::from_str::<Request>(&line) {
        Ok(req) => dispatch(req, &state).await,
        Err(e) => Response::Error {
            message: format!("invalid request: {e}"),
        },
    };

    let body = serde_json::to_string(&resp)?;
    writer.write_all(body.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.shutdown().await?;
    Ok(())
}

async fn dispatch(req: Request, state: &IpcState) -> Response {
    match req {
        Request::Ping => Response::Pong,
        Request::Status => Response::Status {
            tag: state.tag.clone(),
            mount_path: state.mount_path.clone(),
            pid: std::process::id(),
            uptime_secs: state.started_at.elapsed().as_secs(),
            queue_len: state.fs.push_queue_len(),
            pull_enabled: state.pull_enabled,
            user_id: state.user_id.clone(),
            user_name: state.user_name.clone(),
            org_name: state.org_name.clone(),
            backend: Some(state.backend.clone()),
            graph_queue_depth: state.graph_queue.as_ref().map(|q| q.depth()),
            unindexed_files: Some(state.fs.unindexed_count()),
        },
        Request::Sync => {
            let pulled = crate::sync::pull::delta_pull(&state.fs).await.unwrap_or(0);
            // Wait briefly for push queue to drain before responding, so
            // the caller gets a more useful "pushed_pending" number.
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline && state.fs.push_queue_len() > 0 {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Response::SyncDone {
                pulled,
                pushed_pending: state.fs.push_queue_len(),
            }
        }
        Request::Unmount => {
            state.shutdown_notify.notify_waiters();
            Response::UnmountAck
        }
        Request::Search { query, filepath } => match &state.index {
            // Bound the search so a slow/stuck query can't pin the daemon forever.
            // For pgvector/pglite the index serializes through ONE mutex-guarded
            // connection; an unbounded search would hold it past the client's 30s
            // give-up, blocking every later search AND local indexing. Timing out
            // here drops the search future, releasing the connection guard, and
            // returns a typed error before the client deadline (so the client sees
            // a real failure, not a silent fall-through to a stale backend).
            Some(index) => match tokio::time::timeout(
                search_timeout(),
                index.search(&query, filepath.as_deref()),
            )
            .await
            {
                Ok(Ok(hits)) => Response::SearchHits {
                    hits,
                    searchable: true,
                    backend: Some(state.backend.clone()),
                },
                // Genuine search fault — carry the backend so the client can apply
                // the right fail-closed policy from THIS one response (no separate
                // Status RPC to race).
                Ok(Err(e)) => Response::SearchError {
                    message: format!("search failed: {e}"),
                    backend: Some(state.backend.clone()),
                },
                Err(_) => Response::SearchError {
                    message: format!("search timed out after {}s", search_timeout().as_secs()),
                    backend: Some(state.backend.clone()),
                },
            },
            // No local index (hash embedder / indexing disabled) — tell the
            // client so it can fall back to cloud rather than treat empty as final.
            None => Response::SearchHits {
                hits: Vec::new(),
                searchable: false,
                backend: Some(state.backend.clone()),
            },
        },
    }
}
