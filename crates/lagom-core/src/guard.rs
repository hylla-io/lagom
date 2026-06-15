//! The brandable one-call helper (`SPEC.md` §2, §7.2; `CONTEXT.md` "Projection").
//!
//! [`Guard`] pairs a frozen [`Policy`] with the upstream surface it narrows so an
//! integrator wires a slim MCP in two lines: build it from the app's own tool
//! defs + a policy, register [`Guard::slim_defs`] as the downstream `tools/list`,
//! and run every incoming `tools/call` through [`Guard::gate`]. No call into
//! [`crate::project`] / [`crate::rewrite`] plumbing, no policy threading at each
//! call site.
//!
//! lagom stays **invisible**: the branding (downstream tool names, slim
//! descriptions, which tools exist at all) lives entirely in the `Policy` the app
//! supplies — `rename` sets the app's own tool names, `Override` descriptions set
//! the app's own docs, `default_presence`/`drop` decide the surface. The app
//! reads the policy from wherever it likes (its own config, a builder, a JSON
//! string); the helper itself reads nothing (`SPEC.md` "the lib reads NOTHING by
//! self. you feed it.").
//!
//! This is the canonical helper shape; the Python, Node, and Go bindings mirror
//! it exactly (NO DRIFT) so an app gets the same two-call ergonomics in any
//! language.

use crate::policy::Policy;
use crate::project::project;
use crate::rewrite::{Reject, rewrite};
use crate::tooldef::{ToolCall, ToolDef};

/// A frozen projection an integrator gates an MCP through (`SPEC.md` §2).
///
/// Construct one with [`Guard::new`] from the upstream tool defs plus the policy
/// to enforce. It eagerly computes the slim downstream surface ([`slim_defs`])
/// once and gates every call with [`gate`]. The `Policy` carries all branding, so
/// the same `Guard` API serves any app without exposing lagom.
///
/// [`slim_defs`]: Guard::slim_defs
/// [`gate`]: Guard::gate
#[derive(Debug, Clone)]
pub struct Guard {
    policy: Policy,
    slim_defs: Vec<ToolDef>,
}

impl Guard {
    /// Build a guard from an upstream surface and the policy that narrows it.
    ///
    /// `upstream` is the app's full tool defs (its real `tools/list`); `policy`
    /// is the projection to enforce — authored by the app from anywhere (builder,
    /// its own config, a JSON string), carrying the app's branding. The slim
    /// downstream surface is projected once here and cached for [`slim_defs`].
    ///
    /// [`slim_defs`]: Guard::slim_defs
    pub fn new(upstream: &[ToolDef], policy: Policy) -> Self {
        let slim_defs = project(upstream, &policy);
        Self { policy, slim_defs }
    }

    /// The slim downstream tool defs to advertise as `tools/list`.
    ///
    /// Already branded by the policy (renamed names, override/addendum
    /// descriptions, dropped tools omitted, pinned args pruned from each schema).
    /// Register these as the MCP's tools; nothing here names lagom.
    pub fn slim_defs(&self) -> &[ToolDef] {
        &self.slim_defs
    }

    /// Gate one incoming downstream `tools/call`.
    ///
    /// `call` uses the *downstream* (post-rename) tool name and the args the agent
    /// supplied through the slim schema. Returns the upstream call to forward —
    /// pinned values injected, defaults filled, the name mapped back to upstream —
    /// or a [`Reject`] (unknown/dropped tool, violated constraint) carrying the
    /// reason to annotate back to the agent (`SPEC.md` §9.1 — never swallow).
    pub fn gate(&self, call: &ToolCall) -> Result<ToolCall, Reject> {
        rewrite(call, &self.policy)
    }

    /// Borrow the frozen policy this guard enforces (e.g. to `validate` it against
    /// a freshly probed upstream, or to record it for refire).
    pub fn policy(&self) -> &Policy {
        &self.policy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upstream() -> Vec<ToolDef> {
        vec![
            ToolDef::new(
                "search",
                Some("Search.".into()),
                json!({
                    "type": "object",
                    "properties": {
                        "artifact": {"type": "string"},
                        "query": {"type": "string"}
                    },
                    "required": ["artifact", "query"]
                }),
            ),
            ToolDef::new(
                "write_file",
                Some("Write.".into()),
                json!({"type": "object"}),
            ),
        ]
    }

    /// A sealed, branded policy: keep `search` as the app's own `find` name with
    /// the app's own description, pin `artifact`, drop everything else.
    fn branded_policy() -> Policy {
        serde_json::from_value(json!({
            "default_presence": "drop",
            "tools": {
                "search": {
                    "presence": "keep",
                    "rename": "find",
                    "description": {"override": "App find."},
                    "args": {"artifact": {"pin": "hylla"}}
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn slim_defs_are_branded_and_narrowed() {
        let g = Guard::new(&upstream(), branded_policy());
        let defs = g.slim_defs();

        assert_eq!(defs.len(), 1, "write_file must be dropped");
        // Branding: the app's own name + description, not lagom's, not upstream's.
        assert_eq!(defs[0].name, "find");
        assert_eq!(defs[0].description.as_deref(), Some("App find."));
        // Pinned arg pruned, non-pinned arg kept.
        let props = &defs[0].input_schema["properties"];
        assert!(props.get("artifact").is_none(), "pinned arg must be pruned");
        assert!(props.get("query").is_some());
    }

    #[test]
    fn gate_injects_pin_under_renamed_call() {
        let g = Guard::new(&upstream(), branded_policy());
        // The agent calls the *branded* name with only what it can see.
        let out = g
            .gate(&ToolCall::new("find", json!({"query": "x"})))
            .unwrap();
        // Mapped back to the upstream name with the pin injected.
        assert_eq!(out.name, "search");
        assert_eq!(out.arguments["artifact"], json!("hylla"));
    }

    #[test]
    fn gate_rejects_dropped_tool() {
        let g = Guard::new(&upstream(), branded_policy());
        // Dropped upstream tool is not gateable, and the un-renamed original name
        // is hidden too.
        assert!(g.gate(&ToolCall::new("write_file", json!({}))).is_err());
        assert!(g.gate(&ToolCall::new("search", json!({}))).is_err());
    }
}
