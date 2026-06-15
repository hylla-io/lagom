//! The MCP surface lagom transforms: tool definitions and tool calls.
//!
//! These mirror the shapes carried by the MCP `tools/list` response and
//! `tools/call` request, reduced to exactly the fields lagom reasons about. The
//! engine is transport-less, so these are plain data — no JSON-RPC envelope.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single tool as advertised by an upstream server (or projected downstream).
///
/// `input_schema` is the tool's JSON Schema object; lagom transforms it in place
/// (pruning pinned properties, tightening constrained ones) when projecting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    /// The tool name the consumer sees and calls.
    pub name: String,
    /// Human/agent-facing description. `None` when the upstream omits one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The tool's JSON Schema for arguments (an object schema).
    pub input_schema: Value,
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
