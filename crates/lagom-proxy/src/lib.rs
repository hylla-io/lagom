//! # lagom-proxy
//!
//! The native, transport-bearing face: an out-of-process **stdio MCP proxy**
//! that spawns the upstream server as a child process and bridges JSON-RPC
//! between a harness and that child, applying the [`lagom_core`] transforms in
//! between (`SPEC.md` §7.1, §10).
//!
//! It also owns **minting** (`SPEC.md` §8): resolving a stack of policy sources
//! into a single [`ResolvedPolicy`], recording the [`MintRecord`] (provenance +
//! resolved policy) for refire, and re-minting an identical server from a record.
//!
//! Minting invokes no LLM and no nondeterministic input, so the same sources
//! produce a byte-identical resolved policy (`SPEC.md` §5.4) — the precondition
//! for [`refire`].

use std::path::PathBuf;

use lagom_core::Policy;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod bridge;
pub mod skills;

pub use bridge::{Server, serve, serve_audited};
pub use skills::SHIPPED_SKILLS;

/// Test-support seams: spawn-and-validate a [`Server`] without running it, so
/// integration tests can drive [`Server::run_with`] against in-memory streams.
///
/// Not part of the stable API — exposed only so the bridge round-trip tests can
/// inject a duplex harness in place of process stdio.
#[doc(hidden)]
pub mod test_support {
    use super::{ProxyError, ResolvedPolicy, Server};

    /// Spawn the upstream child and validate the policy against its live
    /// `tools/list`, returning the un-run [`Server`].
    pub async fn spawn_and_validate(resolved: ResolvedPolicy) -> Result<Server, ProxyError> {
        super::bridge::spawn_and_validate(resolved).await
    }
}

