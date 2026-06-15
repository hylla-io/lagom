//! Forward transform: `project(upstream, policy) -> projected` (`SPEC.md` §4).
//!
//! Operates on the JSON Schema deterministically — pinned args are deleted from
//! the schema, constrained args are tightened — so the consumer cannot even
//! *form* a disallowed call. Descriptions follow the Tier ladder (§4.2).

use serde_json::{Map, Value};

use crate::policy::{ArgPolicy, Constraint, DescriptionPolicy, Policy, Presence, ToolPolicy};
use crate::tooldef::ToolDef;

/// Project an upstream tool surface through a policy, returning the narrowed
/// surface the consumer sees. Tools resolved to [`Presence::Drop`] are omitted.
pub fn project(upstream: &[ToolDef], policy: &Policy) -> Vec<ToolDef> {
    upstream
        .iter()
        .filter_map(|tool| project_tool(tool, policy))
        .collect()
}

fn project_tool(tool: &ToolDef, policy: &Policy) -> Option<ToolDef> {
    if policy.presence_of(&tool.name) == Presence::Drop {
        return None;
    }
    let Some(tp) = policy.tools.get(&tool.name) else {
        // No explicit rule and kept by default: passthrough, unchanged.
        return Some(tool.clone());
    };

    let mut schema = tool.input_schema.clone();
    let mut restrictions = Vec::new();
    apply_arg_transforms(&mut schema, &tp.args, &mut restrictions);

    Some(ToolDef {
        name: tp.rename.clone().unwrap_or_else(|| tool.name.clone()),
        description: build_description(tool.description.as_deref(), tp, &restrictions),
        input_schema: schema,
    })
}

/// Apply per-argument transforms to a JSON Schema object in place, recording a
/// human-readable note for each restriction (consumed by the addendum builder).
fn apply_arg_transforms(
    schema: &mut Value,
    args: &std::collections::BTreeMap<String, ArgPolicy>,
    restrictions: &mut Vec<String>,
) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    for (arg, policy) in args {
        match policy {
            ArgPolicy::Pin(_) => {
                remove_property(obj, arg);
                remove_required(obj, arg);
                restrictions.push(format!("`{arg}` is fixed"));
            }
            ArgPolicy::Default(value) => {
                if let Some(prop) = property_mut(obj, arg) {
                    prop.insert("default".to_string(), value.clone());
                }
            }
            ArgPolicy::Constrain(constraint) => {
                if let Some(prop) = property_mut(obj, arg) {
                    apply_constraint(prop, constraint);
                    restrictions.push(describe_constraint(arg, constraint));
                }
            }
            ArgPolicy::Passthrough => {}
        }
    }
}

fn apply_constraint(prop: &mut Map<String, Value>, constraint: &Constraint) {
    match constraint {
        Constraint::Enum(values) => {
            prop.insert("enum".to_string(), Value::Array(values.clone()));
        }
        Constraint::Range { min, max } => {
            if let Some(min) = min
                && let Some(n) = serde_json::Number::from_f64(*min)
            {
                prop.insert("minimum".to_string(), Value::Number(n));
            }
            if let Some(max) = max
                && let Some(n) = serde_json::Number::from_f64(*max)
            {
                prop.insert("maximum".to_string(), Value::Number(n));
            }
        }
        Constraint::Pattern(pattern) => {
            prop.insert("pattern".to_string(), Value::String(pattern.clone()));
        }
    }
}

pub(crate) fn describe_constraint(arg: &str, constraint: &Constraint) -> String {
    match constraint {
        Constraint::Enum(values) => {
            let rendered: Vec<String> = values.iter().map(render_value).collect();
            format!("`{arg}` ∈ {{{}}}", rendered.join(", "))
        }
        Constraint::Range { min, max } => match (min, max) {
            (Some(lo), Some(hi)) => format!("`{arg}` ∈ [{lo}, {hi}]"),
            (Some(lo), None) => format!("`{arg}` ≥ {lo}"),
            (None, Some(hi)) => format!("`{arg}` ≤ {hi}"),
            (None, None) => format!("`{arg}` is constrained"),
        },
        Constraint::Pattern(pattern) => format!("`{arg}` matches /{pattern}/"),
    }
}

fn render_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Build the projected description per the Tier ladder (`SPEC.md` §4.2):
/// an override is taken verbatim; otherwise the original is passed through with a
/// deterministic addendum when restrictions were applied.
fn build_description(
    original: Option<&str>,
    tp: &ToolPolicy,
    restrictions: &[String],
) -> Option<String> {
    match &tp.description {
        DescriptionPolicy::Override(text) => Some(text.clone()),
        DescriptionPolicy::Passthrough => {
            let base = original.unwrap_or("").to_string();
            if restrictions.is_empty() {
                original.map(str::to_string)
            } else {
                let addendum = format!("Restricted: {}.", restrictions.join("; "));
                if base.is_empty() {
                    Some(addendum)
                } else {
                    Some(format!("{base}\n\n{addendum}"))
                }
            }
        }
    }
}

