//! A minimal fake upstream MCP server used as an integration-test fixture.
//!
//! It speaks just enough newline-delimited JSON-RPC over stdio (MCP spec
//! `2025-11-25`) to exercise the proxy: it answers `initialize`, `tools/list`,
//! and `tools/call`, and echoes back the arguments it actually received so a
//! test can assert which pins/defaults the proxy injected.
//!
//! Tools advertised:
//! - `search` — args `query` (string) and `artifact` (string).
//! - `delete`  — args `path` (string).
//! - `secret`  — a tool a sealed policy is expected to drop.
//!
//! The fixture is selected via the `LAGOM_FAKE_DRIFT` env var: when set, it
//! advertises a *different* surface (the `artifact` arg renamed to `vault`) so a
//! test can drive the drift-fail-loud path (`SPEC.md` §5.3).
//!
//! It is **strict about the MCP lifecycle** (MCP spec, `2025-11-25`): a
//! `tools/list` or `tools/call` received before the `initialize`/`initialized`
//! handshake completes is answered with a JSON-RPC error, mirroring real
//! upstreams that reject pre-initialize traffic. This lets a test assert lagom
//! performs the handshake before probing tools.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// The tool surface this fixture advertises, depending on `LAGOM_FAKE_DRIFT`.
fn tools(drift: bool) -> Value {
    let artifact_arg = if drift { "vault" } else { "artifact" };
    json!([
        {
            "name": "search",
            "description": "Search an artifact.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    artifact_arg: { "type": "string" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "delete",
            "description": "Delete a path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "secret",
            "description": "A tool that sealed policies drop.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

/// JSON-RPC error code for a request received before the lifecycle handshake.
const NOT_INITIALIZED_CODE: i64 = -32002;

fn main() {
    let drift = std::env::var_os("LAGOM_FAKE_DRIFT").is_some();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    // MCP lifecycle state: `tools/list`/`tools/call` are rejected until the
    // `initialize` request + `notifications/initialized` notification arrive.
    let mut initialized = false;

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");

        if method == "notifications/initialized" {
            initialized = true;
            continue;
        }

        // Strict lifecycle: reject tool traffic before the handshake completes.
        if matches!(method, "tools/list" | "tools/call") && !initialized {
            let err = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": NOT_INITIALIZED_CODE,
                    "message": "received before initialize handshake"
                }
            });
            let mut out = serde_json::to_string(&err).expect("serialize response");
            out.push('\n');
            if stdout.write_all(out.as_bytes()).is_err() {
                break;
            }
            let _ = stdout.flush();
            continue;
        }

        let response = match method {
            "initialize" => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fake-upstream", "version": "0.0.0" }
                }
            })),
            "tools/list" => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tools(drift) }
            })),
            "tools/call" => {
                // Echo the received name + arguments so the test can assert what
                // the proxy forwarded (pins injected, hidden args absent, …).
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": "ok"
                        }],
                        "echo": params
                    }
                }))
            }
            // Notifications (no id) get no response.
            _ if id.is_none() => None,
            // Unknown request: a JSON-RPC method-not-found error.
            _ => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not found" }
            })),
        };

        if let Some(response) = response {
            let mut out = serde_json::to_string(&response).expect("serialize response");
            out.push('\n');
            if stdout.write_all(out.as_bytes()).is_err() {
                break;
            }
            let _ = stdout.flush();
        }
    }
}