/// Anything that can go wrong minting or running the proxy.
#[derive(Debug, Error)]
pub enum ProxyError {
    /// Loading or composing a config source failed.
    #[error("config: {0}")]
    Config(#[from] lagom_config::ConfigError),
    /// Composing config layers or a dynamic overlay attempted to *widen* the
    /// sealed bounds (`SPEC.md` §5.2). Wraps the core [`lagom_core::MergeError`].
    #[error("overlay widens sealed bounds: {0}")]
    Merge(#[from] lagom_core::MergeError),
    /// A policy reference no longer matches the live upstream (`SPEC.md` §5.3).
    /// Carries the per-reference drift messages; serving is refused.
    #[error("policy drift vs upstream: {0:?}")]
    Drift(Vec<String>),
    /// Spawning the upstream child or bridging its stdio failed.
    #[error("upstream child: {source}")]
    Upstream {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Writing the mint record to the audit log failed.
    #[error("audit: {0}")]
    Audit(#[from] lagom_audit::AuditError),
}

/// How to launch the upstream MCP server as a child process (`SPEC.md` §10).
///
/// The config carries the command, args, and environment; lagom spawns it on
/// `initialize` and tears it down on exit.
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
/// These are captured verbatim so the mint is reproducible: the same
/// `PolicySources` resolve to a byte-identical [`ResolvedPolicy`] (`SPEC.md`
/// §5.4), the precondition for [`refire`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicySources {
    /// Config files composed in precedence order (base first, overlays after).
    /// The lowest-precedence path is the integrator base (the sealed ceiling);
    /// each later path narrows it via the narrow-only [`lagom_core::merge`]
    /// (`SPEC.md` §5.2, §6.3).
    #[serde(default)]
    pub config_paths: Vec<PathBuf>,
    /// How to launch the upstream this projection wraps (`SPEC.md` §10). Carried
    /// in the sources so refire reconstructs the same child process.
    pub upstream: UpstreamCommand,
    /// Dynamic mint-time scoping inputs (e.g. file paths a subagent may touch),
    /// captured verbatim so the mint is reproducible (`SPEC.md` §8.1, §5.4).
    ///
    /// Reserved for the builder face's dynamic scoping; the `lagom.toml` face
    /// resolves entirely from `config_paths`. Stored in the [`MintRecord`] for
    /// provenance regardless.
    #[serde(default)]
    pub dynamic_inputs: serde_json::Value,
}

/// A fully-resolved projection spec ready to serve — the deterministic output
/// of [`mint`] (`SPEC.md` §8.2). Validated against the upstream before use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPolicy {
    /// The composed, narrow-only-merged policy the engine consumes.
    pub policy: Policy,
    /// How to launch the upstream this policy projects.
    pub upstream: UpstreamCommand,
}

/// A recorded mint: the resolved policy plus the provenance that produced it.
///
/// Persisted (via [`lagom_audit`]) so a run can be **refired** — re-minted into
/// an identical server (`SPEC.md` §8.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MintRecord {
    /// The run this mint belongs to.
    pub run_id: String,
    /// The provenance that produced [`Self::resolved`].
    pub sources: PolicySources,
    /// The deterministic resolved policy.
    pub resolved: ResolvedPolicy,
}

/// Resolve a stack of policy sources into a single [`ResolvedPolicy`].
///
/// Loads each config layer in `config_paths` order and folds them with the
/// narrow-only [`lagom_core::merge`] (`SPEC.md` §5.2, §6.3): the first path is
/// the integrator base (sealed ceiling), each later path may only narrow it.
/// Empty `config_paths` resolves to a passthrough policy.
///
/// Deterministic: identical `sources` yield a byte-identical result (`SPEC.md`
/// §5.4) — no LLM, no clock, no environment read. This determinism is the
/// precondition for [`refire`].
pub fn mint(sources: &PolicySources) -> Result<ResolvedPolicy, ProxyError> {
    let mut policy = match sources.config_paths.split_first() {
        None => Policy::passthrough(),
        Some((base, overlays)) => {
            let mut acc = lagom_config::load(base)?;
            for overlay in overlays {
                let next = lagom_config::load(overlay)?;
                acc = lagom_core::merge(&acc, &next)?;
            }
            acc
        }
    };
    // Dynamic mint-time scoping (builder face) would narrow `policy` further
    // here; the `lagom.toml` face leaves `dynamic_inputs` empty. We still fold a
    // policy-shaped overlay if one is supplied, keeping mint a pure function of
    // its sources.
    if let Some(overlay) = parse_dynamic_overlay(&sources.dynamic_inputs)? {
        policy = lagom_core::merge(&policy, &overlay)?;
    }
    Ok(ResolvedPolicy {
        policy,
        upstream: sources.upstream.clone(),
    })
}

/// Interpret `dynamic_inputs` as an optional narrowing overlay policy.
///
/// `Null` (the default) means no dynamic scoping. Any other value must
/// deserialize into a [`Policy`]; a malformed value is a config error rather
/// than a silent no-op (`SPEC.md` §9.1 — never swallow).
fn parse_dynamic_overlay(inputs: &serde_json::Value) -> Result<Option<Policy>, ProxyError> {
    if inputs.is_null() {
        return Ok(None);
    }
    let policy: Policy = serde_json::from_value(inputs.clone()).map_err(|e| {
        ProxyError::Config(lagom_config::ConfigError::Lower {
            path: PathBuf::from("<dynamic_inputs>"),
            message: format!("dynamic mint inputs are not a valid policy overlay: {e}"),
        })
    })?;
    Ok(Some(policy))
}

/// Build a [`MintRecord`] from sources, recording its provenance for refire.
///
/// Resolves `sources` via [`mint`] and pairs the result with `run_id` and the
/// original `sources` so the run can be reproduced byte-for-byte (`SPEC.md`
/// §8.2).
pub fn mint_record(
    run_id: impl Into<String>,
    sources: &PolicySources,
) -> Result<MintRecord, ProxyError> {
    let resolved = mint(sources)?;
    Ok(MintRecord {
        run_id: run_id.into(),
        sources: sources.clone(),
        resolved,
    })
}

/// Re-mint the resolved policy from a previously recorded [`MintRecord`]
/// (refire, `SPEC.md` §8.2).
///
/// Bypasses re-resolution: the record already holds the deterministic resolved
/// policy, so refire reproduces the exact projection the run had before, even if
/// the on-disk config has since changed. Returns the [`ResolvedPolicy`] ready to
/// hand to [`serve`].
pub fn refire(record: &MintRecord) -> ResolvedPolicy {
    record.resolved.clone()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lagom_core::{ArgPolicy, Presence};
    use serde_json::json;

    use super::*;

    /// A throwaway temp dir cleaned on drop.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "lagom-proxy-mint-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn write(&self, name: &str, body: &str) -> PathBuf {
            let p = self.path.join(name);
            std::fs::write(&p, body).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn cmd() -> UpstreamCommand {
        UpstreamCommand {
            command: "echo".into(),
            args: vec!["hi".into()],
            env: vec![("K".into(), "V".into())],
        }
    }

    #[test]
    fn mint_empty_sources_is_passthrough() {
        let sources = PolicySources {
            config_paths: vec![],
            upstream: cmd(),
            dynamic_inputs: serde_json::Value::Null,
        };
        let resolved = mint(&sources).unwrap();
        assert_eq!(resolved.policy, Policy::passthrough());
        assert_eq!(resolved.upstream, cmd());
    }

    #[test]
    fn mint_folds_layers_narrow_only() {
        let tmp = TempDir::new("layers");
        let base = tmp.write("base.toml", "[tools.search]\npresence = \"keep\"\n");
        let overlay = tmp.write(
            "overlay.toml",
            "[tools.search.args.artifact]\npin = \"hylla\"\n",
        );
        let sources = PolicySources {
            config_paths: vec![base, overlay],
            upstream: cmd(),
            dynamic_inputs: serde_json::Value::Null,
        };
        let resolved = mint(&sources).unwrap();
        assert!(
            resolved.policy.tools["search"]
                .args
                .contains_key("artifact")
        );
    }

    #[test]
    fn mint_rejects_widening_overlay() {
        let tmp = TempDir::new("widen");
        let base = tmp.write("base.toml", "[tools.search]\npresence = \"drop\"\n");
        let overlay = tmp.write("overlay.toml", "[tools.search]\npresence = \"keep\"\n");
        let sources = PolicySources {
            config_paths: vec![base, overlay],
            upstream: cmd(),
            dynamic_inputs: serde_json::Value::Null,
        };
        let err = mint(&sources).unwrap_err();
        assert!(matches!(err, ProxyError::Merge(_)), "{err:?}");
    }

    #[test]
    fn mint_applies_dynamic_overlay() {
        // A builder-face dynamic input that drops a tool the base kept.
        let tmp = TempDir::new("dyn");
        let base = tmp.write("base.toml", "[tools.search]\npresence = \"keep\"\n");
        let dynamic = json!({
            "default_presence": "keep",
            "tools": { "search": { "presence": "drop" } }
        });
        let sources = PolicySources {
            config_paths: vec![base],
            upstream: cmd(),
            dynamic_inputs: dynamic,
        };
        let resolved = mint(&sources).unwrap();
        assert_eq!(
            resolved.policy.tools["search"].presence,
            Some(Presence::Drop)
        );
    }

    #[test]
    fn mint_malformed_dynamic_overlay_errors() {
        let sources = PolicySources {
            config_paths: vec![],
            upstream: cmd(),
            dynamic_inputs: json!("not a policy"),
        };
        let err = mint(&sources).unwrap_err();
        assert!(matches!(err, ProxyError::Config(_)), "{err:?}");
    }

    #[test]
    fn mint_is_deterministic() {
        let tmp = TempDir::new("det");
        let base = tmp.write("base.toml", "[tools.search.args.x]\npin = 1\n");
        let sources = PolicySources {
            config_paths: vec![base],
            upstream: cmd(),
            dynamic_inputs: serde_json::Value::Null,
        };
        let a = mint(&sources).unwrap();
        let b = mint(&sources).unwrap();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "identical sources must mint a byte-identical resolved policy"
        );
    }

