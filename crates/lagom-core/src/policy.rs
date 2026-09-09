//! The `Policy` model — lagom's canonical projection spec as data.
//!
//! Both authoring front-ends (the typed builder and `lagom.toml`) produce a
//! [`Policy`]; the engine ([`crate::project`], [`crate::rewrite`]) consumes only
//! this. See `SPEC.md` §3.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Whether a tool survives projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// The tool is exposed to the consumer.
    Keep,
    /// The tool is removed from the projected surface.
    Drop,
}

impl Presence {
    /// `Drop` is strictly narrower than `Keep`. Used by the narrow-only merge.
    pub(crate) fn is_narrower_or_equal(self, base: Presence) -> bool {
        matches!(
            (base, self),
            (Presence::Keep, _) | (Presence::Drop, Presence::Drop)
        )
    }
}

/// How a tool's description is rendered downstream.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DescriptionPolicy {
    /// Keep the upstream description; lagom appends a deterministic addendum
    /// stating any restrictions it applied (see `SPEC.md` §4.2 Tier 2).
    #[default]
    Passthrough,
    /// Replace the description with integrator-authored slim text (Tier 1). No
    /// addendum is appended — the override is taken as the final text.
    Override(String),
}

/// A bound on an argument's value domain. The agent still sets the argument, but
/// only within these bounds (`SPEC.md` §4.1 "constrain").
///
/// Deserialization rejects a key inside the `range` variant that is not `min` or
/// `max`, instead of ignoring it (`deny_unknown_fields`; locked by
/// `constraint_range_typoed_maximum_is_rejected`). WHY: `{"range":{"min":1,
/// "maximum":5}}` used to deserialize as `Ok` into `Range { min: Some(1.0), max:
/// None }` — a *partial* parse. The caller got a well-formed `Constrain`
/// enforcing half the author's bound, which reads as correct under audit.
///
/// Scope: this guards the keys of the `range` variant's struct body. An unknown
/// *variant tag* (`{"rango":…}`) is refused by serde's own external tagging,
/// independent of this attribute — locked separately by
/// `constraint_unknown_variant_tag_is_rejected`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Constraint {
    /// The value must be one of these (a subset of the upstream domain).
    Enum(Vec<Value>),
    /// A numeric range; either bound may be open.
    Range {
        /// Inclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        /// Inclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
    },
    /// The value (a string) must match this regular expression.
    Pattern(String),
}

/// What lagom does to a single argument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgPolicy {
    /// Fix the argument to a value: removed from the projected schema, injected
    /// on every call. The keystone transform (`SPEC.md` §3, §4.1).
    Pin(Value),
    /// Narrow the argument's domain; it stays visible and agent-settable.
    Constrain(Constraint),
    /// Supply a value when the agent omits it; stays visible (unlike `Pin`).
    Default(Value),
    /// Leave the argument unchanged.
    Passthrough,
}

/// The rules lagom applies to one tool.
///
/// Deserialization rejects a key outside this struct instead of ignoring it
/// (`deny_unknown_fields`; locked by `tool_policy_typoed_args_key_is_rejected`
/// and `tool_policy_typoed_presence_key_is_rejected`). WHY: a misspelled `args`
/// used to deserialize as `Ok` with `args: {}`, so a pin the author wrote
/// vanished and the agent kept setting that argument itself; a misspelled
/// `presence` fell through to `default_presence` the same way.
///
/// Scope: this guards the `ToolPolicy` shape. `ToolDef`/`ToolCall` (`tooldef.rs`)
/// stay lenient by design — real MCP `tools/list` entries carry extra keys such
/// as `annotations`, `title`, and `outputSchema`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPolicy {
    /// Override the policy-level default presence for this tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<Presence>,
    /// Expose the tool under a different name downstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename: Option<String>,
    /// How the description is rendered.
    #[serde(default)]
    pub description: DescriptionPolicy,
    /// Per-argument transforms, keyed by upstream argument name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, ArgPolicy>,
}

