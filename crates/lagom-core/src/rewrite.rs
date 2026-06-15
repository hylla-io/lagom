//! Inverse transform: `rewrite(call, policy) -> upstream_call | Reject`
//! (`SPEC.md` §4, §9).
//!
//! Re-injects pinned arguments, applies defaults, validates constraints, and
//! maps a renamed tool back to its upstream name. Disallowed calls are rejected
//! *before* reaching the upstream, with a message stating why (errors are never
//! swallowed — §9.1).

use regex::Regex;
use serde_json::{Map, Value};

use crate::policy::{ArgPolicy, Constraint, Policy, Presence, ToolPolicy};
use crate::tooldef::ToolCall;

/// A rejected call. `message` states the violated bound, suitable for returning
/// to the agent as an error result.
#[derive(Debug, Clone, PartialEq)]
pub struct Reject {
    /// Human-readable reason the call was rejected.
    pub message: String,
}

impl Reject {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Reject {}

/// Rewrite a projected call into the upstream call, or reject it.
pub fn rewrite(call: &ToolCall, policy: &Policy) -> Result<ToolCall, Reject> {
    let (upstream_name, tp) = resolve(policy, &call.name)
        .ok_or_else(|| Reject::new(format!("unknown or unavailable tool: `{}`", call.name)))?;

    let mut arguments: Map<String, Value> = match &call.arguments {
        Value::Object(map) => map.clone(),
        Value::Null => Map::new(),
        _ => Map::new(),
    };

    if let Some(tp) = tp {
        for (arg, policy) in &tp.args {
            match policy {
                ArgPolicy::Pin(value) => {
                    arguments.insert(arg.clone(), value.clone());
                }
                ArgPolicy::Default(value) => {
                    arguments
                        .entry(arg.clone())
                        .or_insert_with(|| value.clone());
                }
                ArgPolicy::Constrain(constraint) => {
                    if let Some(value) = arguments.get(arg) {
                        check_constraint(arg, value, constraint)?;
                    }
                }
                ArgPolicy::Passthrough => {}
            }
        }
    }

    Ok(ToolCall {
        name: upstream_name,
        arguments: Value::Object(arguments),
    })
}

/// Resolve a projected (possibly renamed) tool name to its upstream name plus
/// the tool policy. Returns `None` for dropped or hidden-behind-rename tools.
fn resolve<'a>(policy: &'a Policy, name: &str) -> Option<(String, Option<&'a ToolPolicy>)> {
    // A renamed tool is callable only by its new name.
    for (upstream, tp) in &policy.tools {
        if tp.rename.as_deref() == Some(name) {
            return match policy.presence_of(upstream) {
                Presence::Keep => Some((upstream.clone(), Some(tp))),
                Presence::Drop => None,
            };
        }
    }

    if let Some(tp) = policy.tools.get(name) {
        // The original name is hidden once the tool is renamed away.
        if tp.rename.is_some() {
            return None;
        }
        return match policy.presence_of(name) {
            Presence::Keep => Some((name.to_string(), Some(tp))),
            Presence::Drop => None,
        };
    }

    // No explicit rule: callable iff the default keeps it (passthrough).
    match policy.default_presence {
        Presence::Keep => Some((name.to_string(), None)),
        Presence::Drop => None,
    }
}

