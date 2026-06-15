//! Integration tests that drive real `tools/list` + `tools/call` round-trips
//! through the proxy against the in-repo fake upstream fixture
//! (`src/bin/fake_upstream.rs`), asserting the `SPEC.md` guarantees end-to-end:
//!
//! - dropped tools are absent from the projected `tools/list` (§4.1);
//! - a pinned arg is removed from the projected schema and injected on the
//!   upstream call, overriding any agent value (§3, §4.1);
//! - enum / range / pattern constraints are enforced, out-of-bound calls
//!   rejected with an annotation and never forwarded upstream (§9.1);
//! - untouched traffic passes through unchanged (passthrough, `CONTEXT.md`);
//! - startup drift fails loud (§5.3);
//! - refire reproduces a byte-identical resolved policy (§8.2, §5.4).

use std::collections::BTreeMap;

use lagom_audit::{AuditEvent, AuditLog};
use lagom_core::{ArgPolicy, Constraint, Policy, Presence, ToolPolicy};
use lagom_proxy::{
    MintRecord, PolicySources, ProxyError, ResolvedPolicy, UpstreamCommand, mint, refire,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// Path to the compiled fake-upstream fixture binary.
fn fixture_command(drift: bool) -> UpstreamCommand {
    UpstreamCommand {
        command: env!("CARGO_BIN_EXE_fake_upstream").to_string(),
        args: vec![],
        env: if drift {
            vec![("LAGOM_FAKE_DRIFT".to_string(), "1".to_string())]
        } else {
            vec![]
        },
    }
}

/// Build a tool policy from an arg map for terser test setup.
fn tool_policy(presence: Option<Presence>, args: BTreeMap<String, ArgPolicy>) -> ToolPolicy {
    ToolPolicy {
        presence,
        args,
        ..Default::default()
    }
}

/// A policy that exercises every transform against the fixture:
/// - `search`: pin `artifact`, constrain `query` to a pattern;
/// - `delete`: constrain `path` to an enum;
/// - `secret`: dropped.
fn projection_policy() -> Policy {
    let mut search_args = BTreeMap::new();
    search_args.insert("artifact".to_string(), ArgPolicy::Pin(json!("hylla")));
    search_args.insert(
        "query".to_string(),
        ArgPolicy::Constrain(Constraint::Pattern("^[a-z]+$".to_string())),
    );

    let mut delete_args = BTreeMap::new();
    delete_args.insert(
        "path".to_string(),
        ArgPolicy::Constrain(Constraint::Enum(vec![json!("a.txt"), json!("b.txt")])),
    );

    let mut tools = BTreeMap::new();
    tools.insert("search".to_string(), tool_policy(None, search_args));
    tools.insert("delete".to_string(), tool_policy(None, delete_args));
    tools.insert(
        "secret".to_string(),
        tool_policy(Some(Presence::Drop), BTreeMap::new()),
    );

    Policy {
        default_presence: Presence::Keep,
        tools,
    }
}

/// A harness end of the proxy: writes requests in, reads responses out, line by
/// line. Backed by two `tokio::io::duplex` pairs.
struct Harness {
    to_proxy: DuplexStream,
    from_proxy: BufReader<DuplexStream>,
}

impl Harness {
    /// Send one JSON-RPC message as a newline-delimited line.
    async fn send(&mut self, msg: &Value) {
        let mut line = serde_json::to_string(msg).unwrap();
        line.push('\n');
        self.to_proxy.write_all(line.as_bytes()).await.unwrap();
        self.to_proxy.flush().await.unwrap();
    }

    /// Read the next JSON-RPC message line from the proxy.
    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.from_proxy.read_line(&mut line).await.unwrap();
        assert!(n > 0, "proxy closed before sending a response");
        serde_json::from_str(&line).unwrap()
    }
}