/// A complete projection spec over an upstream server.
///
/// Tools absent from `tools` follow `default_presence`. The default
/// `default_presence` is `Keep` — lagom is *passthrough* unless told otherwise
/// (`SPEC.md` §3); a sealed sandbox sets it to `Drop` and allowlists tools.
///
/// Deserialization rejects a key outside this struct instead of ignoring it
/// (`deny_unknown_fields`; locked by `default_presence_kebab_key_is_rejected`
/// and `unknown_policy_key_is_rejected`). WHY: `default-presence` is the
/// canonical spelling on the `lagom.toml` face (`lagom-config`'s `TomlPolicy`),
/// so pasting it into a mint payload used to yield an empty policy that
/// restored `Keep` — a seal downgraded to passthrough with an `Ok`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Presence for tools with no explicit rule.
    #[serde(default = "default_presence")]
    pub default_presence: Presence,
    /// Per-tool rules, keyed by upstream tool name.
    #[serde(default)]
    pub tools: BTreeMap<String, ToolPolicy>,
}

fn default_presence() -> Presence {
    Presence::Keep
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            default_presence: Presence::Keep,
            tools: BTreeMap::new(),
        }
    }
}

impl Policy {
    /// An empty passthrough policy (keeps everything unchanged).
    pub fn passthrough() -> Self {
        Self::default()
    }

    /// A sealed policy that drops every tool unless explicitly kept.
    pub fn sealed() -> Self {
        Self {
            default_presence: Presence::Drop,
            tools: BTreeMap::new(),
        }
    }

    /// Resolve the effective presence for a tool name.
    pub(crate) fn presence_of(&self, tool: &str) -> Presence {
        self.tools
            .get(tool)
            .and_then(|t| t.presence)
            .unwrap_or(self.default_presence)
    }

    /// Every name this policy carries an explicit rule for, in both spellings:
    /// the upstream key and, when set, the projected `rename`.
    ///
    /// This is the *governed* name set — the names whose authority (presence,
    /// pins, constraints) lagom decides. Names outside it fall to
    /// `default_presence`.
    fn governed_names(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().flat_map(|(upstream, tp)| {
            std::iter::once(upstream.as_str()).chain(tp.rename.as_deref())
        })
    }

