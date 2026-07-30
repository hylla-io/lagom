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
use std::time::{Duration, Instant};

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
fn validate_without_any_config_warns_ungated_passthrough() {
    // Zero discovered configs resolve to a passthrough policy (`SPEC.md` §3) —
    // specified behavior, so this must still exit 0. What must NOT happen is
    // silence: an operator whose config never landed on a searched path sees the
    // same output as one who wants passthrough. Both surfaces must say so — the
    // stderr diagnostic (with the searched paths) and validate's own report.
    let cwd = std::fs::canonicalize(empty_cwd()).unwrap();
    let home = std::fs::canonicalize(empty_cwd()).unwrap();

    lagom()
        .current_dir(&cwd)
        // Redirect HOME/XDG so the developer's real user config cannot be found.
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .args(["validate", "--", upstream_bin()])
        .assert()
        .success()
        .stderr(predicate::str::contains("no lagom.toml discovered"))
        .stderr(predicate::str::contains("nothing is gated"))
        .stderr(predicate::str::contains(
            cwd.join("lagom.toml").display().to_string(),
        ))
        .stderr(predicate::str::contains(
            home.join("lagom").join("lagom.toml").display().to_string(),
        ))
        // The report must not claim a grounded policy for a policy that
        // references nothing.
        .stdout(predicate::str::contains("passthrough"))
        .stdout(predicate::str::contains("nothing is gated"))
        .stdout(predicate::str::contains("policy is grounded").not());
}

