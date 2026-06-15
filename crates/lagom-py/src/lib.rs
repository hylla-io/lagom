//! # lagom-py
//!
//! The Python binding face (`SPEC.md` §2, §7.2): the compiled [`lagom_core`]
//! engine shipped as a normal Python dependency via PyO3/maturin. Exposes the
//! transport-less core operations (`project`, `rewrite`, `merge`, `validate`) to
//! in-process Python agents, the spawned-process face (`mint_stdio_server`) for
//! agents that drive a child upstream over stdio, and a typed `PolicyBuilder` so
//! integrators author a [`lagom_core::Policy`] type-safely from Python
//! (`SPEC.md` §6.1). It also bundles the shipped lagom **skills** (`SPEC.md`
//! §12) — `shipped_skills()` / `emit_skills()` expose the identical embedded
//! markdown the CLI's `lagom emit-skills` writes, sourced once from
//! `lagom-proxy`.
//!
//! Built as an `abi3` `extension-module` cdylib, so it is **excluded from the
//! default `cargo build --all`** (which has no Python interpreter to link
//! against) and built via `just py` (maturin) instead — see the root
//! `Cargo.toml` and the `justfile`.
//!
//! ## Marshalling
//!
//! The four pure operations take and return **JSON strings**: the upstream tool
//! defs, the policy, and tool calls all cross the FFI boundary as JSON, keeping
//! the binding decoupled from `lagom_core`'s Rust types and trivially mirrored in
//! every other binding language. A failed parse, a [`lagom_core::Reject`], a
//! [`lagom_core::MergeError`], or a drift failure all surface as a Python
//! `ValueError` carrying the engine's own message (`SPEC.md` §9.1 — never
//! swallow).

use lagom_core::{
    ArgPolicy, Constraint, DescriptionPolicy, Policy, Presence, ToolCall, ToolDef, ToolPolicy,
    merge as core_merge, project as core_project, rewrite as core_rewrite,
    validate as core_validate,
};
use std::path::PathBuf;

use lagom_audit::AuditLog;
use lagom_proxy::{PolicySources, SHIPPED_SKILLS, UpstreamCommand, mint, serve, serve_audited};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::Value;

/// Map any `Display` error into a Python `ValueError` carrying its message.
fn value_err<E: std::fmt::Display>(e: E) -> PyErr {
    PyValueError::new_err(e.to_string())
}

/// Parse a JSON string into `T`, raising `ValueError` on malformed input.
fn parse_json<T: serde::de::DeserializeOwned>(label: &str, json: &str) -> PyResult<T> {
    serde_json::from_str(json).map_err(|e| PyValueError::new_err(format!("invalid {label}: {e}")))
}

/// Serialize `value` to a JSON string, raising `ValueError` on failure.
fn dump_json<T: serde::Serialize>(value: &T) -> PyResult<String> {
    serde_json::to_string(value).map_err(value_err)
}

/// Project an upstream tool surface through a policy (`SPEC.md` §3, §4).
///
/// `upstream_json` is the upstream `tools/list` array of tool defs as JSON;
/// `policy_json` is a [`lagom_core::Policy`] as JSON (e.g. from
/// `PolicyBuilder.build`). Returns the projected surface as a JSON array
/// string — tools dropped, pinned args pruned from schemas, constraints applied.
#[pyfunction]
fn project(upstream_json: &str, policy_json: &str) -> PyResult<String> {
    let upstream: Vec<ToolDef> = parse_json("upstream tool defs", upstream_json)?;
    let policy: Policy = parse_json("policy", policy_json)?;
    let projected = core_project(&upstream, &policy);
    dump_json(&projected)
}

/// Rewrite a projected call back into the upstream call, or raise on reject
/// (`SPEC.md` §4, §9).
///
/// `call_json` is a [`lagom_core::ToolCall`] as JSON; `policy_json` is the
/// policy. Returns the upstream call as JSON (pinned/default args injected); a
/// rejected call (unknown tool, violated constraint) raises `ValueError`
/// annotated with the violated bound.
#[pyfunction]
fn rewrite(call_json: &str, policy_json: &str) -> PyResult<String> {
    let call: ToolCall = parse_json("tool call", call_json)?;
    let policy: Policy = parse_json("policy", policy_json)?;
    let upstream = core_rewrite(&call, &policy).map_err(value_err)?;
    dump_json(&upstream)
}