fn check_constraint(arg: &str, value: &Value, constraint: &Constraint) -> Result<(), Reject> {
    let ok = match constraint {
        Constraint::Enum(allowed) => allowed.contains(value),
        Constraint::Range { min, max } => match value.as_f64() {
            Some(n) => min.is_none_or(|lo| n >= lo) && max.is_none_or(|hi| n <= hi),
            None => false,
        },
        Constraint::Pattern(pattern) => {
            let re = Regex::new(pattern)
                .map_err(|e| Reject::new(format!("invalid constraint on `{arg}`: {e}")))?;
            value.as_str().is_some_and(|s| re.is_match(s))
        }
    };
    if ok {
        Ok(())
    } else {
        Err(Reject::new(format!(
            "argument `{arg}` violates its constraint: {}",
            crate::project::describe_constraint(arg, constraint)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ToolPolicy;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn policy_with(name: &str, tp: ToolPolicy) -> Policy {
        let mut tools = BTreeMap::new();
        tools.insert(name.to_string(), tp);
        Policy {
            default_presence: Presence::Keep,
            tools,
        }
    }

    #[test]
    fn passthrough_call_unchanged() {
        let call = ToolCall::new("search", json!({"query": "x"}));
        let out = rewrite(&call, &Policy::passthrough()).unwrap();
        assert_eq!(out.name, "search");
        assert_eq!(out.arguments, json!({"query": "x"}));
    }

    #[test]
    fn pin_is_injected_overriding_agent_value() {
        let mut args = BTreeMap::new();
        args.insert("artifact".to_string(), ArgPolicy::Pin(json!("hylla")));
        let p = policy_with(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        let call = ToolCall::new("search", json!({"query": "x", "artifact": "evil"}));
        let out = rewrite(&call, &p).unwrap();
        assert_eq!(out.arguments["artifact"], json!("hylla"));
    }

    #[test]
    fn default_fills_only_when_absent() {
        let mut args = BTreeMap::new();
        args.insert("limit".to_string(), ArgPolicy::Default(json!(10)));
        let p = policy_with(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        let out = rewrite(&ToolCall::new("search", json!({})), &p).unwrap();
        assert_eq!(out.arguments["limit"], json!(10));
        let out2 = rewrite(&ToolCall::new("search", json!({"limit": 3})), &p).unwrap();
        assert_eq!(out2.arguments["limit"], json!(3));
    }

    #[test]
    fn enum_constraint_rejects_out_of_set() {
        let mut args = BTreeMap::new();
        args.insert(
            "artifact".to_string(),
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("a"), json!("b")])),
        );
        let p = policy_with(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        assert!(rewrite(&ToolCall::new("search", json!({"artifact": "a"})), &p).is_ok());
        let err = rewrite(&ToolCall::new("search", json!({"artifact": "c"})), &p).unwrap_err();
        assert!(err.message.contains("artifact"));
    }

    #[test]
    fn range_constraint_enforced() {
        let mut args = BTreeMap::new();
        args.insert(
            "limit".to_string(),
            ArgPolicy::Constrain(Constraint::Range {
                min: Some(1.0),
                max: Some(5.0),
            }),
        );
        let p = policy_with(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        assert!(rewrite(&ToolCall::new("search", json!({"limit": 3})), &p).is_ok());
        assert!(rewrite(&ToolCall::new("search", json!({"limit": 9})), &p).is_err());
    }

    #[test]
    fn pattern_constraint_enforced() {
        let mut args = BTreeMap::new();
        args.insert(
            "path".to_string(),
            ArgPolicy::Constrain(Constraint::Pattern("^src/".to_string())),
        );
        let p = policy_with(
            "edit",
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        assert!(rewrite(&ToolCall::new("edit", json!({"path": "src/main.rs"})), &p).is_ok());
        assert!(rewrite(&ToolCall::new("edit", json!({"path": "/etc/passwd"})), &p).is_err());
    }

    #[test]
    fn dropped_tool_is_not_callable() {
        let p = policy_with(
            "search",
            ToolPolicy {
                presence: Some(Presence::Drop),
                ..Default::default()
            },
        );
        assert!(rewrite(&ToolCall::new("search", json!({})), &p).is_err());
    }

    #[test]
    fn renamed_tool_maps_back_and_original_is_hidden() {
        let p = policy_with(
            "search",
            ToolPolicy {
                rename: Some("find".to_string()),
                ..Default::default()
            },
        );
        let out = rewrite(&ToolCall::new("find", json!({"query": "x"})), &p).unwrap();
        assert_eq!(out.name, "search");
        // Original name no longer callable.
        assert!(rewrite(&ToolCall::new("search", json!({})), &p).is_err());
    }

    #[test]
    fn sealed_default_rejects_unlisted() {
        let p = Policy::sealed();
        assert!(rewrite(&ToolCall::new("anything", json!({})), &p).is_err());
    }
}
