//! A minimal fake upstream MCP server, compiled as a test-only helper binary so
//! the CLI integration tests can drive `lagom validate` end-to-end.
//!
//! Speaks just enough newline-delimited JSON-RPC over stdio to answer the
//! drift-probe `tools/list` (`SPEC.md` §5.3). With `LAGOM_FIXTURE_DRIFT` set it
//! advertises a *different* surface (the `artifact` arg renamed to `vault`) so a
//! test can drive the drift-fail path.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

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
        }
    ])
}

fn main() {
    let drift = std::env::var_os("LAGOM_FIXTURE_DRIFT").is_some();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

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
        let response = match method {
            "tools/list" => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tools(drift) }
            })),
            _ if id.is_none() => None,
            _ => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            })),
        };
        if let Some(response) = response {
            let mut out = serde_json::to_string(&response).expect("serialize");
            out.push('\n');
            if stdout.write_all(out.as_bytes()).is_err() {
                break;
            }
            let _ = stdout.flush();
        }
    }
}