/// Spawn the proxy bridging the fixture, returning a harness handle plus the
/// join handle for the running bridge. `audit` is wired in under `run_id`.
async fn start_proxy(
    policy: Policy,
    audit: Option<AuditLog>,
    run_id: &str,
) -> Result<(Harness, tokio::task::JoinHandle<()>), ProxyError> {
    let resolved = ResolvedPolicy {
        policy,
        upstream: fixture_command(false),
    };
    let server = lagom_proxy::test_support::spawn_and_validate(resolved).await?;

    // harness → proxy, and proxy → harness, each a duplex pair.
    let (harness_to_proxy, proxy_in) = tokio::io::duplex(64 * 1024);
    let (proxy_out, harness_from_proxy) = tokio::io::duplex(64 * 1024);

    let run_id = run_id.to_string();
    let handle = tokio::spawn(async move {
        let _ = server.run_with(proxy_in, proxy_out, audit, run_id).await;
    });

    Ok((
        Harness {
            to_proxy: harness_to_proxy,
            from_proxy: BufReader::new(harness_from_proxy),
        },
        handle,
    ))
}

#[tokio::test]
async fn tools_list_projects_drops_pins_and_constraints() {
    let (mut h, _bridge) = start_proxy(projection_policy(), None, "run-list")
        .await
        .expect("serve");

    h.send(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
        .await;
    let resp = h.recv().await;

    let tools = resp.pointer("/result/tools").unwrap().as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    // Dropped tool absent.
    assert!(!names.contains(&"secret"), "dropped tool must be absent");
    assert!(names.contains(&"search") && names.contains(&"delete"));

    // Pinned `artifact` removed from the projected schema; `query` still there.
    let search = tools.iter().find(|t| t["name"] == "search").unwrap();
    let props = search.pointer("/inputSchema/properties").unwrap();
    assert!(
        props.get("artifact").is_none(),
        "pinned arg must be hidden from the schema"
    );
    assert!(props.get("query").is_some());
    // Pattern constraint surfaced on the projected schema.
    assert_eq!(
        search.pointer("/inputSchema/properties/query/pattern"),
        Some(&json!("^[a-z]+$"))
    );

    // Enum constraint surfaced on `delete.path`.
    let delete = tools.iter().find(|t| t["name"] == "delete").unwrap();
    assert_eq!(
        delete.pointer("/inputSchema/properties/path/enum"),
        Some(&json!(["a.txt", "b.txt"]))
    );
}

#[tokio::test]
async fn tools_call_injects_pin_and_forwards() {
    let (mut h, _bridge) = start_proxy(projection_policy(), None, "run-call")
        .await
        .expect("serve");

    // Agent calls search with its own artifact value and a valid query.
    h.send(&json!({
        "jsonrpc":"2.0","id":7,"method":"tools/call",
        "params": {"name":"search","arguments":{"query":"abc","artifact":"evil"}}
    }))
    .await;
    let resp = h.recv().await;

    // The fixture echoes what it actually received: the pin must override.
    let echoed = resp.pointer("/result/echo/arguments").unwrap();
    assert_eq!(
        echoed["artifact"],
        json!("hylla"),
        "pin overrides agent value"
    );
    assert_eq!(echoed["query"], json!("abc"), "passthrough arg preserved");
    assert_eq!(resp["id"], json!(7), "response id correlates to request");
}

#[tokio::test]
async fn batch_is_rejected_not_forwarded() {
    let (mut h, _bridge) = start_proxy(projection_policy(), None, "run-batch")
        .await
        .expect("serve");

    // A JSON-RPC batch (top-level array) carrying a tools/call must NOT slip
    // past projection (pin/constraint enforcement). MCP 2025-11-25 forbids
    // batching, so lagom rejects it loudly rather than forwarding it unexamined.
    h.send(&json!([
        {"jsonrpc":"2.0","id":1,"method":"tools/call",
         "params":{"name":"search","arguments":{"query":"abc","artifact":"evil"}}}
    ]))
    .await;
    let resp = h.recv().await;

    let err = resp
        .get("error")
        .expect("batch must be rejected as an error");
    assert_eq!(err["code"], json!(-32600), "JSON-RPC invalid-request code");
    assert!(
        resp.get("result").is_none(),
        "a batched call must never reach the upstream"
    );
}

#[tokio::test]
async fn out_of_pattern_call_rejected_without_upstream() {
    let (mut h, _bridge) = start_proxy(projection_policy(), None, "run-pat")
        .await
        .expect("serve");

    // `query` violates ^[a-z]+$ — must be rejected, never forwarded.
    h.send(&json!({
        "jsonrpc":"2.0","id":9,"method":"tools/call",
        "params": {"name":"search","arguments":{"query":"ABC123"}}
    }))
    .await;
    let resp = h.recv().await;

    assert_eq!(resp["id"], json!(9));
    let err = resp.get("error").expect("rejection is a JSON-RPC error");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("rejected by lagom"),
        "rejection must be annotated (§9.1): {err}"
    );
    assert!(
        err["message"].as_str().unwrap().contains("query"),
        "annotation names the offending arg"
    );
    // No `result` — it never reached the fixture (which would have echoed).
    assert!(resp.get("result").is_none());
}