fn property_mut<'a>(
    obj: &'a mut Map<String, Value>,
    arg: &str,
) -> Option<&'a mut Map<String, Value>> {
    obj.get_mut("properties")?
        .as_object_mut()?
        .get_mut(arg)?
        .as_object_mut()
}

fn remove_property(obj: &mut Map<String, Value>, arg: &str) {
    if let Some(props) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        props.remove(arg);
    }
}

fn remove_required(obj: &mut Map<String, Value>, arg: &str) {
    if let Some(Value::Array(required)) = obj.get_mut("required") {
        required.retain(|v| v.as_str() != Some(arg));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ToolPolicy;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn tool(name: &str) -> ToolDef {
        ToolDef::new(
            name,
            Some("Search the database.".to_string()),
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "artifact": {"type": "string"},
                    "limit": {"type": "integer"}
                },
                "required": ["query", "artifact"]
            }),
        )
    }

    fn policy_with(name: &str, tp: ToolPolicy) -> Policy {
        let mut tools = BTreeMap::new();
        tools.insert(name.to_string(), tp);
        Policy {
            default_presence: Presence::Keep,
            tools,
        }
    }

    #[test]
    fn passthrough_keeps_tool_unchanged() {
        let upstream = vec![tool("search")];
        let out = project(&upstream, &Policy::passthrough());
        assert_eq!(out, upstream);
    }

    #[test]
    fn sealed_drops_unlisted_tools() {
        let upstream = vec![tool("search")];
        let out = project(&upstream, &Policy::sealed());
        assert!(out.is_empty());
    }

    #[test]
    fn drop_removes_tool() {
        let tp = ToolPolicy {
            presence: Some(Presence::Drop),
            ..Default::default()
        };
        let out = project(&[tool("search")], &policy_with("search", tp));
        assert!(out.is_empty());
    }

    #[test]
    fn pin_removes_arg_from_schema_and_required() {
        let mut args = BTreeMap::new();
        args.insert("artifact".to_string(), ArgPolicy::Pin(json!("hylla")));
        let tp = ToolPolicy {
            args,
            ..Default::default()
        };
        let out = project(&[tool("search")], &policy_with("search", tp));
        let schema = &out[0].input_schema;
        assert!(schema["properties"].get("artifact").is_none());
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|v| v == "artifact"));
        // The consumer is told it is fixed.
        assert!(
            out[0]
                .description
                .as_ref()
                .unwrap()
                .contains("`artifact` is fixed")
        );
    }

    #[test]
    fn constrain_enum_sets_schema_enum() {
        let mut args = BTreeMap::new();
        args.insert(
            "artifact".to_string(),
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("a"), json!("b")])),
        );
        let tp = ToolPolicy {
            args,
            ..Default::default()
        };
        let out = project(&[tool("search")], &policy_with("search", tp));
        let en = out[0].input_schema["properties"]["artifact"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(en, &vec![json!("a"), json!("b")]);
        assert!(
            out[0]
                .description
                .as_ref()
                .unwrap()
                .contains("`artifact` ∈ {a, b}")
        );
    }

    // Invisibility: the deterministic addendum names the restriction, never the
    // engine that imposed it. The agent must not see "lagom" in any projected
    // description (SPEC.md §4.2 Tier 2).
    #[test]
    fn addendum_is_brand_free() {
        let mut args = BTreeMap::new();
        args.insert("token".to_string(), ArgPolicy::Pin(json!("LOCKED")));
        let tp = ToolPolicy {
            args,
            ..Default::default()
        };
        let out = project(&[tool("echo")], &policy_with("echo", tp));
        let desc = out[0].description.as_ref().unwrap();
        assert!(desc.contains("Restricted:"), "addendum missing: {desc}");
        assert!(
            !desc.to_lowercase().contains("lagom"),
            "projected description leaks the brand: {desc}"
        );
    }

    #[test]
    fn constrain_range_sets_bounds() {
        let mut args = BTreeMap::new();
        args.insert(
            "limit".to_string(),
            ArgPolicy::Constrain(Constraint::Range {
                min: Some(1.0),
                max: Some(10.0),
            }),
        );
        let tp = ToolPolicy {
            args,
            ..Default::default()
        };
        let out = project(&[tool("search")], &policy_with("search", tp));
        let prop = &out[0].input_schema["properties"]["limit"];
        assert_eq!(prop["minimum"], json!(1.0));
        assert_eq!(prop["maximum"], json!(10.0));
    }

    #[test]
    fn override_description_takes_precedence_without_addendum() {
        let mut args = BTreeMap::new();
        args.insert("artifact".to_string(), ArgPolicy::Pin(json!("hylla")));
        let tp = ToolPolicy {
            description: DescriptionPolicy::Override("Search the project.".to_string()),
            args,
            ..Default::default()
        };
        let out = project(&[tool("search")], &policy_with("search", tp));
        assert_eq!(out[0].description.as_deref(), Some("Search the project."));
    }

    #[test]
    fn rename_changes_projected_name() {
        let tp = ToolPolicy {
            rename: Some("find".to_string()),
            ..Default::default()
        };
        let out = project(&[tool("search")], &policy_with("search", tp));
        assert_eq!(out[0].name, "find");
    }
}
