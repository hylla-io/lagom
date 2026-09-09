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

// Every public item must be documented — the gate's `clippy -D warnings` turns
// this into an error, upholding the NO-DRIFT docs-always-full invariant.
#![warn(missing_docs)]

use std::path::PathBuf;

use lagom_core::Policy;
use thiserror::Error;

mod bridge;
pub mod skills;

pub use bridge::{Server, Teardown, serve, serve_audited, spawn_and_validate, validate_only};
pub use skills::SHIPPED_SKILLS;

// The mint-record data types and the pure `refire` are owned by `lagom-core`
// (transport-less, wasm-safe) and re-exported here so every native caller — the
// CLI, the Python/Node bindings — keeps the historical `lagom_proxy::{…}` paths.
// Only the *file-loading* mint (resolving `config_paths` off disk) lives in this
// crate; the in-code `lagom_core::mint` and `lagom_core::refire` are pure.
pub use lagom_core::{MintRecord, PolicySources, ResolvedPolicy, UpstreamCommand, refire};

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

        /// Write a config file and return its path as a `String` (the
        /// [`PolicySources::config_paths`] element type).
        fn write(&self, name: &str, body: &str) -> String {
            let p = self.path.join(name);
            std::fs::write(&p, body).unwrap();
            p.to_string_lossy().into_owned()
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

    /// A typo'd policy key in `dynamic_inputs` must fail the mint rather than
    /// yield a silently non-narrowing overlay (`SPEC.md` §8.1 per-agent scoping,
    /// §9.1 never swallow). Covers the two classes that deserialized as `Ok`
    /// before `Policy`/`ToolPolicy` gained `deny_unknown_fields`: the kebab
    /// `default-presence` (canonical on the `lagom.toml` face, so a copy-paste
    /// restored `Keep`) and a typo'd `args` (the pin vanished). Asserts the error
    /// CLASSIFICATION and the `<dynamic_inputs>` source, not message text —
    /// per-face framing is a documented difference (`parity/README.md:18-22`).
    #[test]
    fn mint_typoed_dynamic_overlay_key_is_rejected_not_silently_widened() {
        for (case, dynamic) in [
            (
                "kebab default-presence",
                json!({"default-presence": "drop"}),
            ),
            (
                "typoed args",
                json!({"tools": {"search": {"arg": {"artifact": {"pin": "hylla"}}}}}),
            ),
        ] {
            let sources = PolicySources {
                config_paths: vec![],
                upstream: cmd(),
                dynamic_inputs: dynamic,
            };
            let err = mint(&sources).unwrap_err();
            let ProxyError::Config(lagom_config::ConfigError::Lower { path, .. }) = &err else {
                panic!("{case}: expected a typed config error, got {err:?}");
            };
            assert_eq!(path, &PathBuf::from("<dynamic_inputs>"), "{case}");
        }
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
