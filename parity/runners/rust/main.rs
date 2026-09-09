//! Parity runner — Rust core face (the canonical reference of the NO-DRIFT guard).
//!
//! Reads the shared fixture (`parity/fixtures/cases.json`), runs every case
//! through `lagom_core` directly, and prints a deterministic result map to
//! stdout:
//!
//! ```json
//! {"<case>": {"status": "ok"|"err", "payload": <json-string|null>}, ...}
//! ```
//!
//! `payload` for an `ok` case is the face's exact `serde_json::to_string` output
//! (the byte string every other face must match); for an `err` case it is `null`
//! (error-message *framing* legitimately differs across faces, so the guard
//! compares only the ok/err classification for errors, plus byte-identical ok
//! payloads). The comparator (`parity/runners/compare`) diffs every face's map
//! against this one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use lagom_core::{
    MintRecord, Policy, ToolCall, ToolDef, UpstreamCommand, merge, mint, project, refire, rewrite,
    validate,
};
use serde_json::{Map, Value, json};

/// The result of running one fixture case through this face.
struct CaseResult {
    status: &'static str,
    payload: Option<String>,
}

fn ok(payload: String) -> CaseResult {
    CaseResult {
        status: "ok",
        payload: Some(payload),
    }
}

fn err() -> CaseResult {
    CaseResult {
        status: "err",
        payload: None,
    }
}

/// Deserialize a fixture sub-value into a core type, panicking with context on a
/// malformed fixture (the fixture is committed, so this is a build-the-guard bug,
/// not a runtime path).
fn from<T: serde::de::DeserializeOwned>(label: &str, v: &Value) -> T {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("fixture {label}: {e}"))
}

/// Deserialize a *policy-shaped* fixture input, classifying a refusal as an `err`
/// result instead of panicking.
///
/// WHY this one input is fallible while [`from`] stays panicking: every other
/// face hands the policy across a JSON boundary (wasm bytes, PyO3 string, napi
/// string), so a `deny_unknown_fields` refusal reaches their runner as an
/// ordinary error return and classifies `err`. The reference must model the same
/// boundary, or an unknown-key case would abort this runner rather than be
/// compared — and the comparator would never see the drift it exists to catch.
fn policy_from(v: &Value) -> Result<Policy, ()> {
    serde_json::from_value(v.clone()).map_err(|_| ())
}

/// Run one case through the Rust core face.
fn run_case(case: &Map<String, Value>, by_name: &BTreeMap<String, Value>, upstream: &[ToolDef]) -> CaseResult {
    let op = case["op"].as_str().expect("case op");
    match op {
        "project" => {
            let Ok(policy) = policy_from(&case["policy"]) else {
                return err();
            };
            ok(serde_json::to_string(&project(upstream, &policy)).unwrap())
        }
        "rewrite" => {
            let call: ToolCall = from("call", &case["call"]);
            let Ok(policy) = policy_from(&case["policy"]) else {
                return err();
            };
            match rewrite(&call, &policy) {
                Ok(upstream_call) => ok(serde_json::to_string(&upstream_call).unwrap()),
                Err(_) => err(),
            }
        }
        "merge" => {
            let (Ok(base), Ok(overlay)) =
                (policy_from(&case["base"]), policy_from(&case["overlay"]))
            else {
                return err();
            };
            match merge(&base, &overlay) {
                Ok(merged) => ok(serde_json::to_string(&merged).unwrap()),
                Err(_) => err(),
            }
        }
        "validate" => {
            let Ok(policy) = policy_from(&case["policy"]) else {
                return err();
            };
            match validate(&policy, upstream) {
                Ok(()) => ok("null".to_string()),
                Err(_) => err(),
            }
        }
        "mint" => match mint_record(case) {
            Ok(record) => ok(serde_json::to_string(&record).unwrap()),
            Err(()) => err(),
        },
        "refire" => {
            // Two input shapes. `record` is the on-disk shape the CLI's
            // `refire --record` reads back, deserialized here so a
            // `deny_unknown_fields` refusal on the mint shapes is COMPARED;
            // `mint_of` reuses the record a named mint case produced, which can
            // only ever be well-formed.
            let record = match case.get("record") {
                // Fallible for the same reason `policy_from` is: every other
                // face parses this record from a JSON string, so a refusal must
                // reach the comparator as `err` rather than abort the runner.
                Some(v) => match serde_json::from_value::<MintRecord>(v.clone()) {
                    Ok(record) => record,
                    Err(_) => return err(),
                },
                None => {
                    let mint_name = case["mint_of"].as_str().expect("refire mint_of");
                    let source = by_name
                        .get(mint_name)
                        .unwrap_or_else(|| panic!("refire references unknown case {mint_name}"));
                    let source = source.as_object().unwrap();
                    mint_record(source).expect("referenced mint must succeed")
                }
            };
            ok(serde_json::to_string(&refire(&record)).unwrap())
        }
        other => panic!("unknown op {other}"),
    }
}

/// Build a `MintRecord` from a mint case, or `Err(())` on a widening overlay.
fn mint_record(case: &Map<String, Value>) -> Result<MintRecord, ()> {
    let run_id = case["run_id"].as_str().expect("mint run_id");
    let base = policy_from(&case["base"])?;
    let dynamic: Option<Policy> = match case.get("dynamic") {
        Some(Value::Null) | None => None,
        Some(v) => Some(policy_from(v)?),
    };
    let upstream: UpstreamCommand = from("upstream_command", &case["upstream_command"]);
    mint(run_id, &base, dynamic.as_ref(), upstream).map_err(|_| ())
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("parity/fixtures/cases.json"));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    let fixture: Value = serde_json::from_str(&raw).expect("parse fixture");

    let upstream: Vec<ToolDef> = from("upstream", &fixture["upstream"]);
    let cases = fixture["cases"].as_array().expect("cases array");

    // Index cases by name so refire can find its mint source.
    let by_name: BTreeMap<String, Value> = cases
        .iter()
        .map(|c| (c["name"].as_str().expect("case name").to_string(), c.clone()))
        .collect();

    let mut out = Map::new();
    for case in cases {
        let case = case.as_object().expect("case object");
        let name = case["name"].as_str().unwrap();
        let result = run_case(case, &by_name, &upstream);
        out.insert(
            name.to_string(),
            json!({ "status": result.status, "payload": result.payload }),
        );
    }

    // Pretty + sorted (BTree via Map already preserves insertion; sort by name).
    let sorted: BTreeMap<&String, &Value> = out.iter().collect();
    println!("{}", serde_json::to_string_pretty(&sorted).unwrap());
}
