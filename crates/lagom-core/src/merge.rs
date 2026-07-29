//! Narrow-only composition: `merge(base, overlay) -> Policy | MergeError`
//! (`SPEC.md` §5.2).
//!
//! The overlay (end-user) may only *narrow* the base (integrator). Any widening
//! — re-adding a dropped tool, loosening a constraint, unpinning, re-pinning to a
//! different value — is a load-time error. This function *is* the sealed-bounds
//! enforcement.

use regex::Regex;

use crate::policy::{ArgPolicy, Constraint, DescriptionPolicy, Policy, Presence, ToolPolicy};

/// A widening attempt detected while composing an overlay onto a base.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeError {
    /// What widening was attempted.
    pub message: String,
}

impl MergeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MergeError {}

/// Compose `overlay` onto `base`, narrowing only.
pub fn merge(base: &Policy, overlay: &Policy) -> Result<Policy, MergeError> {
    if !overlay
        .default_presence
        .is_narrower_or_equal(base.default_presence)
    {
        return Err(MergeError::new(
            "overlay widens default presence from `drop` to `keep`",
        ));
    }

    let mut result = base.clone();
    result.default_presence = overlay.default_presence;

    for (name, overlay_tp) in &overlay.tools {
        let base_tp = base.tools.get(name).cloned().unwrap_or_else(|| ToolPolicy {
            presence: Some(base.default_presence),
            ..Default::default()
        });
        let merged = merge_tool(name, &base_tp, overlay_tp, base.default_presence)?;
        result.tools.insert(name.clone(), merged);
    }

    Ok(result)
}

fn merge_tool(
    name: &str,
    base: &ToolPolicy,
    overlay: &ToolPolicy,
    default_presence: Presence,
) -> Result<ToolPolicy, MergeError> {
    let base_presence = base.presence.unwrap_or(default_presence);
    let presence = match overlay.presence {
        Some(p) if !p.is_narrower_or_equal(base_presence) => {
            return Err(MergeError::new(format!(
                "overlay widens presence of tool `{name}`"
            )));
        }
        Some(p) => Some(p),
        None => base.presence,
    };

    // Sealing a rename turns on whether a naming bound was ever SET, not on
    // whether the overlay supplies one. `None` is unbound: a tool the base never
    // mentioned is synthesized from `ToolPolicy::default()` above, and that is how
    // *every* binding arrives — `lagom-node`/`lagom-py` mint with
    // `config_paths: []`, so `lagom_proxy::mint` folds the whole built policy as an
    // overlay onto `Policy::passthrough()`. Treating unbound as sealed made
    // `PolicyBuilder::rename` unexpressible through those faces.
    //
    // An explicit base rename *is* a sealed bound: redirecting it would re-point
    // the integrator's downstream vocabulary at a different upstream tool, so only
    // an identical restatement passes. Rename is vocabulary, not authority — the
    // widenings SPEC.md §5.2 names are re-adding a dropped tool, loosening a
    // constraint, and unpinning.
    //
    // `(None, Some(new))` is the ONLY relaxation, and an overlay-authored name can
    // now land in the merged policy. Exhaustively, by `new`'s relation to the rest
    // of the merged surface (every case locked by a test below):
    //
    // 1. `new` matches nothing else projected — plain vocabulary.
    // 2. `new` collides with another KEPT tool's projected name: that tool's
    //    upstream name, its base-sealed rename (a vocabulary hijack that leaves
    //    the victim's own `rename` field untouched, so no per-tool comparison here
    //    can see it), or a second overlay rename. `merge` has no upstream surface,
    //    so `validate`'s projected-name collision loop is what rejects these, and
    //    it runs on every serve path (`lagom_proxy::spawn_and_validate`). It is
    //    NOT run by the in-process `Guard` face (`guard.rs`) — but a hand-authored
    //    `Policy` already collides there with no merge involved, so that gap is
    //    `Guard`'s, not this function's.
    // 3. `new` shadows a kept tool while the RENAMED tool is dropped: `validate`
    //    ignores dropped tools, so the state is admitted, and `rewrite::resolve`
    //    matches the rename before the direct lookup and returns `None` for a
    //    dropped target — the victim name goes uncallable. Fail-CLOSED: lost
    //    availability, never gained authority.
    // 4. A rename swap/chain (`a`→`b`, `b`→`c`) projects distinct names, so
    //    `validate` passes and the state is admitted. Safe because every bound
    //    stays keyed by UPSTREAM tool: `rewrite` resolves a projected name to one
    //    upstream tool and applies THAT tool's pins/constraints, so a rename can
    //    neither detach a tool's bound nor borrow another tool's.
    let rename = match (&base.rename, &overlay.rename) {
        (None, unbound) => unbound.clone(),
        (Some(sealed), None) => Some(sealed.clone()),
        (Some(sealed), Some(restated)) if sealed == restated => Some(sealed.clone()),
        (Some(sealed), Some(redirect)) => {
            return Err(MergeError::new(format!(
                "overlay may not rename tool `{name}` from `{sealed}` to `{redirect}`"
            )));
        }
    };

    // An overlay may slim the description (Override); it may not re-expose a
    // base override by reverting to Passthrough.
    let description = match (&base.description, &overlay.description) {
        (DescriptionPolicy::Override(_), DescriptionPolicy::Override(t)) => {
            DescriptionPolicy::Override(t.clone())
        }
        (DescriptionPolicy::Override(t), DescriptionPolicy::Passthrough) => {
            DescriptionPolicy::Override(t.clone())
        }
        (_, DescriptionPolicy::Override(t)) => DescriptionPolicy::Override(t.clone()),
        (b, DescriptionPolicy::Passthrough) => b.clone(),
    };

    let mut args = base.args.clone();
    for (arg, overlay_ap) in &overlay.args {
        let base_ap = base
            .args
            .get(arg)
            .cloned()
            .unwrap_or(ArgPolicy::Passthrough);
        args.insert(arg.clone(), narrow_arg(name, arg, &base_ap, overlay_ap)?);
    }

    Ok(ToolPolicy {
        presence,
        rename,
        description,
        args,
    })
}

