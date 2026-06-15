//! Ephemeral mint records — the pure, serde-only data describing a minted
//! projection plus the provenance that produced it (`SPEC.md` §8.2).
//!
//! A **mint** resolves a stack of policy sources into one [`ResolvedPolicy`]; a
//! [`MintRecord`] pairs that resolved policy with its [`PolicySources`]
//! provenance so an *ephemeral projection* (minted for one agent + one run, then
//! discarded) can be **refired** — re-minted into a byte-identical server, even
//! after the on-disk config has changed.
//!
//! These types are deliberately transport-less and I/O-free so they compile to
//! wasm and are shared by every face: the native CLI / proxy (which also loads
//! config files), the wasm/Go binding, and the Python / Node bindings. The pure
//! operations here — [`mint`] (an in-code base + dynamic-overlay narrowing) and
//! [`refire`] — invoke no LLM, no clock, and read nothing from disk, so identical
//! inputs yield a byte-identical result (`SPEC.md` §5.4), the precondition for
//! refire.
//!
//! The *file-loading* mint (resolving [`PolicySources::config_paths`] off disk)
//! lives in the native `lagom-proxy` crate, which re-exports these types; it
//! cannot live here because reading files is not wasm-pure.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::merge::MergeError;
use crate::{Policy, merge};

/// How to launch the upstream MCP server as a child process (`SPEC.md` §10).
///
/// Carried in a mint's provenance so a refire reconstructs the same upstream
/// child. Pure data: the actual spawning lives in the native proxy face.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpstreamCommand {
    /// The executable to run.
    pub command: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to set for the child.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// The provenance of a mint: where the resolved policy came from (`SPEC.md`
/// §8.2). Layered config paths, the upstream launch command, plus any dynamic
/// mint-time inputs.
///
/// Captured verbatim so the mint is reproducible: the same `PolicySources`
/// resolve to a byte-identical [`ResolvedPolicy`] (`SPEC.md` §5.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicySources {
    /// Config files composed in precedence order (base first, overlays after).
    /// The lowest-precedence path is the integrator base (the sealed ceiling);
    /// each later path narrows it via the narrow-only [`merge`] (`SPEC.md` §5.2,
    /// §6.3). Resolved off disk by the native proxy face; empty for the in-code
    /// [`mint`] here.
    #[serde(default)]
    pub config_paths: Vec<String>,
    /// How to launch the upstream this projection wraps (`SPEC.md` §10). Carried
    /// in the sources so refire reconstructs the same child process.
    pub upstream: UpstreamCommand,
    /// Dynamic mint-time scoping inputs (e.g. file paths a subagent may touch),
    /// captured verbatim so the mint is reproducible (`SPEC.md` §8.1, §5.4).
    /// Stored in the [`MintRecord`] for provenance regardless of face.
    #[serde(default)]
    pub dynamic_inputs: Value,
}

/// A fully-resolved projection spec ready to serve — the deterministic output of
/// a mint (`SPEC.md` §8.2). Validated against the upstream before use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPolicy {
    /// The composed, narrow-only-merged policy the engine consumes.
    pub policy: Policy,
    /// How to launch the upstream this policy projects.
    pub upstream: UpstreamCommand,
}

/// A recorded mint: the resolved policy plus the provenance that produced it.
///
/// Persisted so a run can be **refired** — re-minted into an identical server
/// (`SPEC.md` §8.2). The whole record is plain serde data, so an app persists it
/// as JSON wherever it likes and hands it back to [`refire`] to reproduce the
/// exact projection a run had before.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MintRecord {
    /// The run this mint belongs to.
    pub run_id: String,
    /// The provenance that produced [`Self::resolved`].
    pub sources: PolicySources,
    /// The deterministic resolved policy.
    pub resolved: ResolvedPolicy,
}

/// Mint an ephemeral projection in code from an integrator `base` policy narrowed
/// by an optional `dynamic` overlay, recording its provenance (`SPEC.md` §8.1,
/// §8.2).
///
/// This is the **in-code** mint the bindings expose for an app (e.g. sand) that
/// builds policy as data rather than from `lagom.toml` files: `base` is the
/// integrator's sealed ceiling, `dynamic` is the per-agent narrowing computed at
/// spawn (e.g. `path` constrained to the exact files that agent may touch). The
/// overlay may only **narrow** the base; any widening is a [`MergeError`]
/// (`SPEC.md` §5.2 — this merge is the sandbox enforcement).
///
/// Deterministic: no LLM, no clock, no disk — identical inputs mint a
/// byte-identical [`MintRecord`] (`SPEC.md` §5.4), so the returned record can be
/// persisted and later handed to [`refire`] to reproduce the same server.
pub fn mint(
    run_id: impl Into<String>,
    base: &Policy,
    dynamic: Option<&Policy>,
    upstream: UpstreamCommand,
) -> Result<MintRecord, MergeError> {
    let policy = match dynamic {
        Some(overlay) => merge(base, overlay)?,
        None => base.clone(),
    };
    let dynamic_inputs = match dynamic {
        // Capture the overlay verbatim as provenance; `serde_json::to_value` on a
        // `Policy` cannot fail, but be explicit rather than unwrap.
        Some(overlay) => serde_json::to_value(overlay).unwrap_or(Value::Null),
        None => Value::Null,
    };
    let resolved = ResolvedPolicy {
        policy,
        upstream: upstream.clone(),
    };
    Ok(MintRecord {
        run_id: run_id.into(),
        sources: PolicySources {
            config_paths: Vec::new(),
            upstream,
            dynamic_inputs,
        },
        resolved,
    })
}

