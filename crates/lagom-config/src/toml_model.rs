//! The on-disk `lagom.toml` representation (`SPEC.md` §6.2).
//!
//! This is the *flat human surface* — which tools, which args pinned or
//! constrained, description overrides — deserialized from TOML and then lowered
//! into the canonical [`lagom_core::Policy`]. It deliberately does not carry the
//! rich transforms that live in the typed builder.
//!
//! Kept structurally separate from [`lagom_core::policy`] so the wire/disk
//! format can evolve (key names are an open question, `SPEC.md` §13) without
//! perturbing the engine's canonical model.

use std::collections::BTreeMap;

use lagom_core::{ArgPolicy, Constraint, DescriptionPolicy, Policy, Presence, ToolPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A path-free lowering failure (`TomlPolicy` → core [`Policy`]).
///
/// The caller ([`super::load`]) attaches the originating path to produce a
/// [`super::ConfigError::Lower`]; the lowering itself does not know where the
/// document came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerError(pub String);

impl LowerError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LowerError {}

/// Top-level `lagom.toml` document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TomlPolicy {
    /// Default presence for tools with no explicit rule. `"keep"` (passthrough,
    /// the default) or `"drop"` (sealed allowlist). Mirrors
    /// [`lagom_core::Presence`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_presence: Option<String>,
    /// Per-tool rules keyed by upstream tool name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, TomlToolPolicy>,
}

/// The flat per-tool surface in `lagom.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TomlToolPolicy {
    /// `"keep"` or `"drop"`; absent means inherit the document default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<String>,
    /// Expose the tool under a different downstream name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename: Option<String>,
    /// Tier-1 slim description override (`SPEC.md` §4.2). Absent means
    /// passthrough + addendum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Per-argument transforms keyed by upstream argument name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, TomlArgPolicy>,
}

/// The flat per-argument surface: pin a value, constrain a domain, or supply a
/// default. Mutually exclusive — exactly one should be set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct TomlArgPolicy {
    /// Pin the argument to this value: removed from the schema, injected on
    /// every call (`SPEC.md` §3, §4.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<Value>,
    /// Supply this value when the agent omits the argument; stays visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Constrain the argument to an enum subset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<Value>>,
    /// Constrain a numeric argument's inclusive lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Constrain a numeric argument's inclusive upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Constrain a string argument to this regular expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

impl TomlPolicy {
    /// Lower the flat TOML surface into the canonical core [`Policy`].
    ///
    /// Resolves the `"keep"`/`"drop"` strings to [`lagom_core::Presence`] and
    /// the mutually-exclusive arg fields to [`lagom_core::ArgPolicy`]. Returns a
    /// [`LowerError`] if a field is malformed (e.g. an unknown presence string,
    /// or more than one arg transform set).
    pub fn into_policy(self) -> Result<Policy, LowerError> {
        let default_presence = match self.default_presence.as_deref() {
            None => Presence::Keep,
            Some(s) => parse_presence(s)?,
        };

        let mut tools = BTreeMap::new();
        for (name, tt) in self.tools {
            tools.insert(name.clone(), tt.into_tool_policy(&name)?);
        }

        Ok(Policy {
            default_presence,
            tools,
        })
    }
}

impl TomlToolPolicy {
    /// Lower one tool's flat surface into a core [`ToolPolicy`]. `tool` names the
    /// tool for error context.
    fn into_tool_policy(self, tool: &str) -> Result<ToolPolicy, LowerError> {
        let presence = match self.presence.as_deref() {
            None => None,
            Some(s) => Some(parse_presence(s)?),
        };

        let description = match self.description {
            None => DescriptionPolicy::Passthrough,
            Some(text) => DescriptionPolicy::Override(text),
        };

        let mut args = BTreeMap::new();
        for (arg, ta) in self.args {
            args.insert(arg.clone(), ta.into_arg_policy(tool, &arg)?);
        }

        Ok(ToolPolicy {
            presence,
            rename: self.rename,
            description,
            args,
        })
    }
}

impl TomlArgPolicy {
    /// Lower one argument's flat surface into a core [`ArgPolicy`].
    ///
    /// Exactly one transform may be set. `pin`, `default`, `enum`, a `min`/`max`
    /// range, and `pattern` are mutually exclusive; setting more than one (or, in
    /// the case of the empty table, none) is a [`LowerError`]. `tool`/`arg` name
    /// the location for error context.
    fn into_arg_policy(self, tool: &str, arg: &str) -> Result<ArgPolicy, LowerError> {
        // Count which transform families are present. `min`/`max` together count
        // as the single `range` constraint.
        let has_pin = self.pin.is_some();
        let has_default = self.default.is_some();
        let has_enum = self.r#enum.is_some();
        let has_range = self.min.is_some() || self.max.is_some();
        let has_pattern = self.pattern.is_some();

        let set = [has_pin, has_default, has_enum, has_range, has_pattern]
            .iter()
            .filter(|b| **b)
            .count();

        if set == 0 {
            return Err(LowerError::new(format!(
                "argument `{arg}` of tool `{tool}` sets no transform; \
                 specify exactly one of pin, default, enum, min/max, pattern"
            )));
        }
        if set > 1 {
            return Err(LowerError::new(format!(
                "argument `{arg}` of tool `{tool}` sets multiple mutually-exclusive \
                 transforms; specify exactly one of pin, default, enum, min/max, pattern"
            )));
        }

        if let Some(v) = self.pin {
            return Ok(ArgPolicy::Pin(v));
        }
        if let Some(v) = self.default {
            return Ok(ArgPolicy::Default(v));
        }
        if let Some(values) = self.r#enum {
            if values.is_empty() {
                return Err(LowerError::new(format!(
                    "argument `{arg}` of tool `{tool}` has an empty `enum`"
                )));
            }
            return Ok(ArgPolicy::Constrain(Constraint::Enum(values)));
        }
        if has_range {
            return Ok(ArgPolicy::Constrain(Constraint::Range {
                min: self.min,
                max: self.max,
            }));
        }
        // Only `pattern` remains.
        Ok(ArgPolicy::Constrain(Constraint::Pattern(
            self.pattern.expect("pattern set by exclusivity check"),
        )))
    }
}