/// Decide the narrowed arg policy, or reject a widening attempt.
fn narrow_arg(
    tool: &str,
    arg: &str,
    base: &ArgPolicy,
    overlay: &ArgPolicy,
) -> Result<ArgPolicy, MergeError> {
    let widen = || {
        Err(MergeError::new(format!(
            "overlay widens argument `{arg}` of tool `{tool}`"
        )))
    };

    match (base, overlay) {
        // A pin is already maximally narrow: only an identical pin is allowed.
        (ArgPolicy::Pin(a), ArgPolicy::Pin(b)) if a == b => Ok(ArgPolicy::Pin(b.clone())),
        (ArgPolicy::Pin(_), _) => widen(),

        // Passthrough/Default impose no domain bound: any overlay narrows or holds.
        (ArgPolicy::Passthrough | ArgPolicy::Default(_), ov) => Ok(ov.clone()),

        // A constraint may be tightened to a pin (within the constraint) or to a
        // strictly tighter constraint; it may not be relaxed or removed.
        (ArgPolicy::Constrain(c), ArgPolicy::Pin(v)) => {
            if value_satisfies(v, c) {
                Ok(ArgPolicy::Pin(v.clone()))
            } else {
                widen()
            }
        }
        (ArgPolicy::Constrain(base_c), ArgPolicy::Constrain(ov_c)) => {
            if constraint_is_tighter(ov_c, base_c) {
                Ok(ArgPolicy::Constrain(ov_c.clone()))
            } else {
                widen()
            }
        }
        (ArgPolicy::Constrain(_), _) => widen(),
    }
}

fn value_satisfies(value: &serde_json::Value, constraint: &Constraint) -> bool {
    match constraint {
        Constraint::Enum(allowed) => allowed.contains(value),
        Constraint::Range { min, max } => match value.as_f64() {
            Some(n) => min.is_none_or(|lo| n >= lo) && max.is_none_or(|hi| n <= hi),
            None => false,
        },
        // A pin tightening a base Pattern must itself match the base regex,
        // otherwise an overlay could pin a value the integrator walled off
        // (sealed-bounds escape). An uncompilable base pattern admits nothing.
        Constraint::Pattern(pattern) => Regex::new(pattern)
            .map(|re| value.as_str().is_some_and(|s| re.is_match(s)))
            .unwrap_or(false),
    }
}

