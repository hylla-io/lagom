//! Inverse transform: `rewrite(call, policy) -> upstream_call | Reject`
//! (`SPEC.md` §4, §9).
//!
//! Re-injects pinned arguments, applies defaults, validates constraints, and
//! maps a renamed tool back to its upstream name. Disallowed calls are rejected
//! *before* reaching the upstream, with a message stating why (errors are never
//! swallowed — §9.1).
//!
//! Name resolution is EXACT and fail-closed: a name lagom cannot resolve by
//! exact equality is refused here rather than forwarded for the upstream to
//! interpret, so a lenient upstream's name matching can never become the thing
//! that decides which tool runs. What that does and does not cover is stated on
//! [`resolve`] and on [`Policy::case_shadowed_governed_name`].

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
    let (upstream_name, tp) = resolve(policy, &call.name)?;

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
/// the tool policy, or say why the call is refused.
///
/// Every lookup here is EXACT string equality. A name lagom cannot resolve
/// exactly is refused, never forwarded for the upstream to interpret: an
/// upstream that matches names leniently would otherwise reach a tool through a
/// spelling the policy never decided on (case variant, padded name), skipping
/// that tool's presence/pin/constraint rules. Refusal keeps the decision here.
/// See [`Policy::case_shadowed_governed_name`] for why the only folding in this
/// module denies and never matches, and for what that check does NOT cover.
fn resolve<'a>(policy: &'a Policy, name: &str) -> Result<(String, Option<&'a ToolPolicy>), Reject> {
    // A renamed tool is callable only by its new name.
    for (upstream, tp) in &policy.tools {
        if tp.rename.as_deref() == Some(name) {
            return match policy.presence_of(upstream) {
                Presence::Keep => Ok((upstream.clone(), Some(tp))),
                Presence::Drop => Err(unavailable(name, "the tool it renames is dropped")),
            };
        }
    }

    if let Some(tp) = policy.tools.get(name) {
        // The original name is hidden once the tool is renamed away.
        if let Some(rename) = &tp.rename {
            return Err(unavailable(
                name,
                &format!("renamed away; callable only as `{rename}`"),
            ));
        }
        return match policy.presence_of(name) {
            Presence::Keep => Ok((name.to_string(), Some(tp))),
            Presence::Drop => Err(unavailable(name, "dropped by policy")),
        };
    }

    // No exact rule. Two fail-closed refusals come BEFORE the default decides,
    // because a default of `Keep` would otherwise forward the name verbatim.
    if let Some(governed) = policy.case_shadowed_governed_name(name) {
        return Err(unavailable(
            name,
            &format!(
                "no exact rule; differs only by case from `{governed}`, which this policy \
                 governs — lagom matches tool names exactly and refuses variants"
            ),
        ));
    }
    if name.is_empty() || name != name.trim() {
        // Empty (the proxy defaults a missing `params.name` to "") or padded with
        // whitespace. Such a name is not the exact name of any tool lagom has a
        // rule for, and forwarding it relies on upstream trimming. An upstream
        // tool whose real name is empty or padded is reachable only by giving it
        // an explicit rule in the policy — an exact match, made deliberately.
        return Err(unavailable(
            name,
            "not an exact tool name (empty or whitespace-padded)",
        ));
    }

    // Unrelated to anything governed: the default decides (passthrough when
    // `Keep`). NOTE the remaining fail-open surface — under `Keep`, lagom admits
    // a name it knows nothing about, including a non-case variant of an UNGOVERNED
    // upstream tool. Closing that needs the authoritative upstream `tools/list`
    // surface, which this function is not given; it is already probed at mint
    // time (`lagom-proxy`'s `spawn_and_validate` → `Server::upstream_defs`) and
    // held by `Guard`, so threading it in is plumbing, not new knowledge.
    match policy.default_presence {
        Presence::Keep => Ok((name.to_string(), None)),
        Presence::Drop => Err(unavailable(name, "no rule and the policy default is drop")),
    }
}