/// Narrow-only merge of an end-user `overlay` onto an integrator `base`
/// (`SPEC.md` §5.2).
///
/// Both are policies as JSON. Returns the composed policy as JSON. Any attempt to
/// *widen* the sealed bounds (re-add a dropped tool, loosen a constraint, unpin)
/// raises `ValueError` — this merge is the sandbox enforcement.
#[pyfunction]
fn merge(base_json: &str, overlay_json: &str) -> PyResult<String> {
    let base: Policy = parse_json("base policy", base_json)?;
    let overlay: Policy = parse_json("overlay policy", overlay_json)?;
    let merged = core_merge(&base, &overlay).map_err(value_err)?;
    dump_json(&merged)
}

/// Validate a policy against the live upstream tool surface (`SPEC.md` §5.3).
///
/// `policy_json` is the policy; `upstream_json` is the upstream tool defs as
/// JSON. Returns `None` on success; on drift (a policy reference no longer
/// matches the upstream) raises `ValueError` carrying every per-reference drift
/// message — serving must be refused.
#[pyfunction]
fn validate(policy_json: &str, upstream_json: &str) -> PyResult<()> {
    let policy: Policy = parse_json("policy", policy_json)?;
    let upstream: Vec<ToolDef> = parse_json("upstream tool defs", upstream_json)?;
    core_validate(&policy, &upstream).map_err(|drift| {
        let messages: Vec<String> = drift.into_iter().map(|d| d.message).collect();
        PyValueError::new_err(format!("policy drift vs upstream: {}", messages.join("; ")))
    })
}

/// Mint a stdio proxy server for `policy` over a spawned upstream and serve it on
/// this process's stdio until the session ends (`SPEC.md` §7.2, §8, §10).
///
/// `policy_json` is the projection to enforce; `command`/`args`/`env` are how to
/// launch the upstream MCP server as a child process. lagom spawns the child,
/// validates the policy against its live `tools/list` (failing loud on drift),
/// then bridges JSON-RPC between the caller's stdio and the child, applying the
/// transforms in between. **Blocks** until the session ends — call it from the
/// process whose stdio should carry the projected surface.
///
/// With `audit` set to a path the session is routed through
/// [`lagom_proxy::serve_audited`], opening an append-only JSONL log that records
/// the original upstream defs + resolved policy at session start and every
/// rewrite/rejection thereafter, tagged with `run_id` (`SPEC.md` §8.2, §9.3) —
/// mirroring the CLI's `--audit`/`--run-id`. The log's parent directory must
/// already exist. Without `audit` no log is written and behavior is unchanged;
/// `run_id` defaults to `run` and is only meaningful with `audit`.
///
/// Drift, a widening overlay, a child-spawn failure, or an audit-log open
/// failure raise `ValueError`.
#[pyfunction]
#[pyo3(signature = (policy_json, command, args=None, env=None, audit=None, run_id="run"))]
fn mint_stdio_server(
    py: Python<'_>,
    policy_json: &str,
    command: &str,
    args: Option<Vec<String>>,
    env: Option<Vec<(String, String)>>,
    audit: Option<&str>,
    run_id: &str,
) -> PyResult<()> {
    let policy: Policy = parse_json("policy", policy_json)?;
    let upstream = UpstreamCommand {
        command: command.to_string(),
        args: args.unwrap_or_default(),
        env: env.unwrap_or_default(),
    };
    // `mint` resolves the (already-built) policy + upstream into a deterministic
    // ResolvedPolicy; we pass the policy through `dynamic_inputs` so the whole
    // projection is honoured without needing on-disk config files.
    let sources = PolicySources {
        config_paths: vec![],
        upstream,
        dynamic_inputs: serde_json::to_value(&policy).map_err(value_err)?,
    };
    let resolved = mint(&sources).map_err(value_err)?;

    // Open the audit log (if requested) on the calling thread so an open failure
    // surfaces as a ValueError before we release the GIL and start serving.
    let log = match audit {
        Some(path) => Some(AuditLog::open(path).map_err(value_err)?),
        None => None,
    };
    let run_id = run_id.to_string();

    // Serving spawns a child and does blocking stdio I/O, so release the GIL and
    // drive a single-threaded tokio runtime for the lifetime of the session.
    // With no audit log this is identical to `serve(resolved)` (which itself
    // delegates to `serve_audited(resolved, None, "run")`).
    py.allow_threads(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(value_err)?;
        match log {
            None => runtime.block_on(serve(resolved)).map_err(value_err),
            Some(log) => runtime
                .block_on(serve_audited(resolved, Some(log), run_id))
                .map_err(value_err),
        }
    })
}

