//! The policy document: the strict, versioned JSON form of a [`Policy`] that a
//! host stores and hands to lagom (`SPEC.md` §6.6).
//!
//! lagom reads nothing by itself. A host keeps its policies wherever it keeps
//! its own settings — a database row, a config service, a string in memory —
//! and passes the text here. Nothing in this module touches a file.
//!
//! Stricter than a bare [`Policy`] in two ways, both because a stored document
//! outlives the lagom that wrote it:
//!
//! - `lagom_policy` names the format. It is read first, so a document from a
//!   newer lagom fails as "unsupported format", not as a confusing unknown key.
//! - `default_presence` is required. A bare `Policy` defaults a missing key to
//!   `keep`, so a stored ceiling that lost the key would silently become
//!   passthrough.
//!
//! Text is read strictly: a key repeated in any object, at any depth, is
//! refused and named. A plain JSON read keeps only the last value, so
//! `{"v":"forbid","v":"passthrough"}` would lose the forbid without a word.
//!
//! ```
//! let policy = lagom_core::document::parse(
//!     r#"{"lagom_policy":1,"default_presence":"drop","tools":{"search":{"presence":"keep"}}}"#,
//! )
//! .unwrap();
//! assert_eq!(policy.default_presence, lagom_core::Presence::Drop);
//! ```

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::policy::{Policy, Presence, ToolPolicy};

/// The document format this lagom reads and writes.
pub const FORMAT: u64 = 1;

/// The key that carries the format version.
pub const FORMAT_KEY: &str = "lagom_policy";

/// A [`Policy`] in its stored form.
///
/// A separate struct rather than `Policy` flattened in: serde's `flatten` does
/// not combine with `deny_unknown_fields`, and a stored document must refuse a
/// stray key. [`PolicyDocument::of`] destructures `Policy` without `..`, so a new
/// `Policy` field fails to compile there instead of being dropped on save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDocument {
    /// The format version; always [`FORMAT`] for a document this lagom accepts.
    pub lagom_policy: u64,
    /// Presence for tools with no explicit rule. Required, with no default.
    pub default_presence: Presence,
    /// Per-tool rules, keyed by upstream tool name.
    #[serde(default)]
    pub tools: BTreeMap<String, ToolPolicy>,
}

impl PolicyDocument {
    /// The stored form of `policy`, stamped with [`FORMAT`].
    pub fn of(policy: &Policy) -> Self {
        let Policy {
            default_presence,
            tools,
        } = policy;
        Self {
            lagom_policy: FORMAT,
            default_presence: *default_presence,
            tools: tools.clone(),
        }
    }

    /// The engine's [`Policy`] this document describes.
    pub fn into_policy(self) -> Policy {
        Policy {
            default_presence: self.default_presence,
            tools: self.tools,
        }
    }
}

/// Why a policy document was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum DocumentError {
    /// The text is not JSON.
    Json(String),
    /// The JSON is not an object.
    NotAnObject,
    /// The object has no `lagom_policy` key.
    MissingFormat,
    /// The document names a format this lagom does not read.
    UnsupportedFormat {
        /// The `lagom_policy` value found.
        found: Value,
        /// The format this lagom reads.
        supported: u64,
    },
    /// The document is format [`FORMAT`] but its body is not a valid policy: a
    /// missing `default_presence`, an unknown key, an unknown rule.
    Invalid(String),
    /// An object repeats a key. Refused because a plain read keeps only the
    /// last value, which can drop a `forbid` or a seal.
    DuplicateKey {
        /// The repeated key.
        key: String,
        /// JSON Pointer (RFC 6901) to the repeated member, e.g.
        /// `/tools/search/args/version_pin`.
        path: String,
    },
    /// [`from_policy_json`] got a policy with no `default_presence`. A bare
    /// policy reads that as `keep`; a stored one must say which.
    MissingDefaultPresence,
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "policy document is not JSON: {e}"),
            Self::NotAnObject => f.write_str("policy document must be a JSON object"),
            Self::MissingFormat => write!(
                f,
                "not a lagom policy document: missing `{FORMAT_KEY}` (this lagom reads format {FORMAT})"
            ),
            Self::UnsupportedFormat { found, supported } => write!(
                f,
                "policy document format {found} is not supported; this lagom reads format {supported}"
            ),
            Self::Invalid(e) => write!(f, "invalid policy document: {e}"),
            Self::DuplicateKey { key, path } => {
                write!(f, "policy repeats key `{key}` at {path}; say it once")
            }
            Self::MissingDefaultPresence => f.write_str(
                "policy has no `default_presence`; a stored policy must say `keep` or `drop`",
            ),
        }
    }
}

