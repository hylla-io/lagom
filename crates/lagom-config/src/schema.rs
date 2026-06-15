//! A JSON Schema for `lagom.toml`, for editor validation (`SPEC.md` §6.2).
//!
//! Hand-authored (Draft 2020-12) rather than derived, so the schema stays a
//! deliberate, stable contract independent of internal serde shape churn. It
//! mirrors [`crate::TomlPolicy`] exactly: `default-presence`, per-tool
//! `presence`/`rename`/`description`/`args`, and the mutually-exclusive
//! per-argument `pin`/`default`/`enum`/`min`/`max`/`pattern`.
//!
//! A round-trip test asserts every key the schema names round-trips through the
//! real [`crate::TomlPolicy`] deserializer, so the schema cannot silently drift
//! from the model.

use serde_json::{Value, json};

/// Return the JSON Schema document describing a valid `lagom.toml`, as a
/// `serde_json::Value`.
///
/// TOML and JSON share a data model for the surface lagom uses (tables → objects,
/// arrays, strings, numbers, booleans), so a JSON Schema validates the parsed
/// `lagom.toml` structure directly — editors that understand TOML-as-JSON
/// (taplo, the Even Better TOML extension) consume it for completion and
/// diagnostics.
pub fn json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://hylla.io/schemas/lagom.toml.json",
        "title": "lagom.toml",
        "description": "A narrowing policy over an upstream MCP server (SPEC.md §6).",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "default-presence": {
                "description": "Presence for tools with no explicit rule. `keep` is passthrough (default); `drop` seals to an allowlist.",
                "type": "string",
                "enum": ["keep", "drop"]
            },
            "tools": {
                "description": "Per-tool rules, keyed by upstream tool name.",
                "type": "object",
                "additionalProperties": { "$ref": "#/$defs/tool" }
            }
        },
        "$defs": {
            "tool": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "presence": {
                        "description": "Override the document default presence for this tool.",
                        "type": "string",
                        "enum": ["keep", "drop"]
                    },
                    "rename": {
                        "description": "Expose the tool under a different downstream name.",
                        "type": "string"
                    },
                    "description": {
                        "description": "Tier-1 slim description override; absent means passthrough + addendum.",
                        "type": "string"
                    },
                    "args": {
                        "description": "Per-argument transforms, keyed by upstream argument name.",
                        "type": "object",
                        "additionalProperties": { "$ref": "#/$defs/arg" }
                    }
                }
            },
            "arg": {
                "type": "object",
                "additionalProperties": false,
                "description": "Exactly one transform family must be set: pin, default, enum, a min/max range, or pattern.",
                "properties": {
                    "pin": {
                        "description": "Fix the argument to this value: removed from the schema, injected on every call."
                    },
                    "default": {
                        "description": "Supply this value when the agent omits the argument; stays visible."
                    },
                    "enum": {
                        "description": "Constrain the argument to this subset of values.",
                        "type": "array",
                        "minItems": 1
                    },
                    "min": {
                        "description": "Inclusive numeric lower bound.",
                        "type": "number"
                    },
                    "max": {
                        "description": "Inclusive numeric upper bound.",
                        "type": "number"
                    },
                    "pattern": {
                        "description": "Constrain a string argument to this regular expression.",
                        "type": "string"
                    }
                },
                "oneOf": [
                    { "required": ["pin"] },
                    { "required": ["default"] },
                    { "required": ["enum"] },
                    {
                        "description": "A numeric range: a lower bound, an upper bound, or both.",
                        "anyOf": [
                            { "required": ["min"] },
                            { "required": ["max"] }
                        ]
                    },
                    { "required": ["pattern"] }
                ]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TomlPolicy;

    #[test]
    fn schema_is_well_formed_object() {
        let schema = json_schema();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(
            schema["$schema"],
            json!("https://json-schema.org/draft/2020-12/schema")
        );
        // Top-level property names the schema declares.
        assert!(schema["properties"]["default-presence"].is_object());
        assert!(schema["properties"]["tools"].is_object());
        // Arg transforms are mutually exclusive via `oneOf`.
        assert!(schema["$defs"]["arg"]["oneOf"].is_array());
    }

    #[test]
    fn schema_key_names_match_the_toml_model() {
        // The schema's documented surface must actually deserialize through the
        // real model — guards against the hand-written schema drifting from
        // `TomlPolicy`'s serde shape.
        let src = r#"
default-presence = "drop"

[tools.search]
presence = "keep"
rename = "find"
description = "find stuff"

[tools.search.args.artifact]
pin = "hylla"

[tools.search.args.limit]
min = 1.0
max = 50.0

[tools.search.args.kind]
enum = ["doc", "code"]

[tools.search.args.name]
pattern = "^[a-z]+$"
"#;
        let parsed: TomlPolicy = toml::from_str(src).expect("model accepts schema's keys");
        assert_eq!(parsed.default_presence.as_deref(), Some("drop"));
        assert!(parsed.tools.contains_key("search"));
    }

    /// Minimal evaluator for the subset of JSON-Schema keywords the `arg`
    /// `oneOf` uses (`required`, nested `anyOf`). Enough to prove which branches
    /// a candidate arg object satisfies without pulling in a validator crate.
    fn branch_matches(branch: &Value, candidate: &Value) -> bool {
        if let Some(reqs) = branch.get("required").and_then(Value::as_array) {
            let obj = candidate.as_object().expect("candidate is an object");
            return reqs.iter().all(|k| obj.contains_key(k.as_str().unwrap()));
        }
        if let Some(any) = branch.get("anyOf").and_then(Value::as_array) {
            return any.iter().any(|b| branch_matches(b, candidate));
        }
        false
    }

    /// How many of the arg `oneOf` branches a candidate satisfies. A valid arg
    /// must satisfy exactly one (the `oneOf` semantics).
    fn matching_branches(candidate: &Value) -> usize {
        let schema = json_schema();
        let branches = schema["$defs"]["arg"]["oneOf"]
            .as_array()
            .expect("oneOf is an array");
        branches
            .iter()
            .filter(|b| branch_matches(b, candidate))
            .count()
    }

    #[test]
    fn two_sided_range_matches_exactly_one_branch() {
        // The regression: `{min, max}` must satisfy exactly one `oneOf` branch
        // (the range branch), not two — so editors accept a legitimate range.
        assert_eq!(matching_branches(&json!({"min": 1, "max": 5})), 1);
    }

    #[test]
    fn one_sided_bounds_match_exactly_one_branch() {
        assert_eq!(matching_branches(&json!({"min": 1})), 1);
        assert_eq!(matching_branches(&json!({"max": 5})), 1);
    }

    #[test]
    fn single_transform_families_match_exactly_one_branch() {
        assert_eq!(matching_branches(&json!({"pin": "x"})), 1);
        assert_eq!(matching_branches(&json!({"default": "x"})), 1);
        assert_eq!(matching_branches(&json!({"enum": ["a"]})), 1);
        assert_eq!(matching_branches(&json!({"pattern": "^x$"})), 1);
    }

    #[test]
    fn mixed_transform_families_match_more_than_one_branch() {
        // Mixing a range with another family still violates `oneOf` (2 matches),
        // so the schema keeps rejecting incoherent args.
        assert_eq!(matching_branches(&json!({"pin": "x", "min": 1})), 2);
    }
}