/// The shipped lagom **skills** (`SPEC.md` §12) bundled in this binding, as a
/// list of `(filename, body)` tuples.
///
/// Identical bytes to the CLI's `lagom emit-skills`: both faces embed the same
/// canonical markdown from `lagom-proxy`. A host loads these inert documents to
/// learn how to use lagom well; lagom never loads or runs them itself.
#[pyfunction]
fn shipped_skills() -> Vec<(String, String)> {
    SHIPPED_SKILLS
        .iter()
        .map(|(name, body)| ((*name).to_string(), (*body).to_string()))
        .collect()
}

/// Write the shipped skill markdown into `dir` (created if absent), mirroring the
/// CLI's `lagom emit-skills` (`SPEC.md` §12).
///
/// Returns the list of written file paths (as strings). Like `lagom emit`, lagom
/// does not manage how a harness discovers the files — it just drops them where
/// the consumer asks. I/O failures raise `ValueError`.
#[pyfunction]
#[pyo3(signature = (dir="."))]
fn emit_skills(dir: &str) -> PyResult<Vec<String>> {
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).map_err(value_err)?;
    let mut written = Vec::with_capacity(SHIPPED_SKILLS.len());
    for (filename, body) in SHIPPED_SKILLS {
        let path = dir.join(filename);
        std::fs::write(&path, body).map_err(value_err)?;
        written.push(path.to_string_lossy().into_owned());
    }
    Ok(written)
}

/// A typed, fluent builder for a [`lagom_core::Policy`] from Python
/// (`SPEC.md` §6.1).
///
/// The integrator's type-safe authoring front-end: methods narrow the projection
/// (drop tools, pin/constrain/default args, override descriptions); `build`
/// emits the policy as JSON ready to hand to `project`, `rewrite`, `merge`,
/// `validate`, or `mint_stdio_server`. Every method returns nothing and
/// mutates in place, so calls chain naturally in Python.
#[pyclass]
#[derive(Clone)]
struct PolicyBuilder {
    policy: Policy,
}

impl PolicyBuilder {
    /// Borrow (creating if absent) the per-tool policy for `tool`. Internal
    /// helper — not exposed to Python (returns a non-`pyclass` reference).
    fn tool_mut(&mut self, tool: &str) -> &mut ToolPolicy {
        self.policy.tools.entry(tool.to_string()).or_default()
    }
}

#[pymethods]
impl PolicyBuilder {
    /// A passthrough builder: keeps every tool unchanged unless narrowed
    /// (`default_presence = keep`).
    #[new]
    fn new() -> Self {
        Self {
            policy: Policy::passthrough(),
        }
    }

    /// A sealed builder: drops every tool unless explicitly [`keep`](Self::keep)
    /// is called (`default_presence = drop`). The basis of an allowlist sandbox.
    #[staticmethod]
    fn sealed() -> Self {
        Self {
            policy: Policy::sealed(),
        }
    }

    /// Explicitly keep `tool` (overrides a sealed default presence).
    fn keep(&mut self, tool: &str) {
        self.tool_mut(tool).presence = Some(Presence::Keep);
    }

    /// Drop `tool` from the projected surface (`SPEC.md` §4.1).
    fn drop_tool(&mut self, tool: &str) {
        self.tool_mut(tool).presence = Some(Presence::Drop);
    }

    /// Expose `tool` under a different downstream `name`.
    fn rename(&mut self, tool: &str, name: &str) {
        self.tool_mut(tool).rename = Some(name.to_string());
    }

