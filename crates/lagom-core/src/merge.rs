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

    if let Some(rename) = &overlay.rename
        && base.rename.as_ref() != Some(rename)
    {
        return Err(MergeError::new(format!(
            "overlay may not rename tool `{name}`"
        )));
    }

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
        rename: base.rename.clone(),
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
    use serde_json::json;
    use std::collections::BTreeMap;

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
