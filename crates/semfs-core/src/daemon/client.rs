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
    /// No daemon to talk to: socket absent, or connect refused/timed out. Nothing
    /// was sent. Safe to fall back to a directly-resolved backend.
    Unreachable(anyhow::Error),
    /// The daemon accepted the connection but the exchange failed afterward (write
    /// error, response-read timeout, closed without replying, unparseable reply).
    /// A daemon-side fault — surface it, don't fall back.
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
        // Connect timed out — treat as absence (daemon not accepting).
        Err(_elapsed) => {
            return Err(SendError::Unreachable(anyhow::anyhow!(
                "timeout connecting to {}",
                socket.display()
            )))
        }
        // Connect refused / ENOENT / stale socket — absence.
        Ok(Err(e)) => {
            return Err(SendError::Unreachable(
                anyhow::Error::new(e)
                    .context(format!("connect to {}", socket.display())),
            ))
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
        let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .context("timeout waiting for daemon response")?
            .context("read response line")?
            .context("daemon closed without responding")?;
        Ok::<Response, anyhow::Error>(serde_json::from_str(&line)?)
    };
    exchange.await.map_err(SendError::PostConnect)
}