#[tokio::test]
async fn out_of_enum_call_rejected() {
    let (mut h, _bridge) = start_proxy(projection_policy(), None, "run-enum")
        .await
        .expect("serve");

    h.send(&json!({
        "jsonrpc":"2.0","id":11,"method":"tools/call",
        "params": {"name":"delete","arguments":{"path":"/etc/passwd"}}
    }))
    .await;
    let resp = h.recv().await;
    assert!(resp.get("error").is_some(), "out-of-enum must be rejected");

    // An in-enum value is accepted and forwarded.
    h.send(&json!({
        "jsonrpc":"2.0","id":12,"method":"tools/call",
        "params": {"name":"delete","arguments":{"path":"a.txt"}}
    }))
    .await;
    let ok = h.recv().await;
    assert_eq!(
        ok.pointer("/result/echo/arguments/path"),
        Some(&json!("a.txt"))
    );
}

#[tokio::test]
async fn range_constraint_enforced() {
    // A fresh policy constraining delete.path is replaced by a numeric range on a
    // synthetic arg: we reuse `search.query` with a range to keep the fixture.
    let mut args = BTreeMap::new();
    args.insert(
        "query".to_string(),
        ArgPolicy::Constrain(Constraint::Range {
            min: Some(1.0),
            max: Some(10.0),
        }),
    );
    let mut tools = BTreeMap::new();
    tools.insert("search".to_string(), tool_policy(None, args));
    let policy = Policy {
        default_presence: Presence::Keep,
        tools,
    };

    let (mut h, _bridge) = start_proxy(policy, None, "run-range").await.expect("serve");

    // Out of range -> rejected.
    h.send(&json!({
        "jsonrpc":"2.0","id":21,"method":"tools/call",
        "params": {"name":"search","arguments":{"query":42}}
    }))
    .await;
    assert!(h.recv().await.get("error").is_some(), "above max rejected");

    // In range -> forwarded.
    h.send(&json!({
        "jsonrpc":"2.0","id":22,"method":"tools/call",
        "params": {"name":"search","arguments":{"query":5}}
    }))
    .await;
    assert_eq!(
        h.recv().await.pointer("/result/echo/arguments/query"),
        Some(&json!(5))
    );
}

#[tokio::test]
async fn initialize_and_unknown_methods_passthrough_unchanged() {
    let (mut h, _bridge) = start_proxy(projection_policy(), None, "run-pass")
        .await
        .expect("serve");

    // initialize passes through to the upstream and back verbatim.
    h.send(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}))
        .await;
    let init = h.recv().await;
    assert_eq!(
        init.pointer("/result/serverInfo/name"),
        Some(&json!("fake-upstream"))
    );

    // A resources/list (not narrowed in 0.1.0) passes through; the fixture
    // answers method-not-found, which must reach the harness unchanged.
    h.send(&json!({"jsonrpc":"2.0","id":2,"method":"resources/list","params":{}}))
        .await;
    let res = h.recv().await;
    assert_eq!(
        res["error"]["code"],
        json!(-32601),
        "passthrough error unchanged"
    );
}

