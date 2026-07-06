//! Integration test: pipes a mock MCP client session into the real `semfs mcp`
//! binary over stdin/stdout and asserts on the JSON-RPC responses, exactly the
//! way Claude Code (as a stdio MCP client) would drive it.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn mock_mcp_session_over_stdio() {
    let exe = env!("CARGO_BIN_EXE_semfs");

    let mut child = Command::new(exe)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn semfs mcp");

    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
    );

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write to child stdin");
    // Dropping stdin (via take() above going out of scope) closes it, so the
    // server sees EOF and exits after the loop drains — same as a client
    // closing the pipe.

    let output = child
        .wait_with_output()
        .expect("failed to wait on semfs mcp");
    assert!(output.status.success(), "semfs mcp exited non-zero");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

    // Exactly two JSON-RPC lines: initialize's result + tools/list's result.
    // The notification (`notifications/initialized`) must NOT produce a line.
    assert_eq!(
        lines.len(),
        2,
        "expected exactly 2 response lines (notification must get no reply), got: {stdout:?}"
    );

    let init_resp: serde_json::Value =
        serde_json::from_str(lines[0]).expect("initialize response must be valid JSON");
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "semfs");
    assert_eq!(init_resp["result"]["protocolVersion"], "2024-11-05");

    let list_resp: serde_json::Value =
        serde_json::from_str(lines[1]).expect("tools/list response must be valid JSON");
    assert_eq!(list_resp["id"], 2);
    let tools = list_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "search");
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["query"]["type"],
        "string"
    );

    // stdout carries ONLY JSON-RPC — every non-empty line must parse as JSON.
    for line in &lines {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "stdout line is not valid JSON (protocol channel corrupted?): {line:?}"
        );
    }
}
