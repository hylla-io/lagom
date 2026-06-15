//! The MCP surface lagom transforms: tool definitions and tool calls.
//!
//! These mirror the shapes carried by the MCP `tools/list` response and
//! `tools/call` request, reduced to exactly the fields lagom reasons about. The
//! engine is transport-less, so these are plain data — no JSON-RPC envelope.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A single tool as advertised by an upstream server (or projected downstream).
///
/// `input_schema` is the tool's JSON Schema object; lagom transforms it in place
/// (pruning pinned properties, tightening constrained ones) when projecting.
///
/// To keep every binding's contract drift-free against real MCP servers,
/// deserialization is lenient on the schema field: it accepts the MCP-native
/// camelCase key `inputSchema` as well as `input_schema`, and normalizes a
/// missing or JSON-`null` schema to an empty object `{}`. A consumer can
/// therefore hand the raw upstream `tools/list` surface straight to lagom with no
/// field-mapping shim. Serialization always emits the canonical snake_case
/// `input_schema`, so the projected (downstream) contract is unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    /// The tool name the consumer sees and calls.
    pub name: String,
    /// Human/agent-facing description. `None` when the upstream omits one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The tool's JSON Schema for arguments (an object schema). Accepts the
    /// MCP camelCase `inputSchema` on input; missing/null normalizes to `{}`.
    #[serde(
        alias = "inputSchema",
        default = "empty_object",
        deserialize_with = "de_input_schema"
    )]
    pub input_schema: Value,
}

/// The default `input_schema` when an upstream tool omits the key entirely.
fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// Deserialize `input_schema`, normalizing an explicit JSON `null` to `{}`.
///
/// Real MCP servers emit `"inputSchema": null` (or a missing key) for zero-arg
/// tools; both must project as the empty object schema rather than failing or
/// carrying a null through the transform pipeline.
fn de_input_schema<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(if value.is_null() {
        empty_object()
    } else {
        value
    })
}

impl ToolDef {
    /// Construct a tool definition.
    pub fn new(name: impl Into<String>, description: Option<String>, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            description,
            input_schema,
        }
    }
}

/// A tool invocation: the projected name plus the agent-supplied arguments.
///
/// `arguments` is expected to be a JSON object; a non-object value is treated as
/// "no arguments" during rewriting and pinned/default values are still injected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The tool name as the consumer knows it (post-rename).
    pub name: String,
    /// The arguments the consumer supplied.
    pub arguments: Value,
}

impl ToolCall {
    /// Construct a tool call.
    pub fn new(name: impl Into<String>, arguments: Value) -> Self {
        Self {
            name: name.into(),
            arguments,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // The schema field must accept the MCP-native camelCase key so a consumer can
    // pass the raw upstream tools/list surface with no mapping shim (the #1
    // consumer gotcha, confirmed by a real consumer — SAND_LAGOM_FINDINGS.md §4).
    #[test]
    fn deserializes_camelcase_input_schema() {
        let def: ToolDef =
            serde_json::from_value(json!({"name": "t", "inputSchema": {"type": "object"}}))
                .expect("camelCase inputSchema must deserialize");
        assert_eq!(def.input_schema, json!({"type": "object"}));
    }

    #[test]
    fn deserializes_snakecase_input_schema() {
        let def: ToolDef =
            serde_json::from_value(json!({"name": "t", "input_schema": {"type": "object"}}))
                .expect("snake_case input_schema must deserialize");
        assert_eq!(def.input_schema, json!({"type": "object"}));
    }

    // Real MCP servers emit a missing or null schema for zero-arg tools; both
    // normalize to {} rather than failing or carrying a null through the pipeline.
    #[test]
    fn missing_schema_defaults_to_empty_object() {
        let def: ToolDef = serde_json::from_value(json!({"name": "t"}))
            .expect("missing schema must default, not fail");
        assert_eq!(def.input_schema, json!({}));
    }

    #[test]
    fn null_schema_normalizes_to_empty_object() {
        let def: ToolDef = serde_json::from_value(json!({"name": "t", "inputSchema": null}))
            .expect("null schema must normalize, not fail");
        assert_eq!(def.input_schema, json!({}));
    }

    // Output stays canonical snake_case — the projected downstream contract is
    // unchanged, so existing bindings/consumers reading `input_schema` keep working.
    #[test]
    fn serializes_canonical_snake_case() {
        let def = ToolDef::new("t", None, json!({"type": "object"}));
        let out = serde_json::to_value(&def).unwrap();
        assert!(out.get("input_schema").is_some());
        assert!(out.get("inputSchema").is_none());
    }
}