    /// Replace `tool`'s description with integrator-authored slim text
    /// (Tier 1 override, `SPEC.md` §4.2).
    fn describe(&mut self, tool: &str, text: &str) {
        self.tool_mut(tool).description = DescriptionPolicy::Override(text.to_string());
    }

    /// Pin `tool`'s `arg` to `value` (given as a JSON string): removed from the
    /// projected schema, injected on every call (`SPEC.md` §3, §4.1).
    fn pin(&mut self, tool: &str, arg: &str, value_json: &str) -> PyResult<()> {
        let value: Value = parse_json("pin value", value_json)?;
        self.tool_mut(tool)
            .args
            .insert(arg.to_string(), ArgPolicy::Pin(value));
        Ok(())
    }

    /// Supply `tool`'s `arg` with `value` (JSON string) when the agent omits it;
    /// stays visible, unlike [`pin`](Self::pin).
    fn default(&mut self, tool: &str, arg: &str, value_json: &str) -> PyResult<()> {
        let value: Value = parse_json("default value", value_json)?;
        self.tool_mut(tool)
            .args
            .insert(arg.to_string(), ArgPolicy::Default(value));
        Ok(())
    }

    /// Constrain `tool`'s `arg` to an enum subset (`values_json` is a JSON array).
    fn constrain_enum(&mut self, tool: &str, arg: &str, values_json: &str) -> PyResult<()> {
        let values: Vec<Value> = parse_json("enum values", values_json)?;
        self.tool_mut(tool).args.insert(
            arg.to_string(),
            ArgPolicy::Constrain(Constraint::Enum(values)),
        );
        Ok(())
    }

    /// Constrain `tool`'s numeric `arg` to an inclusive `[min, max]` range; either
    /// bound may be `None` (open).
    #[pyo3(signature = (tool, arg, min=None, max=None))]
    fn constrain_range(&mut self, tool: &str, arg: &str, min: Option<f64>, max: Option<f64>) {
        self.tool_mut(tool).args.insert(
            arg.to_string(),
            ArgPolicy::Constrain(Constraint::Range { min, max }),
        );
    }

    /// Constrain `tool`'s string `arg` to match a regex `pattern`.
    fn constrain_pattern(&mut self, tool: &str, arg: &str, pattern: &str) {
        self.tool_mut(tool).args.insert(
            arg.to_string(),
            ArgPolicy::Constrain(Constraint::Pattern(pattern.to_string())),
        );
    }

    /// Emit the authored policy as a JSON string, ready for the engine functions.
    fn build(&self) -> PyResult<String> {
        dump_json(&self.policy)
    }
}