    /// The governed name that `name` differs from only by Unicode case, if any.
    ///
    /// Used by [`crate::rewrite`] to REFUSE such a name: forwarding e.g. `SEARCH`
    /// when the policy governs `search` would hand the decision to the upstream's
    /// name matching, and a lenient upstream would then run the governed tool
    /// without the policy's presence/pin/constraint rules. Verified hole, not
    /// hypothetical: a reviewer observed a dropped tool's upstream receiving
    /// `"name": "SEARCH"`.
    ///
    /// WHY case folding appears here and NOWHERE in the resolve path: folding to
    /// *deny* only ever narrows the set of accepted names; folding to *match*
    /// would widen it, minting extra spellings for every governed tool. Do not
    /// "improve" this into lookup — lagom resolves names by exact equality and
    /// refuses everything else.
    ///
    /// Only meaningful for a `name` with no exact rule; an exactly governed name
    /// resolves on its own rule and never consults this.
    ///
    /// Residual (case only, by construction): folding catches case variants, not
    /// every spelling an upstream might resolve leniently — e.g. interior
    /// zero-width or bidi characters, NFKC-equivalent or full-width forms, or
    /// `-`/`_` swaps survive it. Those are refused only when the policy's default
    /// presence is `Drop`, or once the authoritative upstream tool surface is
    /// threaded into the rewrite decision.
    pub(crate) fn case_shadowed_governed_name(&self, name: &str) -> Option<&str> {
        let folded = name.to_lowercase();
        self.governed_names()
            .find(|governed| governed.to_lowercase() == folded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported defect: the `lagom.toml` spelling pasted into a mint payload
    /// deserialized as an empty `Policy`, silently restoring `Keep`.
    #[test]
    fn default_presence_kebab_key_is_rejected() {
        let err = serde_json::from_str::<Policy>(r#"{"default-presence":"drop"}"#)
            .expect_err("kebab key must not deserialize");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn unknown_policy_key_is_rejected() {
        let err = serde_json::from_str::<Policy>(r#"{"toolz":{}}"#)
            .expect_err("unknown key must not deserialize");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    /// The wire spelling shared by every binding (`parity/fixtures/cases.json`).
    #[test]
    fn snake_case_default_presence_still_parses() {
        let policy: Policy = serde_json::from_str(r#"{"default_presence":"drop"}"#).unwrap();
        assert_eq!(policy.default_presence, Presence::Drop);
    }

    /// `SPEC.md` §3 passthrough default survives the guard.
    #[test]
    fn empty_document_is_still_passthrough() {
        let policy: Policy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy.default_presence, Presence::Keep);
    }

    /// The pin-vanishing defect: singular `arg` used to parse as `Ok` with an
    /// empty `args`, so the caller's pin was gone and the agent still owned the
    /// argument.
    #[test]
    fn tool_policy_typoed_args_key_is_rejected() {
        let json = r#"{"tools":{"search":{"arg":{"artifact":{"pin":"x"}}}}}"#;
        let err = match serde_json::from_str::<Policy>(json) {
            Ok(policy) => panic!("typo'd `arg` must not deserialize; got {:?}", policy.tools),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    /// A misspelled `presence` used to fall through to `default_presence`.
    #[test]
    fn tool_policy_typoed_presence_key_is_rejected() {
        let json = r#"{"tools":{"search":{"presense":"drop"}}}"#;
        let err = match serde_json::from_str::<Policy>(json) {
            Ok(policy) => panic!(
                "typo'd `presense` must not deserialize; got {:?}",
                policy.tools
            ),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    /// The guard must leave the legitimate surface intact: all four fields
    /// together, in the wire spelling of `parity/fixtures/cases.json`.
    #[test]
    fn tool_policy_full_legitimate_surface_still_parses() {
        let json = r#"{"tools":{"search":{"presence":"keep","rename":"find","description":{"override":"App find."},"args":{"artifact":{"pin":"hylla"}}}}}"#;
        let policy: Policy = serde_json::from_str(json).unwrap();
        let tp = &policy.tools["search"];
        assert_eq!(tp.presence, Some(Presence::Keep));
        assert_eq!(tp.rename.as_deref(), Some("find"));
        assert_eq!(
            tp.description,
            DescriptionPolicy::Override("App find.".to_string())
        );
        assert_eq!(tp.args["artifact"], ArgPolicy::Pin(Value::from("hylla")));
    }

    /// The partial-parse defect: `maximum` used to be ignored, yielding
    /// `Range { min: Some(1.0), max: None }` — an `Ok` enforcing half the bound.
    #[test]
    fn constraint_range_typoed_maximum_is_rejected() {
        let json = r#"{"constrain":{"range":{"min":1,"maximum":5}}}"#;
        let err = match serde_json::from_str::<ArgPolicy>(json) {
            Ok(policy) => panic!("typo'd `maximum` must not deserialize; got {policy:?}"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    /// Serde's external tagging already refuses an unknown variant tag; assert it
    /// so a later refactor cannot drop that behavior unnoticed.
    #[test]
    fn constraint_unknown_variant_tag_is_rejected() {
        let json = r#"{"constrain":{"rango":{"min":1}}}"#;
        let err = match serde_json::from_str::<ArgPolicy>(json) {
            Ok(policy) => panic!("unknown variant tag must not deserialize; got {policy:?}"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unknown variant"), "{err}");
    }

    /// The guard must leave the legitimate two-bound range intact.
    #[test]
    fn constraint_range_with_both_bounds_still_parses() {
        let json = r#"{"constrain":{"range":{"min":1,"max":5}}}"#;
        let policy: ArgPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(
            policy,
            ArgPolicy::Constrain(Constraint::Range {
                min: Some(1.0),
                max: Some(5.0),
            })
        );
    }
}
