//! End-to-end test against a REAL foreign MCP server implementation: the
//! shipped `lagom` binary wrapping the Node fixture `bin/fast-mcp.js` over real
//! process stdio (`SPEC.md` §7.1, §10). The in-repo Rust fixture already covers
//! the bridge; this covers a server lagom does not control — different runtime,
//! different framing habits — and proves the full sandbox story an external
//! consumer sees: drop, rename, pin injection, rejection annotation, and the
//! refuse-to-forward-unparsed-bytes guarantee.
//!
//! Requires `node` on PATH, so it is `#[ignore]`d from the plain `just ci`
//! gate and runs via `just e2e` (its own CI job).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command as StdCommand, Stdio};

/// Repo-relative path to the Node MCP fixture.
fn fast_mcp_js() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/fast-mcp.js")
}

/// A fresh, empty working directory so `lagom serve` discovers no stray
/// `lagom.toml` from the checkout.
fn empty_cwd() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lagom-e2e-cwd-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
#[ignore = "needs `node` on PATH; run via `just e2e`"]
fn cli_projects_a_real_node_mcp_server_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("lagom.toml");
    // Sealed allowlist over the fixture's {echo, secret}: drop `secret`, keep
    // `echo` under the downstream name `say`, pin `token` (hidden + injected).
    std::fs::write(
        &cfg,
        concat!(
            "default-presence = \"drop\"\n",
            "[tools.echo]\n",
            "presence = \"keep\"\n",
            "rename = \"say\"\n",
            "[tools.echo.args.token]\n",
            "pin = \"pinned-secret\"\n",
        ),
    )
    .unwrap();

    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_lagom"))
        .current_dir(empty_cwd())
        .args([
            "serve",
            "--config",
            cfg.to_str().unwrap(),
            "--",
            "node",
            fast_mcp_js().to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn lagom serve over the node fixture");

    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));

    let send = |stdin: &mut std::process::ChildStdin, msg: &serde_json::Value| {
        let mut line = serde_json::to_string(msg).unwrap();
        line.push('\n');
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.flush().unwrap();
    };
    let send_raw = |stdin: &mut std::process::ChildStdin, line: &str| {
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };
    let recv = |stdout: &mut BufReader<std::process::ChildStdout>| -> serde_json::Value {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).expect("read response");
        assert!(n > 0, "proxy closed before responding");
        serde_json::from_str(&line).expect("response is JSON-RPC")
    };

    // Harness-side MCP handshake, forwarded verbatim to the real node server.
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let init = recv(&mut stdout);
    assert_eq!(init["id"], serde_json::json!(1));
    assert_eq!(
        init["result"]["serverInfo"]["name"],
        serde_json::json!("fast"),
        "initialize reaches the real upstream"
    );
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );

    // 1. The projected surface: `secret` dropped, `echo` renamed to `say`, and
    //    the pinned `token` erased from schema + required.
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    );
    let list = recv(&mut stdout);
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1, "secret is dropped: {tools:?}");
    assert_eq!(tools[0]["name"], serde_json::json!("say"), "echo renamed");
    let schema = &tools[0]["inputSchema"];
    assert!(
        schema["properties"].get("token").is_none(),
        "pinned arg erased from properties: {schema}"
    );
    assert!(
        schema["required"]
            .as_array()
            .is_none_or(|r| !r.contains(&serde_json::json!("token"))),
        "pinned arg erased from required: {schema}"
    );

    // 2. A call to the downstream name with only `message`: lagom maps
    //    `say` -> `echo` and injects the pin. The fixture echoes name+args
    //    back, proving what the real upstream actually received.
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params": {"name":"say","arguments":{"message":"hi"}}
        }),
    );
    let call = recv(&mut stdout);
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .expect("fixture echoes text");
    assert!(text.starts_with("echo:"), "rename mapped back: {text}");
    assert!(
        text.contains("\"token\":\"pinned-secret\""),
        "pin injected upstream: {text}"
    );
    assert!(text.contains("\"message\":\"hi\""), "arg forwarded: {text}");

    // 3. Calling the dropped tool is rejected by lagom, annotated, and never
    //    reaches the upstream.
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params": {"name":"secret","arguments":{"key":"k"}}
        }),
    );
    let rejected = recv(&mut stdout);
    assert!(
        rejected["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("rejected by lagom")),
        "dropped tool call is rejected annotated: {rejected}"
    );

    // 4. Unparseable bytes are refused, never forwarded (parser-differential
    //    guard) — even though this node fixture would happily ignore them.
    send_raw(&mut stdin, "{\"method\":\"tools/call\"} trailing-garbage");
    let parse_err = recv(&mut stdout);
    assert_eq!(
        parse_err["error"]["code"],
        serde_json::json!(-32700),
        "invalid JSON rejected: {parse_err}"
    );

    // Close the session; the bridge tears the node child down.
    drop(stdin);
    let _ = child.wait().expect("await lagom serve exit");
}
