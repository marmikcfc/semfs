//! `semfs mcp` — minimal stdio MCP (Model Context Protocol) server that exposes
//! semantic search (`semfs grep`) as an MCP tool.
//!
//! Speaks newline-delimited JSON-RPC 2.0 over stdin/stdout (the MCP "stdio"
//! transport): one JSON object per line in, one JSON object per line out.
//! **stdout is the protocol channel** — all logging goes to stderr via
//! `eprintln!` (never `println!`, and never `tracing`, whose default writer
//! in this binary is stdout — see `main.rs::init_tracing`).
//!
//! Why this exists: a prompt hint telling Claude Code to shell out to `semfs
//! grep` is silently ignored. An MCP tool is not — Claude Code is configured
//! with this as a stdio MCP server (`command: semfs, args: ["mcp"]`) so its
//! search tool calls route to semfs's semantic index instead of ripgrep.

use anyhow::Result;
use clap::Args as ClapArgs;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::process::Command;

const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Directory `semfs grep` runs in (its `.semfs` marker locates the mounted
    /// container). Overrides `SEMFS_MCP_MOUNT`; default `/semfs`.
    #[arg(long)]
    pub mount: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let mount = args
        .mount
        .clone()
        .or_else(|| std::env::var("SEMFS_MCP_MOUNT").ok())
        .unwrap_or_else(|| "/semfs".to_string());

    eprintln!("semfs mcp: stdio MCP server starting (mount={mount})");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("semfs mcp: stdin read error: {e}");
                break;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("semfs mcp: failed to parse JSON-RPC line ({e}): {trimmed}");
                continue;
            }
        };

        if let Some(resp) = handle_request(&req, &mount).await {
            let text = serde_json::to_string(&resp)?;
            writeln!(stdout, "{text}")?;
            stdout.flush()?;
        }
    }

    eprintln!("semfs mcp: stdin closed, exiting");
    Ok(())
}

/// Dispatch one parsed JSON-RPC request/notification. `None` means "send
/// nothing" — required for notifications (no `id`), per the JSON-RPC/MCP spec.
async fn handle_request(req: &Value, mount: &str) -> Option<Value> {
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    if method == "notifications/initialized" {
        return None;
    }

    let Some(id) = req.get("id").cloned() else {
        eprintln!("semfs mcp: ignoring notification: {method}");
        return None;
    };

    match method.as_str() {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "semfs", "version": env!("CARGO_PKG_VERSION") }
            }
        })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": [tool_definition()] }
        })),
        "tools/call" => Some(handle_tool_call(id, req, mount)),
        other => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method not found: {other}") }
        })),
    }
}

fn tool_definition() -> Value {
    json!({
        "name": "search",
        "description": "Semantic code search over the mounted semfs knowledge-graph seed. Pass 2-4 key terms or a natural-language question; returns the most relevant file:line ranges and code chunks, ranked by meaning. Prefer this over text/grep search.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "2-4 key terms or a natural-language question"
                }
            },
            "required": ["query"]
        }
    })
}

/// `tools/call` handler. MCP reports tool failures IN-BAND (`isError: true`
/// in the result), never as a JSON-RPC error — so every path here returns
/// `Ok`-shaped JSON-RPC with the outcome carried in `result`.
fn handle_tool_call(id: Value, req: &Value, mount: &str) -> Value {
    let params = req.get("params");
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");

    if name != "search" {
        return tool_error(id, format!("unknown tool: {name:?}"));
    }

    let query = params
        .and_then(|p| p.get("arguments"))
        .and_then(|a| a.get("query"))
        .and_then(Value::as_str);

    let Some(query) = query else {
        return tool_error(id, "missing required argument: query".to_string());
    };

    match run_grep(query, mount) {
        Ok(text) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "content": [{ "type": "text", "text": text }], "isError": false }
        }),
        Err(e) => tool_error(id, e.to_string()),
    }
}

fn tool_error(id: Value, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "content": [{ "type": "text", "text": message }], "isError": true }
    })
}

/// Run `semfs grep <query>` as a SUBPROCESS of the current executable, rooted
/// at `mount` (whose `.semfs` marker is how `grep` locates the container).
/// Subprocess isolation is deliberate: `grep::run` prints results to stdout,
/// which — called in-process here — would corrupt the MCP JSON-RPC channel.
/// Environment is inherited (the container sets `SEMFS_EMBED_MODEL` etc.).
fn run_grep(query: &str, mount: &str) -> Result<String> {
    let exe = std::env::current_exe()?;
    let output = Command::new(exe)
        .arg("grep")
        .arg(query)
        .current_dir(mount)
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(format!(
            "{stdout}\n[semfs grep exited with {}; stderr:]\n{stderr}",
            output.status
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_returns_server_info() {
        let req = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        let resp = handle_request(&req, "/semfs").await.unwrap();
        assert_eq!(resp["result"]["serverInfo"]["name"], "semfs");
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["id"], 1);
    }

    #[tokio::test]
    async fn initialized_notification_gets_no_reply() {
        let req = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(handle_request(&req, "/semfs").await.is_none());
    }

    #[tokio::test]
    async fn unknown_notification_gets_no_reply() {
        let req = json!({"jsonrpc":"2.0","method":"notifications/whatever"});
        assert!(handle_request(&req, "/semfs").await.is_none());
    }

    #[tokio::test]
    async fn tools_list_advertises_search() {
        let req = json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}});
        let resp = handle_request(&req, "/semfs").await.unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "search");
        assert_eq!(
            tools[0]["inputSchema"]["properties"]["query"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_is_in_band_error() {
        let req = json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"not-a-real-tool","arguments":{}}
        });
        let resp = handle_request(&req, "/semfs").await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp.get("error").is_none());
    }

    #[tokio::test]
    async fn tools_call_missing_query_is_in_band_error() {
        let req = json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"search","arguments":{}}
        });
        let resp = handle_request(&req, "/semfs").await.unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[tokio::test]
    async fn unknown_method_is_json_rpc_error() {
        let req = json!({"jsonrpc":"2.0","id":5,"method":"totally/bogus","params":{}});
        let resp = handle_request(&req, "/semfs").await.unwrap();
        assert_eq!(resp["error"]["code"], -32601);
        assert!(resp.get("result").is_none());
    }

    /// Live-mount smoke test — needs a real seed under SEMFS_MCP_MOUNT/.semfs.
    /// Ignored by default; run with `cargo test -- --ignored` against a mount.
    #[tokio::test]
    #[ignore]
    async fn tools_call_search_runs_real_grep() {
        let mount = std::env::var("SEMFS_MCP_MOUNT").unwrap_or_else(|_| "/semfs".to_string());
        let req = json!({
            "jsonrpc":"2.0","id":6,"method":"tools/call",
            "params":{"name":"search","arguments":{"query":"authentication flow"}}
        });
        let resp = handle_request(&req, &mount).await.unwrap();
        assert_eq!(resp["result"]["isError"], false);
    }
}