/// Parse a `"keep"`/`"drop"` presence string into a core [`Presence`].
fn parse_presence(s: &str) -> Result<Presence, LowerError> {
    match s {
        "keep" => Ok(Presence::Keep),
        "drop" => Ok(Presence::Drop),
        other => Err(LowerError::new(format!(
            "unknown presence `{other}`; expected `keep` or `drop`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lower(toml_src: &str) -> Result<Policy, LowerError> {
        let parsed: TomlPolicy = toml::from_str(toml_src).expect("valid TOML for test");
        parsed.into_policy()
    }

    #[test]
    fn empty_document_is_passthrough() {
        let policy = lower("").unwrap();
        assert_eq!(policy, Policy::passthrough());
    }

    #[test]
    fn default_presence_drop_seals() {
        let policy = lower("default-presence = \"drop\"").unwrap();
        assert_eq!(policy.default_presence, Presence::Drop);
    }

    #[test]
    fn unknown_default_presence_is_rejected() {
        let err = lower("default-presence = \"allow\"").unwrap_err();
        assert!(err.0.contains("unknown presence"), "{}", err.0);
    }

    #[test]
    fn tool_drop_rename_and_description() {
        let src = r#"
[tools.search]
presence = "keep"
rename = "find"
description = "find stuff"
"#;
        let policy = lower(src).unwrap();
        let tp = &policy.tools["search"];
        assert_eq!(tp.presence, Some(Presence::Keep));
        assert_eq!(tp.rename.as_deref(), Some("find"));
        assert_eq!(
            tp.description,
            DescriptionPolicy::Override("find stuff".to_string())
        );
    }

    #[test]
    fn arg_transforms_lower_correctly() {
        let cases: &[(&str, ArgPolicy)] = &[
            (r#"pin = "hylla""#, ArgPolicy::Pin(json!("hylla"))),
            (r#"default = 10"#, ArgPolicy::Default(json!(10))),
            (
                r#"enum = ["a", "b"]"#,
                ArgPolicy::Constrain(Constraint::Enum(vec![json!("a"), json!("b")])),
            ),
            (
                r#"min = 1.0
max = 5.0"#,
                ArgPolicy::Constrain(Constraint::Range {
                    min: Some(1.0),
                    max: Some(5.0),
                }),
            ),
            (
                r#"min = 0.0"#,
                ArgPolicy::Constrain(Constraint::Range {
                    min: Some(0.0),
                    max: None,
                }),
            ),
            (
                r#"pattern = "^a.*""#,
                ArgPolicy::Constrain(Constraint::Pattern("^a.*".to_string())),
            ),
        ];
        for (body, expected) in cases {
            let src = format!("[tools.t.args.x]\n{body}\n");
            let policy = lower(&src).unwrap();
            assert_eq!(&policy.tools["t"].args["x"], expected, "body: {body}");
        }
    }

    #[test]
    fn arg_with_no_transform_is_rejected() {
        let err = lower("[tools.t.args.x]\n").unwrap_err();
        assert!(err.0.contains("no transform"), "{}", err.0);
    }

    #[test]
    fn arg_with_conflicting_transforms_is_rejected() {
        let src = "[tools.t.args.x]\npin = \"a\"\ndefault = \"b\"\n";
        let err = lower(src).unwrap_err();
        assert!(err.0.contains("mutually-exclusive"), "{}", err.0);
    }

    #[test]
    fn empty_enum_is_rejected() {
        let err = lower("[tools.t.args.x]\nenum = []\n").unwrap_err();
        assert!(err.0.contains("empty `enum`"), "{}", err.0);
    }

    #[test]
    fn round_trip_toml_policy_serde() {
        // Build a representative document, serialize, re-parse, assert identity.
        let mut args = BTreeMap::new();
        args.insert(
            "artifact".to_string(),
            TomlArgPolicy {
                pin: Some(json!("hylla")),
                ..Default::default()
            },
        );
        let mut tools = BTreeMap::new();
        tools.insert(
            "search".to_string(),
            TomlToolPolicy {
                presence: Some("keep".to_string()),
                rename: Some("find".to_string()),
                description: Some("find stuff".to_string()),
                args,
            },
        );
        let original = TomlPolicy {
            default_presence: Some("drop".to_string()),
            tools,
        };

        let serialized = toml::to_string(&original).unwrap();
        let reparsed: TomlPolicy = toml::from_str(&serialized).unwrap();
        assert_eq!(original, reparsed);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        // `deny_unknown_fields` guards against typo'd keys silently dropping rules.
        let err = toml::from_str::<TomlPolicy>("bogus = true\n");
        assert!(err.is_err());
    }
}