    #[test]
    fn refire_is_byte_identical_to_record() {
        let mut tools = BTreeMap::new();
        let mut args = BTreeMap::new();
        args.insert("a".to_string(), ArgPolicy::Pin(json!("x")));
        tools.insert(
            "t".to_string(),
            lagom_core::ToolPolicy {
                args,
                ..Default::default()
            },
        );
        let resolved = ResolvedPolicy {
            policy: Policy {
                default_presence: Presence::Keep,
                tools,
            },
            upstream: cmd(),
        };
        let record = MintRecord {
            run_id: "run-7".into(),
            sources: PolicySources {
                config_paths: vec![],
                upstream: cmd(),
                dynamic_inputs: serde_json::Value::Null,
            },
            resolved: resolved.clone(),
        };
        let refired = refire(&record);
        assert_eq!(
            serde_json::to_string(&refired).unwrap(),
            serde_json::to_string(&resolved).unwrap(),
            "refire must reproduce the recorded resolved policy byte-for-byte"
        );
    }

    #[test]
    fn mint_record_captures_provenance() {
        let tmp = TempDir::new("rec");
        let base = tmp.write("base.toml", "default-presence = \"keep\"\n");
        let sources = PolicySources {
            config_paths: vec![base],
            upstream: cmd(),
            dynamic_inputs: serde_json::Value::Null,
        };
        let record = mint_record("run-1", &sources).unwrap();
        assert_eq!(record.run_id, "run-1");
        assert_eq!(record.sources, sources);
        assert_eq!(record.resolved, mint(&sources).unwrap());
    }
}
