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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
