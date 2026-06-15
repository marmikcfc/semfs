//! IPC client — connects to a daemon's unix socket and sends a single
//! JSON request, returns the JSON response.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::protocol::{Request, Response};

/// Why an IPC request didn't yield a `Response`, split so callers can tell a
/// MISSING daemon from a PRESENT-but-failing one. The distinction matters for
/// `grep`: an unreachable daemon means "no mount here, fall back to direct/cloud
/// search", but a daemon that accepted the connection and then failed (read
/// timeout, mid-exchange disconnect, malformed reply) is a real fault that must
/// be surfaced — silently re-resolving a different backend would mask it and, for
/// the daemon-only pglite backend, return stale results.
#[derive(Debug)]
pub enum SendError {
    /// Clear ABSENCE: socket file missing, or connect refused / ENOENT (a stale
    /// socket left by a crashed daemon — nothing listening). Nothing was sent.
    /// Safe to fall back to a directly-resolved backend.
    Unreachable(anyhow::Error),
    /// The daemon was REACHED (or is present but wedged) and the exchange failed:
    /// connect timed out on an existing socket (accept loop stalled / backlog
    /// full), or — after connecting — a write error, response-read timeout, close
    /// without replying, or unparseable reply. A daemon-side fault — surface it,
    /// don't silently fall back to a different backend.
    PostConnect(anyhow::Error),
}

impl SendError {
    /// Unwrap to the underlying error (collapsing the classification).
    pub fn into_inner(self) -> anyhow::Error {
        match self {
            SendError::Unreachable(e) | SendError::PostConnect(e) => e,
        }
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Unreachable(e) => write!(f, "{e}"),
            SendError::PostConnect(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SendError {}

/// Send a single request to the daemon that owns the given tag. Collapses the
/// error classification into a flat `anyhow::Error` — for callers that don't need
/// to distinguish absence from failure (status/sync/unmount). `grep` uses
/// [`send_request_classified`] instead.
pub async fn send_request(tag: &str, req: Request) -> Result<Response> {
    let socket = super::socket_path(tag);
    send_request_to_path(&socket, req).await
}

pub async fn send_request_to_path(socket: &Path, req: Request) -> Result<Response> {
    send_request_classified_to_path(socket, req)
        .await
        .map_err(SendError::into_inner)
}

/// Like [`send_request`] but preserves the [`SendError`] classification so the
/// caller can distinguish a missing daemon from one that failed mid-exchange.
pub async fn send_request_classified(
    tag: &str,
    req: Request,
) -> std::result::Result<Response, SendError> {
    let socket = super::socket_path(tag);
    send_request_classified_to_path(&socket, req).await
}

async fn send_request_classified_to_path(
    socket: &Path,
    req: Request,
) -> std::result::Result<Response, SendError> {
    // --- Pre-send: failure here means NO daemon (Unreachable). ---
    if !socket.exists() {
        return Err(SendError::Unreachable(anyhow::anyhow!(
            "daemon not running (no socket at {})",
            socket.display()
        )));
    }
    let stream = match tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(socket))
        .await
    {
        // Connect did NOT complete in 5s on a socket that EXISTS. A unix-socket
        // connect only blocks like this when a daemon is listening but its accept
        // loop is stalled or the listen backlog is saturated — i.e. the daemon is
        // up but wedged. That's a daemon FAULT, not absence; classifying it as
        // Unreachable would let `grep` silently bypass the live daemon and (for
        // pglite, no direct path) return stale cloud results.
        Err(_elapsed) => {
            return Err(SendError::PostConnect(anyhow::anyhow!(
                "timeout connecting to {} (daemon present but not accepting)",
                socket.display()
            )))
        }
        Ok(Err(e)) => {
            // The socket file existed but the connect errored. Only the classic
            // "no listener" errors are clear ABSENCE (a stale socket left by a
            // crashed daemon): ECONNREFUSED, or ENOENT if it vanished mid-race.
            // Any other error means we reached something that then failed — a
            // transport fault to surface, not silent fallback.
            let kind = e.kind();
            let err = anyhow::Error::new(e).context(format!("connect to {}", socket.display()));
            return Err(match kind {
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound => {
                    SendError::Unreachable(err)
                }
                _ => SendError::PostConnect(err),
            });
        }
        Ok(Ok(s)) => s,
    };

    // --- Post-connect: we're talking to a real daemon now. Any failure from
    //     here on is a daemon-side fault (PostConnect), not absence. ---
    let exchange = async {
        let (reader, mut writer) = stream.into_split();
        let body = serde_json::to_string(&req)?;
        writer.write_all(body.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.shutdown().await?;

        let mut lines = BufReader::new(reader).lines();
        // Must stay ABOVE the daemon's search timeout so the daemon's typed error
        // wins before the client gives up. Default 140s (raised 60s→140s alongside
        // the 50s→120s daemon bump, 2026-06-15); override with
        // SEMFS_GREP_CLIENT_WAIT_SECS. Keep > SEMFS_SEARCH_TIMEOUT_SECS (default 120).
        let client_wait = Duration::from_secs(
            std::env::var("SEMFS_GREP_CLIENT_WAIT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(140),
        );
        let line = tokio::time::timeout(client_wait, lines.next_line())
            .await
            .context("timeout waiting for daemon response")?
            .context("read response line")?
            .context("daemon closed without responding")?;
        Ok::<Response, anyhow::Error>(serde_json::from_str(&line)?)
    };
    exchange.await.map_err(SendError::PostConnect)
}
