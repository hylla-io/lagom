//! CLI-level (`assert_cmd`) tests for the `lagom` binary: the `emit`,
//! `emit-skills`, and `validate` subcommands, happy and sad paths.
//!
//! `serve` is exercised end-to-end by the proxy round-trip tests
//! (`lagom-proxy/tests/proxy_roundtrip.rs`); here we cover the surface the CLI
//! adds on top — argument plumbing, snippet rendering, skill emission, and the
//! drift exit code (`SPEC.md` §5.3, §6.5, §12).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command as StdCommand, Stdio};

use assert_cmd::Command;
use predicates::prelude::*;

/// The compiled fake-upstream fixture this crate ships for tests.
fn upstream_bin() -> &'static str {
    env!("CARGO_BIN_EXE_lagom_test_upstream")
}

/// A fresh `lagom` command handle.
fn lagom() -> Command {
    Command::cargo_bin("lagom").expect("lagom binary builds")
}

#[test]
fn emit_prints_pasteable_snippet() {
    lagom()
        .args(["emit", "--name", "hylla", "--", "npx", "-y", "@x/srv"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"mcpServers\""))
        .stdout(predicate::str::contains("\"hylla\""))
        .stdout(predicate::str::contains("\"command\": \"lagom\""))
        .stdout(predicate::str::contains("\"serve\""))
        .stdout(predicate::str::contains("@x/srv"));
}

#[test]
fn emit_embeds_config_path_when_given() {
    lagom()
        .args(["emit", "--config", "lagom.toml", "--", "srv"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--config"))
        .stdout(predicate::str::contains("lagom.toml"));
}

#[test]
fn emit_without_upstream_is_error() {
    // Missing the required trailing upstream command -> clap usage error (exit 2).
    lagom().args(["emit"]).assert().failure();
}

#[test]
fn emit_skills_writes_both_documents() {
    let dir = tempfile::tempdir().unwrap();
    lagom()
        .args(["emit-skills", dir.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("lagom-slim-docs.skill.md"))
        .stdout(predicate::str::contains("lagom-dynamic-mint.skill.md"));

    let slim = std::fs::read_to_string(dir.path().join("lagom-slim-docs.skill.md")).unwrap();
    let mint = std::fs::read_to_string(dir.path().join("lagom-dynamic-mint.skill.md")).unwrap();
    // Each emitted file leads with its naming heading (`SPEC.md` §12).
    assert!(slim.starts_with("# lagom-slim-docs"));
    assert!(mint.starts_with("# lagom-dynamic-mint"));
    // And carries its load-bearing emphasis from the spec.
    assert!(slim.contains("caveman"));
    assert!(mint.contains("ephemeral projection"));
}

#[test]
fn validate_grounded_policy_succeeds() {
    // A policy pinning `artifact` is grounded against the non-drift fixture.
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("lagom.toml");
    std::fs::write(&cfg, "[tools.search.args.artifact]\npin = \"hylla\"\n").unwrap();

    lagom()
        .current_dir(empty_cwd())
        .args([
            "validate",
            "--config",
            cfg.to_str().unwrap(),
            "--",
            upstream_bin(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("ok"));
}

#[test]
fn validate_drifted_policy_exits_nonzero() {
    // The drift fixture renames `artifact` -> `vault`; pinning `artifact` is a
    // stale reference, so validate must fail loud and exit non-zero (§5.3).
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("lagom.toml");
    std::fs::write(&cfg, "[tools.search.args.artifact]\npin = \"hylla\"\n").unwrap();

    lagom()
        .current_dir(empty_cwd())
        .env("LAGOM_FIXTURE_DRIFT", "1")
        .args([
            "validate",
            "--config",
            cfg.to_str().unwrap(),
            "--",
            upstream_bin(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("drift"))
        .stderr(predicate::str::contains("artifact"));
}

#[test]
fn serve_with_audit_writes_trace_after_roundtrip() {
    // Drive the *shipped* `lagom serve --audit` face end-to-end: spawn it as a
    // real subprocess over stdio, complete the MCP handshake, issue one
    // `tools/call` that the pinned policy rewrites, then close the session. The
    // audit log on disk must then carry the full §8.2/§9.3 trace head
    // (OriginalDefs + ResolvedPolicy) plus the Rewrite — proving the flag routes
    // through `serve_audited` and actually persists a trace (the High
    // "no shipped face opens an audit log" gap).
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("lagom.toml");
    // Pin `artifact`: an agent call carrying only `query` is rewritten (the pin
    // is injected), which the proxy records as an `AuditEvent::Rewrite`.
    std::fs::write(&cfg, "[tools.search.args.artifact]\npin = \"hylla\"\n").unwrap();
    let log_path = dir.path().join("audit.jsonl");

    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_lagom"))
        .current_dir(empty_cwd())
        .args([
            "serve",
            "--config",
            cfg.to_str().unwrap(),
            "--audit",
            log_path.to_str().unwrap(),
            "--run-id",
            "cli-run",
            "--",
            upstream_bin(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn lagom serve");

    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));

    let send = |stdin: &mut std::process::ChildStdin, msg: &serde_json::Value| {
        let mut line = serde_json::to_string(msg).unwrap();
        line.push('\n');
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.flush().unwrap();
    };
    let recv = |stdout: &mut BufReader<std::process::ChildStdout>| -> serde_json::Value {
        let mut line = String::new();
        let n = stdout.read_line(&mut line).expect("read response");
        assert!(n > 0, "proxy closed before responding");
        serde_json::from_str(&line).expect("response is JSON-RPC")
    };

    // Harness-side MCP handshake (forwarded verbatim to the upstream).
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let init = recv(&mut stdout);
    assert_eq!(init["id"], serde_json::json!(1));
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );

    // A `tools/call` carrying only `query`; the pinned `artifact` is injected by
    // `rewrite`, producing an `AuditEvent::Rewrite`.
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params": {"name":"search","arguments":{"query":"abc"}}
        }),
    );
    let call = recv(&mut stdout);
    assert_eq!(call["id"], serde_json::json!(2), "call response correlates");

    // Close the session so the bridge finishes and the log is flushed.
    drop(stdin);
    let _ = child.wait().expect("await lagom serve exit");

    let contents = std::fs::read_to_string(&log_path).expect("audit log was created");
    let events: Vec<serde_json::Value> = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is a JSON audit event"))
        .collect();

    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(
        kinds.contains(&"original_defs"),
        "audit log records OriginalDefs (§8.2 trace head): {kinds:?}"
    );
    assert!(
        kinds.contains(&"resolved_policy"),
        "audit log records ResolvedPolicy (refire basis, §8.2): {kinds:?}"
    );
    assert!(
        kinds.contains(&"rewrite"),
        "audit log records the Rewrite from the round-trip (§9.3): {kinds:?}"
    );
    // Every event is tagged with the supplied run id (§8.2 trace linkage).
    assert!(
        events
            .iter()
            .all(|e| e["run_id"] == serde_json::json!("cli-run")),
        "every audit record carries the --run-id"
    );
}

/// A throwaway empty directory to run `validate` from, so config *discovery*
/// (which walks the real cwd) cannot pick up a stray `lagom.toml` and perturb
/// the explicit `--config` under test.
fn empty_cwd() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lagom-cli-cwd-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