#[test]
fn validate_with_real_policy_reports_grounded_not_passthrough() {
    // The discriminating control for the test above: a policy that actually
    // references the upstream keeps the grounded report and emits no
    // unconfigured warning.
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("lagom.toml");
    std::fs::write(&cfg, "[tools.search.args.artifact]\npin = \"hylla\"\n").unwrap();
    let home = std::fs::canonicalize(empty_cwd()).unwrap();

    lagom()
        .current_dir(empty_cwd())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .args([
            "validate",
            "--config",
            cfg.to_str().unwrap(),
            "--",
            upstream_bin(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "ok: policy is grounded against the live upstream",
        ))
        .stdout(predicate::str::contains("passthrough").not())
        .stderr(predicate::str::contains("no lagom.toml discovered").not());
}

#[test]
fn serve_with_vacuous_config_warns_ungated_on_stderr() {
    // The load-bearing case: a 0-byte `lagom.toml` IS discovered, so a
    // discovery-emptiness gate stays silent — yet the resolved policy is
    // byte-identical to zero-config passthrough and gates nothing. `serve` is the
    // surface that actually fronts agents, so silence here is the dangerous one.
    // Exit stays 0 (`SPEC.md` §3 passthrough is legal) and stdout stays byte-empty
    // because it is the JSON-RPC channel.
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(cwd.join("lagom.toml"), "").unwrap();
    let home = std::fs::canonicalize(empty_cwd()).unwrap();

    lagom()
        .current_dir(&cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .args(["serve", "--", upstream_bin()])
        .write_stdin("")
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing is gated"))
        .stderr(predicate::str::contains("PASSTHROUGH"))
        .stderr(predicate::str::contains(
            cwd.join("lagom.toml").display().to_string(),
        ))
        // A discovered-but-vacuous config must NOT be reported as undiscovered:
        // the operator has to be able to tell "my file never landed on a searched
        // path" from "my file is there and says nothing".
        .stderr(predicate::str::contains("no lagom.toml discovered").not())
        .stdout(predicate::str::is_empty());
}

#[test]
fn serve_without_any_config_warns_ungated_on_stderr() {
    // The missing-config half of the same diagnostic must keep naming the searched
    // candidates — that is the information the vacuous case cannot give.
    let cwd = std::fs::canonicalize(empty_cwd()).unwrap();
    let home = std::fs::canonicalize(empty_cwd()).unwrap();

    lagom()
        .current_dir(&cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .args(["serve", "--", upstream_bin()])
        .write_stdin("")
        .assert()
        .success()
        .stderr(predicate::str::contains("no lagom.toml discovered"))
        .stderr(predicate::str::contains("nothing is gated"))
        .stderr(predicate::str::contains(
            cwd.join("lagom.toml").display().to_string(),
        ))
        .stderr(predicate::str::contains(
            home.join("lagom").join("lagom.toml").display().to_string(),
        ))
        .stdout(predicate::str::is_empty());
}

#[test]
fn serve_with_gated_config_emits_no_ungated_warning() {
    // The discriminating control: a policy that actually pins an arg gates
    // something, so neither half of the diagnostic may fire. Without this the two
    // tests above would pass on an unconditional warning.
    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        cwd.join("lagom.toml"),
        "[tools.search.args.artifact]\npin = \"hylla\"\n",
    )
    .unwrap();
    let home = std::fs::canonicalize(empty_cwd()).unwrap();

    lagom()
        .current_dir(&cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .args(["serve", "--", upstream_bin()])
        .write_stdin("")
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing is gated").not())
        .stderr(predicate::str::contains("no lagom.toml discovered").not())
        .stdout(predicate::str::is_empty());
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
        kinds.contains(&"mint"),
        "audit log records the full MintRecord with provenance (§8.2): {kinds:?}"
    );
    // The `mint` event carries the resolved policy AND its provenance (the config
    // path it was discovered from + the upstream command) — the refire basis.
    let mint = events
        .iter()
        .find(|e| e["kind"] == "mint")
        .expect("mint event present");
    assert_eq!(
        mint["record"]["run_id"],
        serde_json::json!("cli-run"),
        "mint record is tagged with the run id"
    );
    assert!(
        mint["record"]["sources"]["config_paths"]
            .as_array()
            .is_some_and(|p| !p.is_empty()),
        "mint provenance records the config path: {}",
        mint["record"]["sources"]
    );
    assert_eq!(
        mint["record"]["sources"]["upstream"]["command"],
        serde_json::json!(upstream_bin()),
        "mint provenance records the upstream launch command"
    );
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
    // Every event is tagged with the supplied run id (§8.2 trace linkage). The
    // `mint` event carries the run id inside its nested record, the rest at top
    // level.
    assert!(
        events
            .iter()
            .all(|e| e["run_id"] == serde_json::json!("cli-run")
                || e["record"]["run_id"] == serde_json::json!("cli-run")),
        "every audit record carries the --run-id"
    );
}

#[test]
fn refire_from_record_serves_the_recorded_projection() {
    // The §8.2 ephemeral refire loop from the shipped CLI face: write a
    // MintRecord (resolved policy pinning `artifact`, provenance, and the upstream
    // launch command) to disk, then `lagom refire --record <path>` must re-mint
    // that exact projection and serve it — proving a persisted record alone (no
    // on-disk lagom.toml) reconstructs the run. We drive a `tools/call` carrying
    // only `query`; the recorded pin is injected, recorded as a Rewrite.
    let dir = tempfile::tempdir().unwrap();
    let record_path = dir.path().join("mint.json");
    let log_path = dir.path().join("refire-audit.jsonl");

    // A MintRecord whose resolved policy pins `artifact` over the fixture upstream.
    let record = serde_json::json!({
        "run_id": "recorded-run",
        "sources": {
            "config_paths": [],
            "upstream": { "command": upstream_bin(), "args": [], "env": [] },
            "dynamic_inputs": null
        },
        "resolved": {
            "policy": {
                "default_presence": "keep",
                "tools": { "search": { "args": { "artifact": { "pin": "hylla" } } } }
            },
            "upstream": { "command": upstream_bin(), "args": [], "env": [] }
        }
    });
    std::fs::write(&record_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_lagom"))
        .current_dir(empty_cwd())
        .args([
            "refire",
            "--record",
            record_path.to_str().unwrap(),
            "--audit",
            log_path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn lagom refire");

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
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params": {"name":"search","arguments":{"query":"abc"}}
        }),
    );
    let call = recv(&mut stdout);
    assert_eq!(call["id"], serde_json::json!(2), "call response correlates");

    drop(stdin);
    let _ = child.wait().expect("await lagom refire exit");

    let contents = std::fs::read_to_string(&log_path).expect("refire audit log was created");
    let events: Vec<serde_json::Value> = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is a JSON audit event"))
        .collect();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();

    // The refired run re-persists its mint record and records the rewrite from the
    // recorded pin — proving refire served the recorded projection, not a fresh
    // discovery (the cwd is empty, so a re-resolve would have found no pin).
    assert!(
        kinds.contains(&"mint"),
        "refire re-persists a mint record: {kinds:?}"
    );
    assert!(
        kinds.contains(&"rewrite"),
        "the recorded pin must be injected (proves the recorded policy served): {kinds:?}"
    );
    let rewrite = events
        .iter()
        .find(|e| e["kind"] == "rewrite")
        .expect("rewrite present");
    assert_eq!(
        rewrite["upstream"]["arguments"]["artifact"],
        serde_json::json!("hylla"),
        "the recorded pin value was injected by the refired projection"
    );
    assert!(
        events
            .iter()
            .all(|e| e["run_id"] == serde_json::json!("recorded-run")
                || e["record"]["run_id"] == serde_json::json!("recorded-run")),
        "refire defaults the run id to the record's own run_id"
    );
}

#[test]
fn refire_missing_record_is_error() {
    lagom()
        .args(["refire", "--record", "/no/such/mint.json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("mint record"));
}

#[test]
fn serve_with_structurally_nondefault_but_ungated_config_warns() {
    // The shapes an equality-with-`Policy::default()` test cannot see: each parses
    // into a `ToolPolicy` that is structurally NON-default, yet `presence: None`
    // falls through to `default_presence = Keep`, `description` only re-words, and
    // `args` is empty — so every upstream tool stays exposed with every argument
    // agent-settable. Silence here is the dangerous case: the operator sees a
    // `[tools.…]` section and concludes they are gated.
    let cases: &[(&str, &str)] = &[
        ("empty tool table", "[tools.search]\n"),
        (
            "description only",
            "[tools.search]\ndescription = \"slim\"\n",
        ),
        ("presence keep", "[tools.search]\npresence = \"keep\"\n"),
        ("rename only", "[tools.search]\nrename = \"s\"\n"),
    ];

    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let home = std::fs::canonicalize(empty_cwd()).unwrap();

    for (label, body) in cases {
        std::fs::write(cwd.join("lagom.toml"), body).unwrap();
        let out = lagom()
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .args(["serve", "--", upstream_bin()])
            .write_stdin("")
            .output()
            .expect("run lagom serve");
        let stderr = String::from_utf8_lossy(&out.stderr);

        assert!(
            out.status.success(),
            "{label}: passthrough must still serve"
        );
        assert!(
            out.stdout.is_empty(),
            "{label}: stdout is the JSON-RPC channel: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains("nothing is gated"),
            "{label}: the ungated diagnostic must fire: {stderr}"
        );
        assert!(
            stderr.contains("restricts anything"),
            "{label}: rules exist, so the message must say they restrict nothing \
             rather than claim the file declares no rule: {stderr}"
        );
        assert!(
            stderr.contains(&cwd.join("lagom.toml").display().to_string()),
            "{label}: the file that gated nothing must be named: {stderr}"
        );
        assert!(
            !stderr.contains("no lagom.toml discovered"),
            "{label}: the file WAS discovered: {stderr}"
        );
    }
}

#[test]
fn serve_with_genuinely_gating_config_stays_silent() {
    // The discriminating controls for the test above: each of these removes
    // authority (drops a tool, seals the surface, fixes or narrows an argument), so
    // the diagnostic must NOT fire. Without them the semantic predicate could be an
    // unconditional warning.
    let cases: &[(&str, &str)] = &[
        ("tool drop", "[tools.search]\npresence = \"drop\"\n"),
        ("sealed default", "default-presence = \"drop\"\n"),
        ("arg pin", "[tools.search.args.artifact]\npin = \"hylla\"\n"),
        (
            "arg constrain",
            "[tools.search.args.artifact]\nenum = [\"hylla\", \"other\"]\n",
        ),
        (
            "one gating rule among ungated ones",
            "[tools.search]\nrename = \"s\"\n[tools.search.args.artifact]\npin = \"hylla\"\n",
        ),
    ];

    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    let home = std::fs::canonicalize(empty_cwd()).unwrap();

    for (label, body) in cases {
        std::fs::write(cwd.join("lagom.toml"), body).unwrap();
        let out = lagom()
            .current_dir(&cwd)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .args(["serve", "--", upstream_bin()])
            .write_stdin("")
            .output()
            .expect("run lagom serve");
        let stderr = String::from_utf8_lossy(&out.stderr);

        assert!(out.status.success(), "{label}: must serve");
        assert!(
            !stderr.contains("nothing is gated"),
            "{label}: this policy removes authority, so no ungated warning may fire: {stderr}"
        );
    }
}

#[test]
fn refire_with_ungated_record_warns_and_names_the_record() {
    // `refire` fronts agents exactly as `serve` does, so a record whose resolved
    // policy gates nothing needs the same diagnostic. Both an empty-passthrough
    // record and a structurally-non-default-but-non-restricting one are covered.
    //
    // The cwd deliberately holds a GATING `lagom.toml`: refire replays the recorded
    // policy verbatim (`SPEC.md` §8.2) and never reads that file, so the warning
    // must still fire — and must not tell the operator to go create a config.
    let upstream = serde_json::json!({ "command": upstream_bin(), "args": [], "env": [] });
    let cases: &[(&str, serde_json::Value)] = &[
        (
            "empty passthrough record",
            serde_json::json!({ "default_presence": "keep", "tools": {} }),
        ),
        (
            "non-restricting rules record",
            serde_json::json!({
                "default_presence": "keep",
                "tools": { "search": { "rename": "s", "description": {"override": "slim"} } }
            }),
        ),
    ];

    let dir = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        cwd.join("lagom.toml"),
        "[tools.search.args.artifact]\npin = \"hylla\"\n",
    )
    .unwrap();

    for (label, policy) in cases {
        let record_path = cwd.join("mint.json");
        let record = serde_json::json!({
            "run_id": "recorded-run",
            "sources": { "config_paths": [], "upstream": upstream, "dynamic_inputs": null },
            "resolved": { "policy": policy, "upstream": upstream }
        });
        std::fs::write(&record_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

        let out = lagom()
            .current_dir(&cwd)
            .args(["refire", "--record", record_path.to_str().unwrap()])
            .write_stdin("")
            .output()
            .expect("run lagom refire");
        let stderr = String::from_utf8_lossy(&out.stderr);

        assert!(out.status.success(), "{label}: refire must still serve");
        assert!(
            out.stdout.is_empty(),
            "{label}: stdout is the JSON-RPC channel: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains("nothing is gated"),
            "{label}: refire must carry the ungated diagnostic too: {stderr}"
        );
        assert!(
            stderr.contains(&record_path.display().to_string()),
            "{label}: the record that gated nothing must be named: {stderr}"
        );
        assert!(
            !stderr.contains("no lagom.toml discovered"),
            "{label}: refire never discovers config; that remedy would be wrong: {stderr}"
        );
    }
}

#[test]
fn refire_with_gating_record_stays_silent() {
    // The discriminating control for the refire surface: a recorded pin removes
    // authority, so no warning may fire.
    let upstream = serde_json::json!({ "command": upstream_bin(), "args": [], "env": [] });
    let dir = tempfile::tempdir().unwrap();
    let record_path = dir.path().join("mint.json");
    let record = serde_json::json!({
        "run_id": "recorded-run",
        "sources": { "config_paths": [], "upstream": upstream, "dynamic_inputs": null },
        "resolved": {
            "policy": {
                "default_presence": "keep",
                "tools": { "search": { "args": { "artifact": { "pin": "hylla" } } } }
            },
            "upstream": upstream
        }
    });
    std::fs::write(&record_path, serde_json::to_string_pretty(&record).unwrap()).unwrap();

    let out = lagom()
        .current_dir(empty_cwd())
        .args(["refire", "--record", record_path.to_str().unwrap()])
        .write_stdin("")
        .output()
        .expect("run lagom refire");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success());
    assert!(
        !stderr.contains("nothing is gated"),
        "a recorded pin gates the upstream: {stderr}"
    );
}

/// Mirror of `app.rs`'s `PREFLIGHT_STDIN_WAIT` (2s). Duplicated as a literal
/// because `lagom-cli` ships a `[[bin]]` and no lib target, so an integration test
/// cannot import the constant; `preflight_failure_waits_the_bounded_read_then_exits_silently`
/// is what couples them — raise one without the other and it goes red.
const PREFLIGHT_STDIN_WAIT: Duration = Duration::from_secs(2);

/// Mirror of `app.rs`'s `PREFLIGHT_ERROR_CODE`, same reason as above.
const PREFLIGHT_ERROR_CODE: i64 = -32002;

/// Outer bound for a pre-flight run: generous on purpose (7.5x the bounded wait) so
/// it cannot be the assertion that discriminates. The `>= PREFLIGHT_STDIN_WAIT`
/// lower bound is; this only catches a wait that never ends.
const PREFLIGHT_OUTER_BOUND: Duration = Duration::from_secs(15);

/// Run `lagom` on real pipes, optionally writing ONE line to its stdin, and return
/// its output plus wall-clock elapsed.
///
/// The stdin write end is taken out of the `Child` and held to the end:
/// `std::process::Child::wait`/`wait_with_output` close `self.stdin` first, which
/// would hand the CLI an immediate EOF and make a "bounded read" test pass without
/// any bound existing. `stderr` is piped so the original cause can be asserted.
fn preflight_run(args: &[&str], stdin_line: Option<&str>) -> (std::process::Output, Duration) {
    let dir = empty_cwd();
    let started = Instant::now();
    let mut child = StdCommand::new(env!("CARGO_BIN_EXE_lagom"))
        .current_dir(&dir)
        // Config discovery walks cwd, HOME and XDG_CONFIG_HOME; point all three at
        // the empty dir so a stray real `lagom.toml` cannot change which cause fires.
        .env("HOME", &dir)
        .env("XDG_CONFIG_HOME", &dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lagom");
    let mut held = child.stdin.take().expect("piped stdin");
    if let Some(line) = stdin_line {
        held.write_all(line.as_bytes()).unwrap();
        held.write_all(b"\n").unwrap();
        held.flush().unwrap();
    }
    let out = child.wait_with_output().expect("await lagom exit");
    let elapsed = started.elapsed();
    drop(held);
    (out, elapsed)
}

#[test]
fn preflight_failure_answers_initialize_with_one_jsonrpc_error() {
    // LGM-6: a pre-flight failure used to close stdout without a JSON-RPC byte, so
    // every distinct cause reached the harness as one opaque
    // `connection closed: initialize response`. Both serving surfaces must instead
    // answer the harness's own `initialize` id with the cause.
    //
    // Two surfaces, two id JSON types (number and string) so the echo is proven
    // verbatim rather than coerced.
    let record = "/no/such/mint.json";
    let cases: &[(&str, Vec<&str>, serde_json::Value, &str)] = &[
        (
            // Cause: upstream spawn (`ProxyError::Upstream`), inside warn_then_spawn.
            "serve, upstream spawn failure, numeric id",
            vec!["serve", "--", "/no/such/upstream-bin"],
            serde_json::json!(7),
            "upstream child",
        ),
        (
            // Cause: `read_mint_record` (`CliError::Record`), before any child exists.
            "refire, unreadable mint record, string id",
            vec!["refire", "--record", record],
            serde_json::json!("init-42"),
            "mint record",
        ),
    ];

    for (label, args, id, stderr_needle) in cases {
        let request =
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{}});
        let (out, _) = preflight_run(args, Some(&request.to_string()));

        assert!(
            !out.status.success(),
            "{label}: a pre-flight failure still exits non-zero"
        );
        let stdout = String::from_utf8(out.stdout.clone()).expect("stdout is UTF-8");
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            1,
            "{label}: exactly ONE frame may go on the wire, got {stdout:?}"
        );
        let reply: serde_json::Value =
            serde_json::from_str(lines[0]).expect("the reply is a JSON-RPC frame");
        assert_eq!(reply["jsonrpc"], serde_json::json!("2.0"), "{label}");
        assert_eq!(
            reply["id"], *id,
            "{label}: the harness's exact id must be echoed for correlation"
        );
        assert_eq!(
            reply["error"]["code"],
            serde_json::json!(PREFLIGHT_ERROR_CODE),
            "{label}: the pre-flight code distinguishes 'never started' from a reject"
        );
        assert!(
            reply.get("result").is_none(),
            "{label}: a failure may never look like a result: {reply}"
        );
        let message = reply["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(stderr_needle),
            "{label}: the reply must name the cause, got {message:?}"
        );
        // The reply is additive: stderr still explains the cause on its own.
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(stderr_needle),
            "{label}: stderr must stay intact, got {stderr:?}"
        );
    }

    // Scope control: `validate` is not a serving surface, so nothing correlates and
    // stdout must stay byte-empty even fed the same request. Without this the tests
    // above would pass on an unconditional reply from any subcommand.
    let request = serde_json::json!({"jsonrpc":"2.0","id":7,"method":"initialize","params":{}});
    let (out, _) = preflight_run(
        &["validate", "--", "/no/such/upstream-bin"],
        Some(&request.to_string()),
    );
    assert!(!out.status.success(), "validate still exits non-zero");
    assert!(
        out.stdout.is_empty(),
        "validate must not answer a JSON-RPC frame: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn preflight_failure_waits_the_bounded_read_then_exits_silently() {
    // THE falsifier for the bounded read. stdin is piped and held OPEN with nothing
    // written, so the CLI cannot see EOF: the only thing that can end its wait is
    // its own bound. "exits within a generous bound" would pass with no bound at all
    // (the pre-LGM-6 binary exited in milliseconds), so the discriminating assertion
    // is the LOWER one — elapsed >= PREFLIGHT_STDIN_WAIT.
    let (out, elapsed) = preflight_run(&["refire", "--record", "/no/such/mint.json"], None);

    assert!(
        elapsed >= PREFLIGHT_STDIN_WAIT,
        "the bounded stdin read must actually have run: elapsed {elapsed:?} < {PREFLIGHT_STDIN_WAIT:?}"
    );
    assert!(
        elapsed < PREFLIGHT_OUTER_BOUND,
        "the wait must be BOUNDED, not a hang: elapsed {elapsed:?}"
    );
    assert!(
        out.stdout.is_empty(),
        "with nothing to correlate to, stdout stays byte-empty: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!out.status.success(), "the cause still fails the run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("mint record"),
        "stderr still explains the cause: {stderr:?}"
    );
}

#[test]
fn preflight_failure_does_not_answer_a_notification() {
    // JSON-RPC 2.0 §4.1: a notification gets no reply. An absent `id` and an
    // explicit null `id` are both non-correlatable, so answering either would put a
    // stray frame on the harness's wire.
    let cases: &[(&str, serde_json::Value)] = &[
        (
            "no id at all",
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        ),
        (
            "explicit null id",
            serde_json::json!({"jsonrpc":"2.0","id":null,"method":"initialize"}),
        ),
    ];

    for (label, line) in cases {
        let (out, _) = preflight_run(
            &["refire", "--record", "/no/such/mint.json"],
            Some(&line.to_string()),
        );
        assert!(
            out.stdout.is_empty(),
            "{label}: no reply may be written, got {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(!out.status.success(), "{label}: the cause still fails");
    }
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
