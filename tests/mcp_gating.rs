//! Regression tests for the MCP broadcast-tool gating.
//!
//! These tests spawn the real `aleoflow mcp` binary over stdio and inspect
//! the actual `tools/list` JSON-RPC response, rather than testing the arg
//! builders in isolation. They pin the verified behavior of rmcp 3.1.1's
//! `ToolRouter::disable_route`: disabled tools are genuinely absent from
//! the `tools/list` listing (and any call is refused).
//!
//! NOTES:
//! - The tool names must be read from the `tools` array of the
//!   `tools/list` response (id == 2) only. Grepping the raw stdout is NOT a
//!   valid check: the `initialize` response's `serverInfo.instructions`
//!   text also contains the literal broadcast tool names.
//! - The server does not necessarily exit on stdin EOF, so the helper reads
//!   until the `tools/list` response arrives and then terminates the child.

use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Funds-spending tools that must be hidden unless the server is started
/// with `ALEOFLOW_MCP_ALLOW_BROADCAST=true`. Keep in sync with
/// `BROADCAST_TOOLS` in src/mcp.rs — if a tool is renamed there, update it
/// here too, or this test silently stops guarding the real names.
const BROADCAST_TOOLS: [&str; 3] = [
    "aleoflow_deploy_broadcast",
    "aleoflow_execute_broadcast",
    "aleoflow_send_broadcast",
];

/// Dry-run counterparts that must always be listed (positive controls).
const DRY_RUN_TOOLS: [&str; 3] = [
    "aleoflow_deploy_dry_run",
    "aleoflow_execute_dry_run",
    "aleoflow_send_dry_run",
];

/// Give the MCP server ample time to answer; the handshake and listing are
/// purely local, so in practice this returns in well under a second.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Spawn `aleoflow mcp`, perform the initialize + tools/list exchange over
/// stdio, and return the tool names from the `tools/list` response.
///
/// `allow_broadcast`: if `Some(value)`, sets `ALEOFLOW_MCP_ALLOW_BROADCAST`
/// on the child; if `None`, explicitly removes it so the test never depends
/// on the parent environment. Each child gets its own env, so these tests
/// are safe to run in parallel with each other and with the unit tests.
fn request_tools_list(allow_broadcast: Option<&str>) -> Vec<String> {
    let bin = env!("CARGO_BIN_EXE_aleoflow");
    let mut cmd = Command::new(bin);
    cmd.arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("ALEOFLOW_MCP_ALLOW_BROADCAST");
    if let Some(v) = allow_broadcast {
        cmd.env("ALEOFLOW_MCP_ALLOW_BROADCAST", v);
    }

    let mut child = cmd.spawn().expect("failed to spawn aleoflow mcp");

    {
        let stdin = child.stdin.as_mut().expect("mcp stdin not piped");
        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"gating-test","version":"0"}}}"#;
        let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        write!(stdin, "{initialize}\n{list}\n")
            .expect("failed to write requests to aleoflow mcp stdin");
    }
    // IMPORTANT: the owned ChildStdin is still open inside `child.stdin`;
    // we do not rely on EOF. Close it anyway so the child sees EOF too.
    child.stdin.take();

    let mut stdout = child.stdout.take().expect("mcp stdout not piped");
    let mut stderr = child.stderr.take().expect("mcp stderr not piped");
    let (tx_out, rx_out) = mpsc::channel::<String>();
    let (tx_err, rx_err) = mpsc::channel::<String>();
    let reader_out = thread::spawn(move || {
        let mut buf = String::new();
        if stdout.read_to_string(&mut buf).is_ok() {
            for line in buf.lines() {
                if tx_out.send(line.to_string()).is_err() {
                    break;
                }
            }
        }
    });
    let reader_err = thread::spawn(move || {
        let mut buf = String::new();
        if stderr.read_to_string(&mut buf).is_ok() && tx_err.send(buf).is_err() {
            // channel closed; nothing to do
        }
    });

    let deadline = Instant::now() + TIMEOUT;
    let mut names: Vec<String> = Vec::new();
    let mut raw_out: Vec<String> = Vec::new();
    let mut got_response = false;
    while !got_response {
        match rx_out.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                raw_out.push(line.clone());
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue, // skip partial/other lines
                };
                if v.get("id").and_then(Value::as_u64) != Some(2) {
                    continue; // ignore the initialize response and notifications
                }
                // This is the tools/list response: collect tool names and stop.
                let tools = v
                    .get("result")
                    .and_then(|r| r.get("tools"))
                    .and_then(Value::as_array);
                for tool in tools.into_iter().flatten() {
                    if let Some(name) = tool.get("name").and_then(Value::as_str) {
                        names.push(name.to_string());
                    }
                }
                got_response = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let stderr_text = rx_err
                        .recv_timeout(Duration::from_millis(1000))
                        .unwrap_or_default();
                    panic!(
                        "timed out after {}s waiting for aleoflow mcp to answer tools/list.\n\
                         binary: {}\n\
                         raw stdout received:\n{}\n\
                         stderr:\n{}",
                        TIMEOUT.as_secs(),
                        bin,
                        raw_out.join("\n"),
                        stderr_text
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The child closed stdout before answering tools/list (e.g. it
                // crashed on startup). Fail with full diagnostics instead of
                // letting an empty name list fail an assertion downstream.
                let stderr_text = rx_err
                    .recv_timeout(Duration::from_millis(1000))
                    .unwrap_or_default();
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader_out.join();
                let _ = reader_err.join();
                panic!(
                    "aleoflow mcp closed stdout before answering tools/list.\n\
                     binary: {}\n\
                     raw stdout received:\n{}\n\
                     stderr:\n{}",
                    bin,
                    raw_out.join("\n"),
                    stderr_text
                );
            }
        }
    }

    // Terminate the server and reap the reader threads.
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader_out.join();
    let _ = reader_err.join();

    names
}

#[test]
fn broadcast_tools_are_absent_without_allow_env() {
    let names = request_tools_list(None);

    for tool in BROADCAST_TOOLS {
        assert!(
            !names.iter().any(|n| n == tool),
            "broadcast tool '{tool}' must NOT be listed without \
             ALEOFLOW_MCP_ALLOW_BROADCAST. tools/list returned: {names:?}"
        );
    }

    // Positive controls: the safe dry-run counterparts must always be listed.
    for tool in DRY_RUN_TOOLS {
        assert!(
            names.iter().any(|n| n == tool),
            "dry-run tool '{tool}' must always be listed. tools/list returned: {names:?}"
        );
    }
}

#[test]
fn broadcast_tools_are_listed_with_allow_env() {
    let names = request_tools_list(Some("true"));

    for tool in BROADCAST_TOOLS {
        assert!(
            names.iter().any(|n| n == tool),
            "broadcast tool '{tool}' must be listed when \
             ALEOFLOW_MCP_ALLOW_BROADCAST=true. tools/list returned: {names:?}"
        );
    }
}