/// Build the refusal for an unresolvable tool name.
///
/// The `unknown or unavailable tool` prefix is the stable wire text (annotated
/// downstream per `SPEC.md` §9.1); `why` appends the specific cause so a refusal
/// is diagnosable instead of uniformly opaque.
fn unavailable(name: &str, why: &str) -> Reject {
    Reject::new(format!("unknown or unavailable tool: `{name}` — {why}"))
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

    // -----------------------------------------------------------------------
    // Name-variant fail-closed (`SPEC.md` §4, §9.1). A reviewer executed the
    // hole these lock: under a default-KEEP policy that drops `search`, the
    // names `SEARCH` and `search ` fell through to the default and were
    // FORWARDED, and an upstream that resolves names leniently would then run
    // the dropped tool. Refusal — never a wider match — is the fix.
    // -----------------------------------------------------------------------

    /// The policy shape the reviewer exploited: default keep, `search` dropped.
    fn default_keep_drops_search() -> Policy {
        policy_with(
            "search",
            ToolPolicy {
                presence: Some(Presence::Drop),
                ..Default::default()
            },
        )
    }

    #[test]
    fn case_variant_of_a_governed_name_is_refused() {
        let p = default_keep_drops_search();
        for name in ["SEARCH", "Search", "sEaRcH"] {
            let err = rewrite(&ToolCall::new(name, json!({"query": "x"})), &p)
                .unwrap_err_or_else_name(name);
            assert!(
                err.message.contains("search"),
                "reject must name the governed tool it shadows: {}",
                err.message
            );
        }
    }

    #[test]
    fn whitespace_variant_is_refused_listed_or_not() {
        let dropped = default_keep_drops_search();
        for name in ["search ", " search", "\tsearch", "search\n"] {
            rewrite(&ToolCall::new(name, json!({})), &dropped).unwrap_err_or_else_name(name);
            // Also refused with no rule at all: a padded name is never an exact
            // tool name, so it cannot ride the default-keep passthrough either.
            rewrite(&ToolCall::new(name, json!({})), &Policy::passthrough())
                .unwrap_err_or_else_name(name);
        }
    }

    #[test]
    fn empty_tool_name_is_refused_under_passthrough() {
        // The proxy defaults a missing `params.name` to "" — that must not be
        // forwarded as a tool call.
        rewrite(&ToolCall::new("", json!({})), &Policy::passthrough()).unwrap_err_or_else_name("");
    }

    #[test]
    fn case_variant_cannot_evade_a_pin() {
        let mut args = BTreeMap::new();
        args.insert("artifact".to_string(), ArgPolicy::Pin(json!("hylla")));
        let p = policy_with(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        // Exact name: the pin is inescapable.
        let out = rewrite(&ToolCall::new("search", json!({"artifact": "evil"})), &p).unwrap();
        assert_eq!(out.arguments["artifact"], json!("hylla"));
        // Case variant: refused, so there is no pin-free forward of this tool.
        rewrite(&ToolCall::new("SEARCH", json!({"artifact": "evil"})), &p)
            .unwrap_err_or_else_name("SEARCH");
    }

    #[test]
    fn rename_still_resolves_exactly_while_its_variants_are_refused() {
        let p = policy_with(
            "search",
            ToolPolicy {
                rename: Some("find".to_string()),
                ..Default::default()
            },
        );
        // Legitimate rename behaviour is untouched.
        let out = rewrite(&ToolCall::new("find", json!({"query": "x"})), &p).unwrap();
        assert_eq!(out.name, "search");
        // Variants of BOTH the projected and the upstream spelling are refused.
        for name in ["FIND", "find ", "SEARCH", "search"] {
            rewrite(&ToolCall::new(name, json!({})), &p).unwrap_err_or_else_name(name);
        }
    }

    #[test]
    fn unrelated_unlisted_name_still_passes_through() {
        // The refusal is scoped: it does NOT turn default-keep into a sealed
        // policy. A name that resembles nothing governed still follows the
        // default — this is the residual the upstream-surface plumbing closes.
        let p = default_keep_drops_search();
        let out = rewrite(&ToolCall::new("grep", json!({"pattern": "x"})), &p).unwrap();
        assert_eq!(out.name, "grep");
    }

    /// Test-only sugar: assert a call was refused, naming it in the panic so a
    /// regression says *which* variant leaked instead of just "unwrap on Ok".
    trait MustReject {
        fn unwrap_err_or_else_name(self, name: &str) -> Reject;
    }

    impl MustReject for Result<ToolCall, Reject> {
        fn unwrap_err_or_else_name(self, name: &str) -> Reject {
            match self {
                Ok(forwarded) => panic!(
                    "name variant `{name}` must be refused, was forwarded as `{}`",
                    forwarded.name
                ),
                Err(reject) => reject,
            }
        }
    }
}