/// Is `inner` a (non-strict) subset of `outer`? Conservative: unknown shapes
/// (e.g. differing kinds, pattern vs pattern) are treated as not-tighter unless
/// provably so, so the safe default is to reject.
fn constraint_is_tighter(inner: &Constraint, outer: &Constraint) -> bool {
    match (inner, outer) {
        (Constraint::Enum(i), Constraint::Enum(o)) => i.iter().all(|v| o.contains(v)),
        (Constraint::Enum(i), o) => i.iter().all(|v| value_satisfies(v, o)),
        (
            Constraint::Range {
                min: imin,
                max: imax,
            },
            Constraint::Range {
                min: omin,
                max: omax,
            },
        ) => {
            let lower_ok = match (imin, omin) {
                (_, None) => true,
                (Some(i), Some(o)) => *i >= *o,
                (None, Some(_)) => false,
            };
            let upper_ok = match (imax, omax) {
                (_, None) => true,
                (Some(i), Some(o)) => *i <= *o,
                (None, Some(_)) => false,
            };
            lower_ok && upper_ok
        }
        // Identical patterns are equal (allowed); anything else is not provable.
        (Constraint::Pattern(i), Constraint::Pattern(o)) => i == o,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tooldef::{ToolCall, ToolDef};
    use serde_json::json;
    use std::collections::BTreeMap;

    /// An upstream surface exposing `names`, each with one string arg `k`. Needed
    /// because the newly-admitted rename states are only *decidable* against the
    /// live surface (`validate`) or a call (`rewrite`).
    fn defs(names: &[&str]) -> Vec<ToolDef> {
        names
            .iter()
            .map(|n| {
                ToolDef::new(
                    *n,
                    None,
                    json!({"type": "object", "properties": {"k": {"type": "string"}}}),
                )
            })
            .collect()
    }

    /// A policy over several tools, keeping everything not explicitly ruled.
    fn many(entries: Vec<(&str, ToolPolicy)>) -> Policy {
        let mut tools = BTreeMap::new();
        for (name, tp) in entries {
            tools.insert(name.to_string(), tp);
        }
        Policy {
            default_presence: Presence::Keep,
            tools,
        }
    }

    /// `rename: Some(to)` and nothing else.
    fn renamed(to: &str) -> ToolPolicy {
        ToolPolicy {
            rename: Some(to.to_string()),
            ..Default::default()
        }
    }

    /// A tool pinning its `k` arg to `value` — a bound that must stay attached to
    /// this upstream tool no matter what name it projects under.
    fn pinning(value: &str) -> ToolPolicy {
        let mut args = BTreeMap::new();
        args.insert("k".to_string(), ArgPolicy::Pin(json!(value)));
        ToolPolicy {
            args,
            ..Default::default()
        }
    }

    fn one(name: &str, tp: ToolPolicy, default_presence: Presence) -> Policy {
        let mut tools = BTreeMap::new();
        tools.insert(name.to_string(), tp);
        Policy {
            default_presence,
            tools,
        }
    }

    #[test]
    fn overlay_may_drop_a_kept_tool() {
        let base = Policy::passthrough();
        let overlay = one(
            "search",
            ToolPolicy {
                presence: Some(Presence::Drop),
                ..Default::default()
            },
            Presence::Keep,
        );
        let merged = merge(&base, &overlay).unwrap();
        assert_eq!(merged.presence_of("search"), Presence::Drop);
    }

    #[test]
    fn overlay_may_not_keep_a_dropped_default() {
        let base = Policy::sealed();
        let mut overlay = Policy::passthrough();
        overlay.default_presence = Presence::Keep;
        assert!(merge(&base, &overlay).is_err());
    }

    #[test]
    fn overlay_may_not_rekeep_a_dropped_tool() {
        let base = one(
            "search",
            ToolPolicy {
                presence: Some(Presence::Drop),
                ..Default::default()
            },
            Presence::Keep,
        );
        let overlay = one(
            "search",
            ToolPolicy {
                presence: Some(Presence::Keep),
                ..Default::default()
            },
            Presence::Keep,
        );
        assert!(merge(&base, &overlay).is_err());
    }

    #[test]
    fn overlay_may_pin_an_unrestricted_arg() {
        let base = Policy::passthrough();
        let mut args = BTreeMap::new();
        args.insert("artifact".to_string(), ArgPolicy::Pin(json!("hylla")));
        let overlay = one(
            "search",
            ToolPolicy {
                args,
                ..Default::default()
            },
            Presence::Keep,
        );
        let merged = merge(&base, &overlay).unwrap();
        assert_eq!(
            merged.tools["search"].args["artifact"],
            ArgPolicy::Pin(json!("hylla"))
        );
    }

    /// The bindings' exact shape: `lagom-node`/`lagom-py` mint with
    /// `config_paths: []`, so `lagom_proxy::mint` folds the whole built policy —
    /// `rename` included — as an overlay onto `Policy::passthrough()`. Nothing
    /// bound the name, so it must land.
    #[test]
    fn overlay_may_name_a_tool_the_base_never_bound() {
        let base = Policy::passthrough();
        let overlay = one(
            "search",
            ToolPolicy {
                presence: Some(Presence::Keep),
                rename: Some("find".to_string()),
                ..Default::default()
            },
            Presence::Keep,
        );
        let merged = merge(&base, &overlay).unwrap();
        assert_eq!(
            merged.tools["search"].rename.as_deref(),
            Some("find"),
            "a rename over an unbound name must survive the merge"
        );
    }

    /// Same, but the base mentions the tool with `rename: None` — still unbound,
    /// so still nameable. Guards against keying on tool *presence* in `base.tools`
    /// instead of on the rename bound itself.
    #[test]
    fn overlay_may_name_a_listed_tool_with_no_rename_bound() {
        let base = one(
            "search",
            ToolPolicy {
                presence: Some(Presence::Keep),
                ..Default::default()
            },
            Presence::Keep,
        );
        let overlay = one(
            "search",
            ToolPolicy {
                rename: Some("find".to_string()),
                ..Default::default()
            },
            Presence::Keep,
        );
        let merged = merge(&base, &overlay).unwrap();
        assert_eq!(merged.tools["search"].rename.as_deref(), Some("find"));
    }

    /// THE SECURITY HALF: a base rename is a sealed bound. An overlay that
    /// redirects it would re-point the integrator's downstream vocabulary at a
    /// different upstream tool — rejected, and the base name must be untouched.
    #[test]
    fn overlay_may_not_redirect_a_sealed_rename() {
        let base = one(
            "search",
            ToolPolicy {
                presence: Some(Presence::Keep),
                rename: Some("find".to_string()),
                ..Default::default()
            },
            Presence::Keep,
        );
        let overlay = one(
            "search",
            ToolPolicy {
                rename: Some("admin".to_string()),
                ..Default::default()
            },
            Presence::Keep,
        );
        let err = merge(&base, &overlay).unwrap_err();
        assert!(
            err.message.contains("may not rename tool `search`"),
            "{}",
            err.message
        );
    }

    /// An overlay restating the identical rename is a no-op, not a widening.
    #[test]
    fn overlay_may_restate_an_identical_rename() {
        let base = one(
            "search",
            ToolPolicy {
                rename: Some("find".to_string()),
                ..Default::default()
            },
            Presence::Keep,
        );
        let overlay = one(
            "search",
            ToolPolicy {
                rename: Some("find".to_string()),
                ..Default::default()
            },
            Presence::Keep,
        );
        let merged = merge(&base, &overlay).unwrap();
        assert_eq!(merged.tools["search"].rename.as_deref(), Some("find"));
    }

    /// A silent overlay may not erase a sealed rename: `None` means "no opinion",
    /// so the base's name holds. Reverting to the upstream name would re-expose
    /// vocabulary the integrator deliberately hid.
    #[test]
    fn overlay_silence_cannot_erase_a_sealed_rename() {
        let base = one(
            "search",
            ToolPolicy {
                rename: Some("find".to_string()),
                ..Default::default()
            },
            Presence::Keep,
        );
        let overlay = one(
            "search",
            ToolPolicy {
                presence: Some(Presence::Keep),
                ..Default::default()
            },
            Presence::Keep,
        );
        let merged = merge(&base, &overlay).unwrap();
        assert_eq!(merged.tools["search"].rename.as_deref(), Some("find"));
    }

    /// A rename never buys presence: a sealed-by-default base still drops a tool
    /// the overlay only renamed, so naming cannot be a re-exposure vector.
    #[test]
    fn rename_does_not_resurrect_a_dropped_tool() {
        let base = Policy::sealed();
        let mut overlay = Policy::sealed();
        overlay.tools.insert(
            "search".to_string(),
            ToolPolicy {
                rename: Some("find".to_string()),
                ..Default::default()
            },
        );
        let merged = merge(&base, &overlay).unwrap();
        assert_eq!(merged.presence_of("search"), Presence::Drop);
    }

    #[test]
    fn overlay_may_tighten_enum_to_subset() {
        let mut bargs = BTreeMap::new();
        bargs.insert(
            "artifact".to_string(),
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("a"), json!("b"), json!("c")])),
        );
        let base = one(
            "search",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );

        let mut oargs = BTreeMap::new();
        oargs.insert(
            "artifact".to_string(),
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("a")])),
        );
        let overlay = one(
            "search",
            ToolPolicy {
                args: oargs,
                ..Default::default()
            },
            Presence::Keep,
        );

        assert!(merge(&base, &overlay).is_ok());
    }

    #[test]
    fn overlay_may_not_widen_enum() {
        let mut bargs = BTreeMap::new();
        bargs.insert(
            "artifact".to_string(),
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("a")])),
        );
        let base = one(
            "search",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );

        let mut oargs = BTreeMap::new();
        oargs.insert(
            "artifact".to_string(),
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("a"), json!("z")])),
        );
        let overlay = one(
            "search",
            ToolPolicy {
                args: oargs,
                ..Default::default()
            },
            Presence::Keep,
        );

        assert!(merge(&base, &overlay).is_err());
    }

    #[test]
    fn overlay_may_not_unpin() {
        let mut bargs = BTreeMap::new();
        bargs.insert("artifact".to_string(), ArgPolicy::Pin(json!("hylla")));
        let base = one(
            "search",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );

        let mut oargs = BTreeMap::new();
        oargs.insert("artifact".to_string(), ArgPolicy::Passthrough);
        let overlay = one(
            "search",
            ToolPolicy {
                args: oargs,
                ..Default::default()
            },
            Presence::Keep,
        );

        assert!(merge(&base, &overlay).is_err());
    }

    #[test]
    fn overlay_may_not_pin_outside_base_pattern() {
        let mut bargs = BTreeMap::new();
        bargs.insert(
            "path".to_string(),
            ArgPolicy::Constrain(Constraint::Pattern("^src/".to_string())),
        );
        let base = one(
            "edit",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );
        let mut oargs = BTreeMap::new();
        oargs.insert("path".to_string(), ArgPolicy::Pin(json!("/etc/passwd")));
        let overlay = one(
            "edit",
            ToolPolicy {
                args: oargs,
                ..Default::default()
            },
            Presence::Keep,
        );
        assert!(merge(&base, &overlay).is_err());
    }

    // ---------------------------------------------------------------------
    // States the relaxed `(None, Some(new))` rename arm newly ADMITS. Before the
    // relaxation the merged `rename` was always `base.rename`, so no
    // overlay-authored name could reach the projected surface at all. Each state
    // below is therefore reachable only now, and each must be shown safe or shown
    // rejected — case numbering matches the enumeration on the rename match.
    // ---------------------------------------------------------------------

    /// Case 2a: the overlay names a kept tool onto ANOTHER kept tool's upstream
    /// name. `merge` accepts (nothing was bound), and the shadow is rejected by
    /// `validate`'s collision check — the defense that runs on every serve path.
    /// The non-colliding control proves the rejection is caused by the collision,
    /// not by unrelated drift.
    #[test]
    fn overlay_rename_onto_another_kept_tools_name_is_caught_as_a_collision() {
        let up = defs(&["danger", "safe"]);
        let base = Policy::passthrough();

        let control = merge(&base, &many(vec![("danger", renamed("harmless"))])).unwrap();
        assert!(
            crate::validate::validate(&control, &up).is_ok(),
            "a rename onto a free name must validate"
        );

        let shadow = merge(&base, &many(vec![("danger", renamed("safe"))])).unwrap();
        assert_eq!(shadow.tools["danger"].rename.as_deref(), Some("safe"));
        let errs = crate::validate::validate(&shadow, &up).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("collides")),
            "{errs:?}"
        );
    }

    /// Case 2b — the vocabulary HIJACK: the base sealed `search`'s downstream name
    /// as `find`; the overlay points a different tool at `find` without touching
    /// `search.rename`, so no per-tool rename comparison can see it. `validate`
    /// must reject, otherwise `rewrite` would route `find` to `admin`
    /// (BTreeMap order) and the integrator's sealed name would be hijacked.
    #[test]
    fn overlay_rename_may_not_hijack_a_sealed_rename_target() {
        let up = defs(&["admin", "search"]);
        let base = many(vec![("search", renamed("find"))]);

        let control = merge(&base, &many(vec![("admin", renamed("ops"))])).unwrap();
        assert!(crate::validate::validate(&control, &up).is_ok());

        let hijack = merge(&base, &many(vec![("admin", renamed("find"))])).unwrap();
        assert_eq!(hijack.tools["search"].rename.as_deref(), Some("find"));
        let errs = crate::validate::validate(&hijack, &up).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("collides")),
            "{errs:?}"
        );
    }

    /// Case 2c: two overlay renames aiming at one new name. Neither collides with
    /// an upstream name, so only the projected-name map catches it.
    #[test]
    fn two_overlay_renames_onto_one_name_are_caught_as_a_collision() {
        let up = defs(&["alpha", "beta"]);
        let merged = merge(
            &Policy::passthrough(),
            &many(vec![("alpha", renamed("x")), ("beta", renamed("x"))]),
        )
        .unwrap();
        let errs = crate::validate::validate(&merged, &up).unwrap_err();
        assert!(
            errs.iter().any(|e| e.message.contains("collides")),
            "{errs:?}"
        );
    }

    /// Case 3: the overlay renames a DROPPED tool onto a kept tool's name.
    /// `validate` skips dropped tools, so this state is admitted — and it is safe
    /// because it is fail-CLOSED: `rewrite::resolve` matches the rename before the
    /// direct lookup and returns `None` for a dropped target, so the shadowed name
    /// stops resolving instead of resolving to the hidden tool. Availability lost,
    /// authority never gained.
    #[test]
    fn rename_from_a_dropped_tool_shadows_fail_closed() {
        let up = defs(&["search", "write_file"]);
        let base = many(vec![
            ("search", ToolPolicy::default()),
            (
                "write_file",
                ToolPolicy {
                    presence: Some(Presence::Drop),
                    ..Default::default()
                },
            ),
        ]);
        let merged = merge(&base, &many(vec![("write_file", renamed("search"))])).unwrap();

        // Admitted: no drift error, because a dropped tool projects no name.
        assert!(crate::validate::validate(&merged, &up).is_ok());
        // Fail-closed: the shadowed name resolves to nothing, never to `write_file`.
        let reject = crate::rewrite::rewrite(&ToolCall::new("search", json!({})), &merged)
            .expect_err("the shadowed name must not resolve");
        assert!(reject.message.contains("search"), "{}", reject.message);
        // And the dropped tool is unreachable under either name.
        assert!(crate::rewrite::rewrite(&ToolCall::new("write_file", json!({})), &merged).is_err());
    }

    /// Case 4: a rename swap/chain (`alpha`→`beta`, `beta`→`gamma`) projects
    /// distinct names, so `validate` admits it. Safe because bounds are keyed by
    /// UPSTREAM tool: the projected name `beta` reaches `alpha` and gets ALPHA's
    /// pin, not beta's. A rename can neither detach a bound nor borrow another
    /// tool's — the property that makes rename vocabulary rather than authority.
    #[test]
    fn rename_swap_keeps_every_bound_attached_to_its_upstream_tool() {
        let up = defs(&["alpha", "beta"]);
        let base = many(vec![("alpha", pinning("A")), ("beta", pinning("B"))]);
        let merged = merge(
            &base,
            &many(vec![("alpha", renamed("beta")), ("beta", renamed("gamma"))]),
        )
        .unwrap();
        assert!(crate::validate::validate(&merged, &up).is_ok());

        let via_beta =
            crate::rewrite::rewrite(&ToolCall::new("beta", json!({"k": "agent"})), &merged)
                .unwrap();
        assert_eq!(via_beta.name, "alpha");
        assert_eq!(via_beta.arguments["k"], json!("A"));

        let via_gamma =
            crate::rewrite::rewrite(&ToolCall::new("gamma", json!({"k": "agent"})), &merged)
                .unwrap();
        assert_eq!(via_gamma.name, "beta");
        assert_eq!(via_gamma.arguments["k"], json!("B"));

        // `alpha`'s own name is hidden once it is renamed away.
        assert!(crate::rewrite::rewrite(&ToolCall::new("alpha", json!({})), &merged).is_err());
    }

    /// Case 1, degenerate: renaming a tool to its own upstream name. Must stay a
    /// no-op — not a self-collision, and not a lockout of the tool.
    #[test]
    fn overlay_may_rename_a_tool_to_its_own_upstream_name() {
        let up = defs(&["search"]);
        let merged = merge(
            &Policy::passthrough(),
            &many(vec![("search", renamed("search"))]),
        )
        .unwrap();
        assert!(crate::validate::validate(&merged, &up).is_ok());
        let out =
            crate::rewrite::rewrite(&ToolCall::new("search", json!({"k": "x"})), &merged).unwrap();
        assert_eq!(out.name, "search");
    }

    /// The sealing property under the relaxed arm: a base PIN survives an
    /// overlay-authored rename and still applies through the new name. If a rename
    /// could shake off the pin, naming would become an authority escape.
    #[test]
    fn an_overlay_rename_cannot_detach_a_base_pin() {
        let base = many(vec![("search", pinning("hylla"))]);
        let merged = merge(&base, &many(vec![("search", renamed("find"))])).unwrap();
        assert_eq!(
            merged.tools["search"].args["k"],
            ArgPolicy::Pin(json!("hylla"))
        );

        // Pinned arg pruned from the projected schema under the NEW name.
        let projected = crate::project::project(&defs(&["search"]), &merged);
        assert_eq!(projected[0].name, "find");
        assert!(projected[0].input_schema["properties"].get("k").is_none());

        // ...and re-injected on the way back, so the bound is inescapable.
        let out = crate::rewrite::rewrite(&ToolCall::new("find", json!({"k": "escape"})), &merged)
            .unwrap();
        assert_eq!(out.name, "search");
        assert_eq!(out.arguments["k"], json!("hylla"));
    }

    // ---------------------------------------------------------------------
    // The "unbound vs. deliberately bound" line, field by field — the sealing
    // property must hold for every field, not just `rename`.
    // ---------------------------------------------------------------------

    /// A base `Constrain` is a deliberate domain bound: an overlay may not trade it
    /// for a `Default`, which restricts nothing (`Default` only fills an omitted
    /// argument, leaving the agent free to send any value).
    #[test]
    fn overlay_may_not_replace_a_constraint_with_a_default() {
        let mut bargs = BTreeMap::new();
        bargs.insert(
            "k".to_string(),
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("a")])),
        );
        let base = one(
            "search",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );
        let mut oargs = BTreeMap::new();
        oargs.insert("k".to_string(), ArgPolicy::Default(json!("a")));
        let overlay = one(
            "search",
            ToolPolicy {
                args: oargs,
                ..Default::default()
            },
            Presence::Keep,
        );
        let err = merge(&base, &overlay).unwrap_err();
        assert!(
            err.message.contains("widens argument `k`"),
            "{}",
            err.message
        );
    }

    /// Same bound, erased rather than swapped: `Constrain` → `Passthrough` removes
    /// the domain bound outright and must be rejected.
    #[test]
    fn overlay_may_not_erase_a_constraint_with_passthrough() {
        let mut bargs = BTreeMap::new();
        bargs.insert(
            "k".to_string(),
            ArgPolicy::Constrain(Constraint::Range {
                min: Some(1.0),
                max: Some(5.0),
            }),
        );
        let base = one(
            "search",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );
        let mut oargs = BTreeMap::new();
        oargs.insert("k".to_string(), ArgPolicy::Passthrough);
        let overlay = one(
            "search",
            ToolPolicy {
                args: oargs,
                ..Default::default()
            },
            Presence::Keep,
        );
        assert!(merge(&base, &overlay).is_err());
    }

    /// A base `Pin` is maximally narrow, so every non-identical overlay shape is a
    /// widening — including `Constrain` (re-exposes the argument) and `Default`
    /// (makes the value agent-settable again).
    #[test]
    fn overlay_may_not_soften_a_pin_to_a_constraint_or_default() {
        let mut bargs = BTreeMap::new();
        bargs.insert("k".to_string(), ArgPolicy::Pin(json!("hylla")));
        let base = one(
            "search",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );

        for softer in [
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("hylla")])),
            ArgPolicy::Default(json!("hylla")),
            ArgPolicy::Pin(json!("other")),
        ] {
            let mut oargs = BTreeMap::new();
            oargs.insert("k".to_string(), softer.clone());
            let overlay = one(
                "search",
                ToolPolicy {
                    args: oargs,
                    ..Default::default()
                },
                Presence::Keep,
            );
            assert!(
                merge(&base, &overlay).is_err(),
                "a pin must not soften to {softer:?}"
            );
        }
    }

    /// The other side of the line: a base `Default` is NOT a domain bound (the
    /// agent may already send any value), so an overlay may replace or drop it.
    /// Documented by test so it is never mistaken for a loosening.
    #[test]
    fn a_base_default_is_not_a_bound_an_overlay_can_loosen() {
        let mut bargs = BTreeMap::new();
        bargs.insert("k".to_string(), ArgPolicy::Default(json!("base")));
        let base = one(
            "search",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );

        for replacement in [
            ArgPolicy::Passthrough,
            ArgPolicy::Default(json!("overlay")),
            ArgPolicy::Pin(json!("overlay")),
        ] {
            let mut oargs = BTreeMap::new();
            oargs.insert("k".to_string(), replacement.clone());
            let overlay = one(
                "search",
                ToolPolicy {
                    args: oargs,
                    ..Default::default()
                },
                Presence::Keep,
            );
            let merged = merge(&base, &overlay).unwrap();
            assert_eq!(merged.tools["search"].args["k"], replacement);
        }
    }

    /// Description: a base `Override` is a set bound — overlay silence may not
    /// revert it to `Passthrough` and re-expose the upstream text. An unbound
    /// (`Passthrough`) base takes the overlay's slimmer text.
    #[test]
    fn overlay_silence_cannot_revert_a_base_description_override() {
        let base = one(
            "search",
            ToolPolicy {
                description: DescriptionPolicy::Override("slim".into()),
                ..Default::default()
            },
            Presence::Keep,
        );
        let silent = one("search", ToolPolicy::default(), Presence::Keep);
        assert_eq!(
            merge(&base, &silent).unwrap().tools["search"].description,
            DescriptionPolicy::Override("slim".into())
        );

        let unbound = one("search", ToolPolicy::default(), Presence::Keep);
        let slimming = one(
            "search",
            ToolPolicy {
                description: DescriptionPolicy::Override("slimmer".into()),
                ..Default::default()
            },
            Presence::Keep,
        );
        assert_eq!(
            merge(&unbound, &slimming).unwrap().tools["search"].description,
            DescriptionPolicy::Override("slimmer".into())
        );
    }

    /// Presence, the unbound case: a base tool with `presence: None` carries no
    /// per-tool bound, so it resolves against the merged (overlay) default — which
    /// `merge` already proved narrower-or-equal. Narrowing the default therefore
    /// drops it, and can never re-keep anything.
    #[test]
    fn an_unbound_tool_presence_narrows_with_the_overlay_default() {
        let base = one("search", ToolPolicy::default(), Presence::Keep);
        let mut overlay = Policy::passthrough();
        overlay.default_presence = Presence::Drop;
        let merged = merge(&base, &overlay).unwrap();
        assert_eq!(merged.presence_of("search"), Presence::Drop);
    }

    #[test]
    fn overlay_may_pin_inside_base_pattern() {
        let mut bargs = BTreeMap::new();
        bargs.insert(
            "path".to_string(),
            ArgPolicy::Constrain(Constraint::Pattern("^src/".to_string())),
        );
        let base = one(
            "edit",
            ToolPolicy {
                args: bargs,
                ..Default::default()
            },
            Presence::Keep,
        );
        let mut oargs = BTreeMap::new();
        oargs.insert("path".to_string(), ArgPolicy::Pin(json!("src/main.rs")));
        let overlay = one(
            "edit",
            ToolPolicy {
                args: oargs,
                ..Default::default()
            },
            Presence::Keep,
        );
        assert!(merge(&base, &overlay).is_ok());
    }
}