impl std::error::Error for DocumentError {}

/// Parse a policy document from JSON text, refusing a repeated key anywhere.
pub fn parse(text: &str) -> Result<Policy, DocumentError> {
    from_value(read_strict(text)?)
}

/// Read a policy document already parsed as JSON.
///
/// **Cannot see a repeated key.** A [`Value`] object keeps only the last value
/// of a repeated key, so the duplicate is gone before this runs. A host holding
/// the text calls [`parse`] instead.
pub fn from_value(value: Value) -> Result<Policy, DocumentError> {
    let Some(object) = value.as_object() else {
        return Err(DocumentError::NotAnObject);
    };
    match object.get(FORMAT_KEY) {
        None => return Err(DocumentError::MissingFormat),
        Some(found) if found.as_u64() != Some(FORMAT) => {
            return Err(DocumentError::UnsupportedFormat {
                found: found.clone(),
                supported: FORMAT,
            });
        }
        Some(_) => {}
    }
    serde_json::from_value::<PolicyDocument>(value)
        .map(PolicyDocument::into_policy)
        .map_err(|e| DocumentError::Invalid(e.to_string()))
}

/// The stored form of `policy` as a JSON value.
pub fn to_value(policy: &Policy) -> Value {
    serde_json::to_value(PolicyDocument::of(policy))
        .expect("a policy document has string keys and plain values, so it always serializes")
}

/// The stored form of `policy` as compact JSON text. Deterministic: the struct
/// fixes the top-level order and tools and arguments are ordered maps, so the
/// same policy always yields the same bytes.
pub fn to_string(policy: &Policy) -> String {
    serde_json::to_string(&PolicyDocument::of(policy))
        .expect("a policy document has string keys and plain values, so it always serializes")
}

/// The stored form of a bare `Policy` given as JSON text: what a binding's
/// `policy_to_document` runs.
///
/// Stricter than reading a `Policy` and calling [`to_string`]: a repeated key is
/// refused, and so is a missing `default_presence`, which a bare policy would
/// read as `keep` and this would then store as an explicit `keep`.
pub fn from_policy_json(text: &str) -> Result<String, DocumentError> {
    let value = read_strict(text)?;
    let Some(object) = value.as_object() else {
        return Err(DocumentError::NotAnObject);
    };
    if !object.contains_key("default_presence") {
        return Err(DocumentError::MissingDefaultPresence);
    }
    let policy: Policy =
        serde_json::from_value(value).map_err(|e| DocumentError::Invalid(e.to_string()))?;
    Ok(to_string(&policy))
}

/// JSON text to a [`Value`], refusing a key repeated in any object.
///
/// Two passes: [`NoRepeats`] only walks keys, then serde_json builds the value
/// as usual, so numbers read exactly as serde_json reads them under any
/// feature set.
fn read_strict(text: &str) -> Result<Value, DocumentError> {
    let duplicate = RefCell::new(None);
    let mut de = serde_json::Deserializer::from_str(text);
    let checked = NoRepeats {
        path: String::new(),
        duplicate: &duplicate,
    }
    .deserialize(&mut de)
    .and_then(|()| de.end());
    if let Some((key, path)) = duplicate.into_inner() {
        return Err(DocumentError::DuplicateKey { key, path });
    }
    checked.map_err(|e| DocumentError::Json(e.to_string()))?;
    serde_json::from_str(text).map_err(|e| DocumentError::Json(e.to_string()))
}