/// The `lagom` Python extension module (imported as `lagom`).
#[pymodule]
fn lagom(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(project, m)?)?;
    m.add_function(wrap_pyfunction!(rewrite, m)?)?;
    m.add_function(wrap_pyfunction!(merge, m)?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    m.add_function(wrap_pyfunction!(mint_stdio_server, m)?)?;
    m.add_function(wrap_pyfunction!(shipped_skills, m)?)?;
    m.add_function(wrap_pyfunction!(emit_skills, m)?)?;
    m.add_class::<PolicyBuilder>()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upstream_json() -> String {
        json!([
            {
                "name": "search",
                "description": "Search.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "artifact": {"type": "string"},
                        "query": {"type": "string"}
                    },
                    "required": ["artifact", "query"]
                }
            },
            {
                "name": "write_file",
                "description": "Write.",
                "input_schema": {"type": "object", "properties": {}}
            }
        ])
        .to_string()
    }

    /// A sealed builder that keeps `search` and pins its `artifact` arg drops the
    /// other tool and prunes the pinned property from the projected schema.
    #[test]
    fn project_drops_tool_and_pins_arg() {
        let mut b = PolicyBuilder::sealed();
        b.keep("search");
        b.pin("search", "artifact", "\"hylla\"").unwrap();
        let policy = b.build().unwrap();

        let projected = project(&upstream_json(), &policy).unwrap();
        let defs: Vec<ToolDef> = serde_json::from_str(&projected).unwrap();

        assert_eq!(defs.len(), 1, "write_file must be dropped");
        assert_eq!(defs[0].name, "search");
        let props = &defs[0].input_schema["properties"];
        assert!(props.get("artifact").is_none(), "pinned arg must be pruned");
        assert!(props.get("query").is_some());
    }

    /// Rewrite injects the pinned value and a constraint violation raises.
    #[test]
    fn rewrite_injects_pin_and_rejects_constraint() {
        let mut b = PolicyBuilder::new();
        b.pin("search", "artifact", "\"hylla\"").unwrap();
        b.constrain_enum("search", "query", "[\"a\", \"b\"]")
            .unwrap();
        let policy = b.build().unwrap();

        let ok = rewrite(
            &json!({"name": "search", "arguments": {"query": "a"}}).to_string(),
            &policy,
        )
        .unwrap();
        let call: ToolCall = serde_json::from_str(&ok).unwrap();
        assert_eq!(call.arguments["artifact"], json!("hylla"));

        let err = rewrite(
            &json!({"name": "search", "arguments": {"query": "z"}}).to_string(),
            &policy,
        );
        assert!(err.is_err(), "out-of-enum query must reject");
    }

    /// A widening overlay is rejected by the narrow-only merge.
    #[test]
    fn merge_rejects_widening() {
        let base = PolicyBuilder::sealed().build().unwrap();
        let mut over = PolicyBuilder::new();
        over.keep("search");
        let overlay = over.build().unwrap();
        // base drops all; overlay re-adds -> widening -> error.
        assert!(merge(&base, &overlay).is_err());
    }

    /// Validate fails loud when the policy references a vanished tool.
    #[test]
    fn validate_flags_drift() {
        let mut b = PolicyBuilder::new();
        b.pin("ghost", "x", "1").unwrap();
        let policy = b.build().unwrap();
        assert!(validate(&policy, &upstream_json()).is_err());
    }

    /// Malformed JSON surfaces as a Python ValueError, never a panic.
    #[test]
    fn malformed_json_is_value_error() {
        assert!(project("not json", "{}").is_err());
    }

    /// The binding bundles both shipped skills with their `.skill.md` names.
    #[test]
    fn shipped_skills_bundles_both() {
        let skills = shipped_skills();
        assert_eq!(skills.len(), 2);
        let names: Vec<&str> = skills.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"lagom-slim-docs.skill.md"));
        assert!(names.contains(&"lagom-dynamic-mint.skill.md"));
        assert!(skills.iter().all(|(_, body)| !body.trim().is_empty()));
    }

    /// `mint_stdio_server`'s optional audit path fails loud (ValueError, no
    /// panic) when the audit log cannot be opened — here a path whose parent
    /// directory does not exist — and does so *before* any upstream is spawned.
    /// This exercises the new audit wiring without needing a live upstream.
    #[test]
    fn mint_stdio_server_audit_open_failure_is_value_error() {
        let policy = PolicyBuilder::new().build().unwrap();
        let bad_audit = std::env::temp_dir()
            .join(format!("lagom-py-no-such-dir-{}", std::process::id()))
            .join("audit.jsonl");
        assert!(
            !bad_audit.parent().unwrap().exists(),
            "test precondition: parent dir must be absent"
        );

        pyo3::prepare_freethreaded_python();
        Python::with_gil(|py| {
            let err = mint_stdio_server(
                py,
                &policy,
                "this-upstream-is-never-spawned",
                None,
                None,
                Some(bad_audit.to_str().unwrap()),
                "run",
            );
            assert!(
                err.is_err(),
                "opening an audit log under a missing dir must raise ValueError"
            );
        });
    }

    /// `emit_skills` writes both files with their expected headings.
    #[test]
    fn emit_skills_writes_both_with_headings() {
        let dir = std::env::temp_dir().join(format!("lagom-py-skills-{}", std::process::id()));
        let written = emit_skills(dir.to_str().unwrap()).unwrap();
        assert_eq!(written.len(), 2);

        let slim = std::fs::read_to_string(dir.join("lagom-slim-docs.skill.md")).unwrap();
        let mint = std::fs::read_to_string(dir.join("lagom-dynamic-mint.skill.md")).unwrap();
        assert!(slim.starts_with("# lagom-slim-docs"));
        assert!(mint.starts_with("# lagom-dynamic-mint"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
