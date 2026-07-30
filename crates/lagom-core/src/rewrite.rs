//! Inverse transform: `rewrite(call, policy) -> upstream_call | Reject`
//! (`SPEC.md` §4, §9).
//!
//! Re-injects pinned arguments, applies defaults, validates constraints, and
//! maps a renamed tool back to its upstream name. Disallowed calls are rejected
//! *before* reaching the upstream, with a message stating why (errors are never
//! swallowed — §9.1).
//!
//! Name resolution matches by exact string equality and is fail-closed FOR A
//! BOUNDED SET of names, not for all input. What is actually enforced, each
//! clause with its locking test:
//!
//! - a name the policy governs resolves on its own rule, or is refused when that
//!   rule drops it (`dropped_tool_is_not_callable`);
//! - a renamed tool answers to its new name and its upstream spelling is refused
//!   (`renamed_tool_maps_back_and_original_is_hidden`,
//!   `rename_still_resolves_exactly_while_its_variants_are_refused`);
//! - a case variant of a governed name is refused, not folded into a match
//!   (`case_variant_of_a_governed_name_is_refused`,
//!   `case_variant_cannot_evade_a_pin`);
//! - an empty or whitespace-padded name is refused even under a passthrough
//!   policy (`empty_tool_name_is_refused_under_passthrough`,
//!   `whitespace_variant_is_refused_listed_or_not`).
//!
//! What is NOT enforced: an UNGOVERNED name — one that is neither an exact rule
//! key/rename nor a case variant of one, and is neither empty nor padded — is
//! not resolved at all. It follows `default_presence`, so under `Keep` it is
//! FORWARDED verbatim (`unrelated_unlisted_name_still_passes_through`,
//! `passthrough_call_unchanged`). That is the specified default, `SPEC.md` §3
//! ("presence — keep | drop (default: keep — *passthrough*)"), not a gap to
//! close by sealing this module; see the residual note in [`resolve`].
//!
//! Consequence: for the bounded set above, a lenient upstream's name matching
//! does not get to decide which tool runs; outside it, under `default_presence =
//! Keep`, the upstream does resolve the name. Further residuals (non-case
//! spellings of governed names) are enumerated on
//! [`Policy::case_shadowed_governed_name`].

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
/// Both LOOKUPS that can accept a name are exact string equality: the rename
/// scan (`tp.rename.as_deref() == Some(name)`) and the `policy.tools` map get.
/// (The third accepting path performs no lookup at all — the `default_presence`
/// arm at the end of the body; see "NOT refused" below.)
///
/// The sole folded comparison in this crate's resolve path is
/// [`Policy::case_shadowed_governed_name`] (`policy.rs:183-187`, the only
/// `to_lowercase` in `policy.rs`/`rewrite.rs`); its `Some` is turned into an
/// `Err` at its single call site in the body below, so folding narrows what is
/// accepted rather than widening it. That direction is locked by
/// `case_variant_cannot_evade_a_pin`, which shows the folded spelling refused
/// rather than forwarded pin-free.
///
/// Refused here, so the upstream's name matching does not pick the tool:
/// - a case variant of a governed name (`case_variant_of_a_governed_name_is_refused`);
/// - the upstream spelling of a renamed tool, and variants of either spelling
///   (`rename_still_resolves_exactly_while_its_variants_are_refused`);
/// - an empty or whitespace-padded name, whether or not the unpadded base name
///   is governed (`empty_tool_name_is_refused_under_passthrough`,
///   `whitespace_variant_is_refused_listed_or_not`) — a rule keyed on the padded
///   spelling itself would still resolve exactly, above;
/// - a governed name whose rule drops it (`dropped_tool_is_not_callable`), and
///   an unruled name under a drop default (`sealed_default_rejects_unlisted`).
///
/// NOT refused: an ungoverned name that is none of the above falls to
/// `default_presence` and is returned unchanged when that is `Keep`
/// (`unrelated_unlisted_name_still_passes_through`) — `SPEC.md` §3's specified
/// default. This function is therefore not a sealed allowlist; sealing it is
/// what `Policy::sealed()` is for.
///
/// See [`Policy::case_shadowed_governed_name`] for the spellings folding does
/// NOT catch (zero-width/bidi, NFKC/full-width, `-`/`_` swaps).
fn resolve<'a>(policy: &'a Policy, name: &str) -> Result<(String, Option<&'a ToolPolicy>), Reject> {
    // A renamed tool answers to its new name; the upstream spelling is refused
    // below (`renamed_tool_maps_back_and_original_is_hidden`,
    // `rename_still_resolves_exactly_while_its_variants_are_refused`).
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
        // whitespace. Control flow, not a claim: the exact `policy.tools` get
        // above already returned for any rule keyed on this literal spelling, so
        // reaching here means no rule matches it and forwarding would rely on
        // upstream trimming. Refusal is locked by
        // `empty_tool_name_is_refused_under_passthrough` and
        // `whitespace_variant_is_refused_listed_or_not`. An upstream tool whose
        // real name really is empty or padded therefore needs an explicit rule
        // under that exact spelling to be callable — that positive path is not
        // covered by a test today.
        return Err(unavailable(
            name,
            "not an exact tool name (empty or whitespace-padded)",
        ));
    }

    // Unrelated to anything governed: `default_presence` decides. Under `Keep`
    // the name is returned verbatim and forwarded — `SPEC.md` §3's specified
    // default, locked by `unrelated_unlisted_name_still_passes_through`. So
    // lagom does admit names it holds no rule for, including non-case variants
    // of an UNGOVERNED upstream tool.
    //
    // RESIDUAL, and what closing it would actually cost. Deciding those names
    // here needs the authoritative upstream `tools/list` surface, and this
    // function's public entrypoint admits none: `rewrite` takes
    // `(&ToolCall, &Policy)` (see its signature above), so a surface cannot arrive
    // as an argument without changing that signature (lock class: the type
    // system). What each production call site holds — as committed; no test pins
    // this list, so an added caller would not turn anything red:
    //   * `Guard::gate` (`guard.rs:72-74`) passes only `self.policy`. `Guard` is
    //     `{ policy, slim_defs }` (`guard.rs:37-40`); `Guard::new`
    //     (`guard.rs:51-53`) takes `upstream: &[ToolDef]`, projects it once into
    //     `slim_defs` (`guard.rs:52`), and drops it. `project` omits dropped tools
    //     (`project::tests::drop_removes_tool`, `sealed_drops_unlisted_tools`,
    //     both asserting an empty projection), and dropped names are exactly the
    //     ones a refusal would have to recognise, so `slim_defs` does not stand in
    //     for the full surface.
    //   * the proxy's `bridge::handle_tools_call` — parameters
    //     `(msg, policy: &lagom_core::Policy, audit, run_id)`, no defs — called from
    //     `bridge::pump_downstream`, whose own parameters carry `Arc<Policy>` and no
    //     defs. `Server`'s private `upstream_defs: Vec<ToolDef>` field does hold the
    //     probed surface, but `Server::run_with` destructures `self` and MOVES that
    //     field into `AuditEvent::OriginalDefs` before it spawns the pumps.
    //     (Cited by symbol, not line: `bridge.rs` renumbers often.)
    //   * the three binding faces — `lagom-node/src/lib.rs:97`,
    //     `lagom-py/src/lib.rs:98`, `lagom-wasm/src/lib.rs:230` — each parse
    //     `{call, policy}` out of JSON and pass just those two.
    // The momentary holder is the proxy's `Server` struct; `Guard` does not hold it,
    // per its two fields at `guard.rs:37-40`. Closing the residual is
    // therefore NEW RETAINED STATE — a surface that today is either destroyed by
    // projection or moved into the audit event has to be retained and threaded —
    // plus a decision about a surface that can go stale (`tools/list_changed`
    // adds upstream tools mid-session). A possible future change, not plumbing.
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
    // the dropped tool. The fix is refusal, not a wider match: folding is used
    // to deny only (see `resolve`'s doc), and these tests fix that direction —
    // each asserts a refusal, so turning the fold into a lookup fails them.
    // Scope: they cover case and whitespace variants of GOVERNED names. An
    // ungoverned name still follows the default; that is asserted separately by
    // `unrelated_unlisted_name_still_passes_through`.
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
            // Also refused with no rule at all: neither policy here has a rule
            // keyed on the padded spelling, so it does not ride the default-keep
            // passthrough. (A policy that DID key a rule on `"search "` would
            // resolve it exactly — untested; see `resolve`.)
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
        // Exact name: the pin overrides the value the agent sent.
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
        // default (`SPEC.md` §3). This test IS the lock for the "ungoverned name
        // is forwarded under `Keep`" clause in the module doc and `resolve`'s doc
        // — the residual those describe, which closing would require new retained
        // upstream-surface state.
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
