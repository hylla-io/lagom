//! Drift safety: `validate(policy, upstream) -> Ok | Vec<DriftError>`
//! (`SPEC.md` §5.3).
//!
//! Checks every policy reference against the live upstream surface. A policy
//! that pins/constrains a vanished argument, or rules a vanished tool, is drift:
//! callers must fail loud rather than forward a broken call.
//!
//! The call-contract guarantee (§5.1) holds by construction: the only transform
//! that removes a property from the projected schema is `Pin`, which supplies
//! the value on every call — so a hidden argument always has a value.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::policy::{ArgPolicy, Constraint, Policy, Presence};
use crate::tooldef::ToolDef;

/// A reference in the policy that no longer matches the upstream surface.
#[derive(Debug, Clone, PartialEq)]
pub struct DriftError {
    /// Human-readable description of the stale reference.
    pub message: String,
}

impl DriftError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DriftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for DriftError {}

/// Validate a policy against the upstream tool surface. Returns every stale
/// reference found; `Ok(())` means the policy is fully grounded.
pub fn validate(policy: &Policy, upstream: &[ToolDef]) -> Result<(), Vec<DriftError>> {
    let mut errors = Vec::new();

    for (tool_name, tp) in &policy.tools {
        let Some(def) = upstream.iter().find(|t| &t.name == tool_name) else {
            errors.push(DriftError::new(format!(
                "policy references unknown upstream tool `{tool_name}`"
            )));
            continue;
        };

        let props = def
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object());

        for (arg, ap) in &tp.args {
            let Some(prop) = props.and_then(|p| p.get(arg)) else {
                errors.push(DriftError::new(format!(
                    "policy references unknown argument `{arg}` on tool `{tool_name}`"
                )));
                continue;
            };

            // The argument exists upstream — additionally check that any
            // literal value the policy supplies (a pin, a default, or an enum
            // subset) is coherent with the upstream property's declared
            // JSON-Schema `type` (and `enum`, when present). A pin
            // `limit="STRING"` on an integer argument is structurally doomed:
            // catch it loud at load time rather than blindly forwarding a call
            // that only the upstream can reject at runtime (`SPEC.md` §5.3).
            check_literals(&mut errors, tool_name, arg, ap, prop);
        }
    }

    // Two kept tools must not project to the same downstream name: a rename onto
    // another kept tool's name would shadow it and silently route the innocuous
    // name to the hidden tool. Detect any collision among surviving tools.
    let mut projected: BTreeMap<String, String> = BTreeMap::new();
    for def in upstream {
        if policy.presence_of(&def.name) == Presence::Drop {
            continue;
        }
        let name = policy
            .tools
            .get(&def.name)
            .and_then(|t| t.rename.clone())
            .unwrap_or_else(|| def.name.clone());
        if let Some(other) = projected.insert(name.clone(), def.name.clone()) {
            errors.push(DriftError::new(format!(
                "projected name `{name}` collides: both `{other}` and `{}` map to it",
                def.name
            )));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Check that the literal values a policy supplies for an argument are coherent
/// with the upstream property schema. Only literals lagom actually sends or
/// narrows to are checked: a `Pin`/`Default` value, and each member of a
/// `Constrain(Enum)` subset. `Constrain(Range)`/`Constrain(Pattern)` and
/// `Passthrough` carry no literal to validate here.
fn check_literals(
    errors: &mut Vec<DriftError>,
    tool: &str,
    arg: &str,
    ap: &ArgPolicy,
    prop: &Value,
) {
    match ap {
        ArgPolicy::Pin(v) => check_value(errors, tool, arg, "pin", v, prop),
        ArgPolicy::Default(v) => check_value(errors, tool, arg, "default", v, prop),
        ArgPolicy::Constrain(Constraint::Enum(values)) => {
            for v in values {
                check_value(errors, tool, arg, "enum member", v, prop);
            }
        }
        ArgPolicy::Constrain(_) | ArgPolicy::Passthrough => {}
    }
}

/// Validate a single literal against the upstream property's `type` and `enum`.
/// Pushes a `DriftError` per incoherence (a type mismatch, or a value outside
/// the upstream's own enum domain).
fn check_value(
    errors: &mut Vec<DriftError>,
    tool: &str,
    arg: &str,
    role: &str,
    value: &Value,
    prop: &Value,
) {
    if let Some(expected) = prop.get("type").and_then(Value::as_str)
        && !value_matches_type(value, expected)
    {
        errors.push(DriftError::new(format!(
            "{role} value {value} for argument `{arg}` on tool `{tool}` \
             is not a valid `{expected}` per the upstream schema"
        )));
    }

    if let Some(domain) = prop.get("enum").and_then(Value::as_array)
        && !domain.contains(value)
    {
        errors.push(DriftError::new(format!(
            "{role} value {value} for argument `{arg}` on tool `{tool}` \
             is not in the upstream enum domain"
        )));
    }
}

/// Whether a JSON value satisfies a JSON-Schema primitive `type` keyword.
///
/// `integer` additionally requires an integral number (JSON Schema treats
/// `1.0` as an integer; a fractional float is not). `number` accepts any
/// numeric. Unknown/unsupported type names are treated as a match so lagom
/// never rejects on a schema keyword it does not model.
fn value_matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some() || is_integral_f64(value),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// Whether a value is a float with no fractional part (JSON Schema integer).
fn is_integral_f64(value: &Value) -> bool {
    value.as_f64().is_some_and(|f| f.fract() == 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ArgPolicy, Constraint, ToolPolicy};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn upstream() -> Vec<ToolDef> {
        vec![ToolDef::new(
            "search",
            None,
            json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )]
    }

    /// An upstream whose `search` tool has a string `query`, an integer `limit`,
    /// and a string `kind` constrained to an upstream enum.
    fn typed_upstream() -> Vec<ToolDef> {
        vec![ToolDef::new(
            "search",
            None,
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer"},
                    "kind": {"type": "string", "enum": ["doc", "code"]}
                }
            }),
        )]
    }

    fn policy_arg(arg: &str, ap: ArgPolicy) -> Policy {
        let mut args = BTreeMap::new();
        args.insert(arg.to_string(), ap);
        policy_with(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
        )
    }

    fn policy_with(name: &str, tp: ToolPolicy) -> Policy {
        let mut tools = BTreeMap::new();
        tools.insert(name.to_string(), tp);
        Policy {
            default_presence: crate::policy::Presence::Keep,
            tools,
        }
    }

    #[test]
    fn grounded_policy_validates() {
        let mut args = BTreeMap::new();
        args.insert("query".to_string(), ArgPolicy::Pin(json!("x")));
        let p = policy_with(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        assert!(validate(&p, &upstream()).is_ok());
    }

    #[test]
    fn unknown_tool_is_drift() {
        let p = policy_with("missing", ToolPolicy::default());
        let errs = validate(&p, &upstream()).unwrap_err();
        assert!(errs[0].message.contains("missing"));
    }

    #[test]
    fn unknown_arg_is_drift() {
        let mut args = BTreeMap::new();
        args.insert("vanished".to_string(), ArgPolicy::Pin(json!("x")));
        let p = policy_with(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        let errs = validate(&p, &upstream()).unwrap_err();
        assert!(errs[0].message.contains("vanished"));
    }

    #[test]
    fn rename_onto_another_kept_tool_is_collision() {
        let up = vec![
            ToolDef::new("danger", None, json!({"type": "object", "properties": {}})),
            ToolDef::new("safe", None, json!({"type": "object", "properties": {}})),
        ];
        let mut tools = BTreeMap::new();
        tools.insert(
            "danger".to_string(),
            ToolPolicy {
                rename: Some("safe".to_string()),
                ..Default::default()
            },
        );
        let p = Policy {
            default_presence: crate::policy::Presence::Keep,
            tools,
        };
        let errs = validate(&p, &up).unwrap_err();
        assert!(errs.iter().any(|e| e.message.contains("collides")));
    }

    #[test]
    fn pin_with_wrong_type_is_drift() {
        let p = policy_arg("limit", ArgPolicy::Pin(json!("STRING")));
        let errs = validate(&p, &typed_upstream()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("pin") && e.message.contains("integer")),
            "{errs:?}"
        );
    }

    #[test]
    fn pin_with_matching_type_validates() {
        let p = policy_arg("limit", ArgPolicy::Pin(json!(10)));
        assert!(validate(&p, &typed_upstream()).is_ok());
    }

    #[test]
    fn integer_arg_accepts_integral_float_pin() {
        // JSON Schema treats 10.0 as a valid integer.
        let p = policy_arg("limit", ArgPolicy::Pin(json!(10.0)));
        assert!(validate(&p, &typed_upstream()).is_ok());
    }

    #[test]
    fn integer_arg_rejects_fractional_float_pin() {
        let p = policy_arg("limit", ArgPolicy::Pin(json!(10.5)));
        let errs = validate(&p, &typed_upstream()).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("integer")),
            "{errs:?}"
        );
    }

    #[test]
    fn default_with_wrong_type_is_drift() {
        let p = policy_arg("limit", ArgPolicy::Default(json!("nope")));
        let errs = validate(&p, &typed_upstream()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("default") && e.message.contains("integer")),
            "{errs:?}"
        );
    }

    #[test]
    fn enum_member_with_wrong_type_is_drift() {
        let p = policy_arg(
            "limit",
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("not-a-number"), json!(3)])),
        );
        let errs = validate(&p, &typed_upstream()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("enum member") && e.message.contains("integer")),
            "{errs:?}"
        );
    }

    #[test]
    fn pin_outside_upstream_enum_domain_is_drift() {
        // `kind` is upstream-constrained to {doc, code}; pinning "wiki" is drift.
        let p = policy_arg("kind", ArgPolicy::Pin(json!("wiki")));
        let errs = validate(&p, &typed_upstream()).unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.message.contains("upstream enum domain")),
            "{errs:?}"
        );
    }

    #[test]
    fn pin_within_upstream_enum_domain_validates() {
        let p = policy_arg("kind", ArgPolicy::Pin(json!("doc")));
        assert!(validate(&p, &typed_upstream()).is_ok());
    }

    #[test]
    fn enum_subset_of_upstream_enum_validates() {
        let p = policy_arg(
            "kind",
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("doc")])),
        );
        assert!(validate(&p, &typed_upstream()).is_ok());
    }

    #[test]
    fn untyped_upstream_property_skips_literal_check() {
        // A property with no `type` keyword: lagom must not reject literals.
        let up = vec![ToolDef::new(
            "search",
            None,
            json!({"type": "object", "properties": {"anything": {}}}),
        )];
        let p = policy_arg("anything", ArgPolicy::Pin(json!("whatever")));
        assert!(validate(&p, &up).is_ok());
    }
}