#[tokio::test]
async fn list_changed_reprojection() {
    // After a tools/list_changed notification the harness re-issues tools/list;
    // the proxy must re-project that fresh response too.
    let (mut h, _bridge) = start_proxy(projection_policy(), None, "run-changed")
        .await
        .expect("serve");

    for id in [100, 101] {
        h.send(&json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":{}}))
            .await;
        let resp = h.recv().await;
        let names: Vec<&str> = resp
            .pointer("/result/tools")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(
            !names.contains(&"secret"),
            "re-projection still drops secret"
        );
    }
}

#[tokio::test]
async fn drift_fails_loud_and_refuses_to_serve() {
    // The drift fixture renames `artifact` to `vault`; our policy still pins
    // `artifact`, a stale reference -> validate must fail and refuse to serve.
    let resolved = ResolvedPolicy {
        policy: projection_policy(),
        upstream: fixture_command(true),
    };
    let err = lagom_proxy::serve_audited(resolved, None, "run-drift")
        .await
        .expect_err("drift must refuse to serve");
    match err {
        ProxyError::Drift(msgs) => {
            assert!(
                msgs.iter().any(|m| m.contains("artifact")),
                "drift names the vanished arg: {msgs:?}"
            );
        }
        other => panic!("expected Drift, got {other:?}"),
    }
}

#[tokio::test]
async fn pipelined_requests_stay_id_correlated() {
    // Fire a burst of mixed requests *before* reading any response, to exercise
    // the duplex bridge under interleaving. A rejected `tools/call` is answered
    // by the downstream→upstream task; forwarded ones by the upstream→downstream
    // task. Responses may arrive in any order, so we index them by id and assert
    // each one is correct — every id must be answered exactly once.
    use std::collections::HashMap;

    let (mut h, _bridge) = start_proxy(projection_policy(), None, "run-pipe")
        .await
        .expect("serve");

    // ids 1..=6: alternating valid (forwarded) and invalid (rejected) searches.
    let mut expect_ok = std::collections::HashSet::new();
    let mut expect_err = std::collections::HashSet::new();
    for id in 1..=6u64 {
        let query = if id % 2 == 0 { "abc" } else { "BAD" };
        if id % 2 == 0 {
            expect_ok.insert(id);
        } else {
            expect_err.insert(id);
        }
        h.send(&json!({
            "jsonrpc":"2.0","id":id,"method":"tools/call",
            "params": {"name":"search","arguments":{"query":query}}
        }))
        .await;
    }

    let mut by_id: HashMap<u64, Value> = HashMap::new();
    for _ in 1..=6 {
        let resp = h.recv().await;
        let id = resp["id"].as_u64().expect("numeric id echoed back");
        assert!(by_id.insert(id, resp).is_none(), "each id answered once");
    }

    for id in expect_ok {
        let resp = &by_id[&id];
        assert!(
            resp.get("result").is_some(),
            "id {id} (valid) must be forwarded: {resp}"
        );
        assert_eq!(
            resp.pointer("/result/echo/arguments/artifact"),
            Some(&json!("hylla")),
            "id {id} got the pin injected"
        );
    }
    for id in expect_err {
        let resp = &by_id[&id];
        assert!(
            resp.get("error").is_some(),
            "id {id} (invalid) must be rejected: {resp}"
        );
    }
}

#[tokio::test]
async fn audit_log_records_rewrite_and_rejection() {
    let dir = std::env::temp_dir().join(format!("lagom-proxy-audit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("audit.jsonl");
    let audit = AuditLog::open(&log_path).unwrap();

    let (mut h, bridge) = start_proxy(projection_policy(), Some(audit), "run-audit")
        .await
        .expect("serve");

    // One accepted (rewrite) and one rejected call.
    h.send(&json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params": {"name":"search","arguments":{"query":"abc"}}
    }))
    .await;
    h.recv().await;
    h.send(&json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call",
        "params": {"name":"search","arguments":{"query":"BAD"}}
    }))
    .await;
    h.recv().await;

    // Close the harness so the bridge finishes and flushes.
    drop(h);
    let _ = bridge.await;

    let contents = std::fs::read_to_string(&log_path).unwrap();
    let events: Vec<AuditEvent> = contents
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AuditEvent::Rewrite { .. })),
        "a rewrite was recorded"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AuditEvent::Rejection { .. })),
        "a rejection was recorded"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn handshake_runs_and_audit_records_original_defs_and_resolved_policy() {
    // The fake fixture is *strict* about the MCP lifecycle: it answers
    // `tools/list` only after the `initialize`/`initialized` handshake. So a
    // successful spawn-and-validate here is itself proof the proxy performed the
    // handshake before probing (`SPEC.md` §5.3; MCP `2025-11-25` lifecycle) —
    // before the fix, the probe's bare `tools/list` would have been rejected and
    // `start_proxy` would error.
    let dir = std::env::temp_dir().join(format!(
        "lagom-proxy-trace-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("audit.jsonl");
    let audit = AuditLog::open(&log_path).unwrap();

    let (mut h, bridge) = start_proxy(projection_policy(), Some(audit), "run-trace")
        .await
        .expect("handshake + probe must succeed against the strict-lifecycle fixture");

    // Drive one call so the session is live, then close to flush.
    h.send(&json!({
        "jsonrpc":"2.0","id":1,"method":"tools/call",
        "params": {"name":"search","arguments":{"query":"abc"}}
    }))
    .await;
    h.recv().await;
    drop(h);
    let _ = bridge.await;

    let contents = std::fs::read_to_string(&log_path).unwrap();
    let events: Vec<AuditEvent> = contents
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    // OriginalDefs recorded once at session start, carrying the probed surface.
    let original = events
        .iter()
        .find_map(|e| match e {
            AuditEvent::OriginalDefs { run_id, defs } => Some((run_id, defs)),
            _ => None,
        })
        .expect("OriginalDefs must be recorded at session start (§8.2, §9.3)");
    assert_eq!(original.0, "run-trace", "trace head carries the run id");
    let names: Vec<&str> = original.1.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"search") && names.contains(&"delete") && names.contains(&"secret"),
        "OriginalDefs holds the *full* upstream surface, pre-projection: {names:?}"
    );

    // ResolvedPolicy recorded once, equal to the policy being served.
    let resolved = events
        .iter()
        .find_map(|e| match e {
            AuditEvent::ResolvedPolicy { run_id, policy } => Some((run_id, policy)),
            _ => None,
        })
        .expect("ResolvedPolicy must be recorded at session start (§8.2, §9.3)");
    assert_eq!(resolved.0, "run-trace");
    assert_eq!(
        resolved.1,
        &projection_policy(),
        "ResolvedPolicy mirrors the served projection spec"
    );

    // Exactly one of each trace-head event (recorded once, not per message).
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AuditEvent::OriginalDefs { .. }))
            .count(),
        1,
        "OriginalDefs recorded exactly once"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AuditEvent::ResolvedPolicy { .. }))
            .count(),
        1,
        "ResolvedPolicy recorded exactly once"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn refire_is_byte_identical() {
    let tmp = std::env::temp_dir().join(format!("lagom-refire-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let cfg = tmp.join("lagom.toml");
    std::fs::write(
        &cfg,
        "[tools.search.args.artifact]\npin = \"hylla\"\n[tools.secret]\npresence = \"drop\"\n",
    )
    .unwrap();

    let sources = PolicySources {
        config_paths: vec![cfg],
        upstream: fixture_command(false),
        dynamic_inputs: Value::Null,
    };
    let resolved = mint(&sources).expect("mint");
    let record = MintRecord {
        run_id: "run-refire".into(),
        sources,
        resolved: resolved.clone(),
    };
    let refired = refire(&record);
    assert_eq!(
        serde_json::to_string(&refired).unwrap(),
        serde_json::to_string(&resolved).unwrap(),
        "refire reproduces the recorded resolved policy byte-for-byte"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