/// Re-mint the resolved policy from a previously recorded [`MintRecord`]
/// (refire, `SPEC.md` §8.2).
///
/// Bypasses re-resolution: the record already holds the deterministic resolved
/// policy, so refire reproduces the exact projection the run had before — even if
/// the on-disk config has since changed. Returns the [`ResolvedPolicy`] ready to
/// hand to the proxy's `serve`.
pub fn refire(record: &MintRecord) -> ResolvedPolicy {
    record.resolved.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Presence;
    use serde_json::json;

    fn cmd() -> UpstreamCommand {
        UpstreamCommand {
            command: "echo".into(),
            args: vec!["hi".into()],
            env: vec![("K".into(), "V".into())],
        }
    }

    /// A sealed base keeping `search`; a dynamic overlay that drops it (a legal
    /// narrowing) mints a record whose resolved policy has `search` dropped.
    #[test]
    fn mint_narrows_base_with_dynamic_overlay() {
        let base: Policy = serde_json::from_value(json!({
            "default_presence": "keep",
            "tools": { "search": { "presence": "keep" } }
        }))
        .unwrap();
        let dynamic: Policy = serde_json::from_value(json!({
            "default_presence": "keep",
            "tools": { "search": { "presence": "drop" } }
        }))
        .unwrap();

        let record = mint("run-1", &base, Some(&dynamic), cmd()).unwrap();
        assert_eq!(record.run_id, "run-1");
        assert_eq!(
            record.resolved.policy.tools["search"].presence,
            Some(Presence::Drop)
        );
        assert_eq!(record.resolved.upstream, cmd());
        // Provenance captured the dynamic overlay verbatim.
        assert!(!record.sources.dynamic_inputs.is_null());
        assert!(record.sources.config_paths.is_empty());
    }

    /// With no dynamic overlay the resolved policy is the base unchanged, and the
    /// recorded provenance carries a null `dynamic_inputs`.
    #[test]
    fn mint_without_overlay_is_base() {
        let base = Policy::passthrough();
        let record = mint("r", &base, None, cmd()).unwrap();
        assert_eq!(record.resolved.policy, Policy::passthrough());
        assert!(record.sources.dynamic_inputs.is_null());
    }

    /// A widening overlay (re-keep a dropped tool) is rejected — the sandbox
    /// enforcement, even on the in-code mint path (`SPEC.md` §5.2).
    #[test]
    fn mint_rejects_widening_overlay() {
        let base: Policy = serde_json::from_value(json!({
            "default_presence": "keep",
            "tools": { "search": { "presence": "drop" } }
        }))
        .unwrap();
        let overlay: Policy = serde_json::from_value(json!({
            "default_presence": "keep",
            "tools": { "search": { "presence": "keep" } }
        }))
        .unwrap();
        assert!(mint("r", &base, Some(&overlay), cmd()).is_err());
    }

    /// Minting is deterministic: identical inputs mint a byte-identical record.
    #[test]
    fn mint_is_deterministic() {
        let base: Policy = serde_json::from_value(json!({
            "tools": { "search": { "args": { "x": { "pin": 1 } } } }
        }))
        .unwrap();
        let a = mint("r", &base, None, cmd()).unwrap();
        let b = mint("r", &base, None, cmd()).unwrap();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    /// Refire reproduces the recorded resolved policy byte-for-byte.
    #[test]
    fn refire_is_byte_identical_to_record() {
        let base: Policy = serde_json::from_value(json!({
            "tools": { "t": { "args": { "a": { "pin": "x" } } } }
        }))
        .unwrap();
        let record = mint("run-7", &base, None, cmd()).unwrap();
        let refired = refire(&record);
        assert_eq!(
            serde_json::to_string(&refired).unwrap(),
            serde_json::to_string(&record.resolved).unwrap()
        );
    }

    /// A `MintRecord` round-trips through JSON losslessly — the persistence
    /// contract the CLI `refire --record` and every binding rely on.
    #[test]
    fn mint_record_json_round_trips() {
        let base = Policy::passthrough();
        let record = mint("run", &base, None, cmd()).unwrap();
        let json = serde_json::to_string(&record).unwrap();
        let back: MintRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(record, back);
    }
}