/// Walks a JSON value and fails on the first repeated key, recording where.
/// The record carries the key out typed; serde's error carries only a message.
struct NoRepeats<'a> {
    /// JSON Pointer of the value being walked.
    path: String,
    /// Set to `(key, pointer)` when a repeat is found.
    duplicate: &'a RefCell<Option<(String, String)>>,
}

impl NoRepeats<'_> {
    fn child(&self, token: &str) -> Self {
        NoRepeats {
            path: format!(
                "{}/{}",
                self.path,
                token.replace('~', "~0").replace('/', "~1")
            ),
            duplicate: self.duplicate,
        }
    }
}

impl<'de> DeserializeSeed<'de> for NoRepeats<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for NoRepeats<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut index = 0usize;
        while seq
            .next_element_seed(self.child(&index.to_string()))?
            .is_some()
        {
            index += 1;
        }
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            let child = self.child(&key);
            if !seen.insert(key.clone()) {
                let message = format!("duplicate key `{key}` at {}", child.path);
                *self.duplicate.borrow_mut() = Some((key, child.path));
                return Err(de::Error::custom(message));
            }
            map.next_value_seed(child)?;
        }
        Ok(())
    }
}

/// A JSON Schema (draft 2020-12) for a policy document, for editors and for a
/// host that validates before it stores.
///
/// Hand-written so the contract is deliberate; `schema_names_every_key_a_document_carries`
/// fails if the serde shape and this schema drift apart.
pub fn json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://hylla.io/schemas/lagom-policy-document.json",
        "title": "lagom policy document",
        "description": "A narrowing policy over an upstream MCP server, in the form a host stores (SPEC.md §6.6).",
        "type": "object",
        "additionalProperties": false,
        "required": [FORMAT_KEY, "default_presence"],
        "properties": {
            FORMAT_KEY: {
                "description": "The document format version.",
                "const": FORMAT
            },
            "default_presence": { "$ref": "#/$defs/presence" },
            "tools": {
                "description": "Per-tool rules, keyed by upstream tool name.",
                "type": "object",
                "additionalProperties": { "$ref": "#/$defs/tool" }
            }
        },
        "$defs": {
            "presence": {
                "description": "`keep` exposes a tool; `drop` removes it.",
                "enum": ["keep", "drop"]
            },
            "tool": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "presence": { "$ref": "#/$defs/presence" },
                    "rename": {
                        "description": "Expose the tool under a different downstream name.",
                        "type": "string"
                    },
                    "description": {
                        "description": "`passthrough` keeps the upstream text plus an addendum; `{\"override\": text}` replaces it.",
                        "oneOf": [
                            { "const": "passthrough" },
                            {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["override"],
                                "properties": { "override": { "type": "string" } }
                            }
                        ]
                    },
                    "args": {
                        "description": "Per-argument rules, keyed by upstream argument name.",
                        "type": "object",
                        "additionalProperties": { "$ref": "#/$defs/arg" }
                    }
                }
            },
            "arg": {
                "description": "Exactly one rule per argument.",
                "oneOf": [
                    { "const": "passthrough" },
                    { "const": "forbid", "description": "The argument must be absent; a call carrying it is refused." },
                    {
                        "type": "object", "additionalProperties": false, "required": ["pin"],
                        "properties": { "pin": { "description": "Fixed value, hidden and injected on every call." } }
                    },
                    {
                        "type": "object", "additionalProperties": false, "required": ["default"],
                        "properties": { "default": { "description": "Supplied when the agent omits the argument." } }
                    },
                    {
                        "type": "object", "additionalProperties": false, "required": ["constrain"],
                        "properties": { "constrain": { "$ref": "#/$defs/constraint" } }
                    }
                ]
            },
            "constraint": {
                "oneOf": [
                    {
                        "type": "object", "additionalProperties": false, "required": ["enum"],
                        "properties": { "enum": { "type": "array" } }
                    },
                    {
                        "type": "object", "additionalProperties": false, "required": ["range"],
                        "properties": {
                            "range": {
                                "type": "object", "additionalProperties": false,
                                "properties": { "min": { "type": "number" }, "max": { "type": "number" } }
                            }
                        }
                    },
                    {
                        "type": "object", "additionalProperties": false, "required": ["pattern"],
                        "properties": { "pattern": { "type": "string" } }
                    }
                ]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ArgPolicy, Constraint, DescriptionPolicy};

    /// A policy using every rule the format can carry.
    fn every_rule() -> Policy {
        let mut args = BTreeMap::new();
        args.insert("a".to_string(), ArgPolicy::Pin(json!("x")));
        args.insert("b".to_string(), ArgPolicy::Default(json!(3)));
        args.insert(
            "c".to_string(),
            ArgPolicy::Constrain(Constraint::Enum(vec![json!("e")])),
        );
        args.insert(
            "d".to_string(),
            ArgPolicy::Constrain(Constraint::Range {
                min: Some(1.0),
                max: Some(2.0),
            }),
        );
        args.insert(
            "e".to_string(),
            ArgPolicy::Constrain(Constraint::Pattern("^x$".into())),
        );
        args.insert("f".to_string(), ArgPolicy::Forbid);
        args.insert("g".to_string(), ArgPolicy::Passthrough);
        let mut tools = BTreeMap::new();
        tools.insert(
            "search".to_string(),
            ToolPolicy {
                presence: Some(Presence::Keep),
                rename: Some("find".into()),
                description: DescriptionPolicy::Override("Find.".into()),
                args,
            },
        );
        tools.insert("plain".to_string(), ToolPolicy::default());
        Policy {
            default_presence: Presence::Drop,
            tools,
        }
    }

    #[test]
    fn round_trips_every_rule() {
        let policy = every_rule();
        assert_eq!(parse(&to_string(&policy)).unwrap(), policy);
    }

    #[test]
    fn stored_form_is_stamped_and_deterministic() {
        let text = to_string(&every_rule());
        assert!(
            text.starts_with(r#"{"lagom_policy":1,"default_presence":"drop","#),
            "{text}"
        );
        assert_eq!(text, to_string(&every_rule()));
    }

    #[test]
    fn missing_default_presence_is_refused_not_defaulted_to_keep() {
        let err = parse(r#"{"lagom_policy":1,"tools":{}}"#).unwrap_err();
        assert!(
            matches!(&err, DocumentError::Invalid(m) if m.contains("default_presence")),
            "{err}"
        );
        // Control: the bare Policy shape still defaults it, which is the hazard.
        let bare: Policy = serde_json::from_str(r#"{"tools":{}}"#).unwrap();
        assert_eq!(bare.default_presence, Presence::Keep);
    }

    #[test]
    fn missing_format_is_refused() {
        let err = parse(r#"{"default_presence":"drop"}"#).unwrap_err();
        assert_eq!(err, DocumentError::MissingFormat);
    }

    #[test]
    fn a_future_format_is_named_not_reported_as_an_unknown_key() {
        let err =
            parse(r#"{"lagom_policy":2,"default_presence":"drop","new_key":true}"#).unwrap_err();
        assert_eq!(
            err,
            DocumentError::UnsupportedFormat {
                found: json!(2),
                supported: FORMAT
            }
        );
        for found in [json!("1"), json!(1.5), json!(null)] {
            let doc = json!({"lagom_policy": found, "default_presence": "drop"});
            assert!(
                matches!(
                    from_value(doc),
                    Err(DocumentError::UnsupportedFormat { .. })
                ),
                "{found}"
            );
        }
    }

    #[test]
    fn unknown_keys_are_refused_at_every_level() {
        for text in [
            r#"{"lagom_policy":1,"default_presence":"drop","toolz":{}}"#,
            r#"{"lagom_policy":1,"default_presence":"drop","tools":{"s":{"arg":{}}}}"#,
            r#"{"lagom_policy":1,"default_presence":"drop","tools":{"s":{"args":{"x":"forbidden"}}}}"#,
        ] {
            assert!(
                matches!(parse(text), Err(DocumentError::Invalid(_))),
                "{text}"
            );
        }
    }

    /// Each level where a repeat would silently change the policy. The first
    /// value is always the restricting one, so a last-wins read would widen.
    const REPEATS: [(&str, &str, &str); 7] = [
        (
            r#"{"lagom_policy":1,"default_presence":"drop","default_presence":"keep"}"#,
            "default_presence",
            "/default_presence",
        ),
        (
            r#"{"lagom_policy":2,"lagom_policy":1,"default_presence":"drop"}"#,
            "lagom_policy",
            "/lagom_policy",
        ),
        (
            r#"{"lagom_policy":1,"default_presence":"drop","tools":{"s":{"presence":"keep"}},"tools":{}}"#,
            "tools",
            "/tools",
        ),
        (
            r#"{"lagom_policy":1,"default_presence":"keep","tools":{"s":{"presence":"drop"},"s":{"presence":"keep"}}}"#,
            "s",
            "/tools/s",
        ),
        (
            r#"{"lagom_policy":1,"default_presence":"keep","tools":{"s":{"presence":"drop","presence":"keep"}}}"#,
            "presence",
            "/tools/s/presence",
        ),
        (
            r#"{"lagom_policy":1,"default_presence":"keep","tools":{"s":{"args":{"v":"forbid","v":"passthrough"}}}}"#,
            "v",
            "/tools/s/args/v",
        ),
        (
            r#"{"lagom_policy":1,"default_presence":"keep","tools":{"a/b":{"args":{"x":{"pin":[{"k":1,"k":2}]}}}}}"#,
            "k",
            "/tools/a~1b/args/x/pin/0/k",
        ),
    ];

    #[test]
    fn a_repeated_key_is_refused_and_named_at_every_level() {
        for (text, key, path) in REPEATS {
            assert_eq!(
                parse(text).unwrap_err(),
                DocumentError::DuplicateKey {
                    key: key.into(),
                    path: path.into()
                },
                "{text}"
            );
            // Control: a plain read accepts it, keeping the last value.
            let merged: Value = serde_json::from_str(text).unwrap();
            if key == "v" {
                assert_eq!(merged["tools"]["s"]["args"]["v"], json!("passthrough"));
                assert_eq!(
                    from_value(merged).unwrap().tools["s"].args["v"],
                    crate::policy::ArgPolicy::Passthrough,
                    "from_value cannot see the repeat; that is its documented limit"
                );
            }
        }
    }

    #[test]
    fn repeat_message_names_key_and_path() {
        let msg = parse(REPEATS[5].0).unwrap_err().to_string();
        assert!(
            msg.contains("`v`") && msg.contains("/tools/s/args/v"),
            "{msg}"
        );
    }

    #[test]
    fn same_key_in_different_objects_is_not_a_repeat() {
        let text = r#"{"lagom_policy":1,"default_presence":"drop","tools":{"a":{"presence":"keep","args":{"x":"forbid"}},"b":{"presence":"keep","args":{"x":"forbid"}}}}"#;
        assert_eq!(parse(text).unwrap().tools.len(), 2);
    }

    #[test]
    fn syntax_and_trailing_text_are_still_json_errors() {
        for text in [
            r#"{"lagom_policy":1,"#,
            r#"{"lagom_policy":1,"default_presence":"drop"} x"#,
        ] {
            assert!(matches!(parse(text), Err(DocumentError::Json(_))), "{text}");
        }
    }

    #[test]
    fn numbers_read_exactly_as_serde_json_reads_them() {
        let text = r#"{"lagom_policy":1,"default_presence":"drop","tools":{"s":{"args":{"n":{"pin":18446744073709551615},"f":{"pin":-1.5e3},"i":{"pin":-7}}}}}"#;
        let args = &parse(text).unwrap().tools["s"].args;
        assert_eq!(args["n"], crate::policy::ArgPolicy::Pin(json!(u64::MAX)));
        assert_eq!(args["f"], crate::policy::ArgPolicy::Pin(json!(-1500.0)));
        assert_eq!(args["i"], crate::policy::ArgPolicy::Pin(json!(-7)));
    }

    #[test]
    fn from_policy_json_refuses_a_missing_default_presence() {
        assert_eq!(
            from_policy_json(r#"{"tools":{}}"#).unwrap_err(),
            DocumentError::MissingDefaultPresence
        );
        assert_eq!(
            from_policy_json(r#"{"default_presence":"drop","tools":{}}"#).unwrap(),
            r#"{"lagom_policy":1,"default_presence":"drop","tools":{}}"#
        );
    }

    #[test]
    fn from_policy_json_refuses_a_repeat_and_bad_shapes() {
        assert_eq!(
            from_policy_json(r#"{"default_presence":"drop","tools":{"s":{"args":{"v":"forbid","v":"passthrough"}}}}"#)
                .unwrap_err(),
            DocumentError::DuplicateKey {
                key: "v".into(),
                path: "/tools/s/args/v".into()
            }
        );
        assert_eq!(
            from_policy_json("[]").unwrap_err(),
            DocumentError::NotAnObject
        );
        assert!(matches!(
            from_policy_json(r#"{"default_presence":"drop","toolz":{}}"#),
            Err(DocumentError::Invalid(_))
        ));
    }

    #[test]
    fn non_json_and_non_object_are_refused() {
        assert!(matches!(parse("not json"), Err(DocumentError::Json(_))));
        assert_eq!(parse("[]").unwrap_err(), DocumentError::NotAnObject);
    }

    #[test]
    fn tools_may_be_omitted() {
        let policy = parse(r#"{"lagom_policy":1,"default_presence":"drop"}"#).unwrap();
        assert_eq!(policy, Policy::sealed());
    }

    /// Collect every object key `value` carries, by the schema definition that
    /// governs it, and fail on any the schema does not name.
    #[test]
    fn schema_names_every_key_a_document_carries() {
        let schema = json_schema();
        let doc = to_value(&every_rule());
        let top: Vec<&String> = doc.as_object().unwrap().keys().collect();
        for key in top {
            assert!(schema["properties"].get(key).is_some(), "top-level `{key}`");
        }
        let tool_props = &schema["$defs"]["tool"]["properties"];
        let arg_branches = schema["$defs"]["arg"]["oneOf"].as_array().unwrap();
        let constraint_branches = schema["$defs"]["constraint"]["oneOf"].as_array().unwrap();
        for tool in doc["tools"].as_object().unwrap().values() {
            for key in tool.as_object().unwrap().keys() {
                assert!(tool_props.get(key).is_some(), "tool key `{key}`");
            }
            for arg in tool["args"]
                .as_object()
                .into_iter()
                .flat_map(|a| a.values())
            {
                let named = arg_branches.iter().any(|b| match arg {
                    Value::String(tag) => b.get("const") == Some(&json!(tag)),
                    Value::Object(o) => {
                        let key = o.keys().next().unwrap();
                        b["properties"].get(key).is_some()
                    }
                    _ => false,
                });
                assert!(named, "arg rule {arg} has no schema branch");
                if let Some(c) = arg.get("constrain") {
                    let key = c.as_object().unwrap().keys().next().unwrap();
                    assert!(
                        constraint_branches
                            .iter()
                            .any(|b| b["properties"].get(key).is_some()),
                        "constraint `{key}`"
                    );
                }
            }
        }
        assert_eq!(
            schema["required"],
            json!([FORMAT_KEY, "default_presence"]),
            "the schema must require what the parser requires"
        );
    }
}
