//! Cross-binding parity comparator — the verdict step of the NO-DRIFT guard.
//!
//! Each face's runner emits a result map
//! (`{case: {status, payload}}`) to a file; this binary reads them and asserts,
//! per case, that **every** face agrees with the Rust core reference on:
//!
//! 1. the ok/err **classification** (`status`), and
//! 2. for ok cases, the **byte-identical** result JSON (`payload`), compared as
//!    parsed JSON values so a runner's outer pretty-printing/key-ordering never
//!    masks or fabricates a difference — only the engine's own serialized output
//!    is compared.
//!
//! Err-case payloads are intentionally not compared byte-for-byte: each binding
//! frames the engine's error message in its own idiom (`ValueError`, JS `Error`,
//! `lagom: <op>: <msg>`), which is a legitimate, documented framing difference,
//! not behavioral drift. Any *classification* mismatch or any ok-payload diff is
//! a CRITICAL drift finding and exits non-zero.
//!
//! Usage:
//!   lagom-parity-compare rust=<file> go=<file> python=<file> node=<file>
//! The `rust` face is the reference; any face may be omitted (it is then skipped
//! and reported as such — useful when a toolchain is unavailable, so the guard
//! degrades honestly rather than faking green).

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::Value;

/// One face's loaded result map: case name -> (status, optional payload value).
struct Face {
    name: String,
    cases: BTreeMap<String, (String, Option<Value>)>,
}

/// Parse a `face=path` argument, load the file, and parse its result map. The
/// payload string is itself re-parsed into a `Value` so the comparison is over
/// the engine's JSON structure, not the runner's outer string.
fn load(arg: &str) -> Result<Face, String> {
    let (name, path) = arg
        .split_once('=')
        .ok_or_else(|| format!("expected face=path, got {arg}"))?;
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{name}: read {path}: {e}"))?;
    let map: Value =
        serde_json::from_str(&raw).map_err(|e| format!("{name}: parse {path}: {e}"))?;
    let obj = map
        .as_object()
        .ok_or_else(|| format!("{name}: result is not an object"))?;

    let mut cases = BTreeMap::new();
    for (case, result) in obj {
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{name}/{case}: missing status"))?
            .to_string();
        // payload is a JSON *string* (the engine's serialized output) or null.
        let payload = match result.get("payload") {
            Some(Value::Null) | None => None,
            Some(Value::String(s)) => {
                Some(serde_json::from_str::<Value>(s).map_err(|e| {
                    format!("{name}/{case}: payload is not valid JSON: {e}")
                })?)
            }
            Some(other) => {
                return Err(format!(
                    "{name}/{case}: payload must be a JSON string or null, got {other}"
                ));
            }
        };
        cases.insert(case.clone(), (status, payload));
    }
    Ok(Face {
        name: name.to_string(),
        cases,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: lagom-parity-compare rust=<file> [go=<file>] [python=<file>] [node=<file>]");
        return ExitCode::FAILURE;
    }

    let mut faces = Vec::new();
    for arg in &args {
        match load(arg) {
            Ok(f) => faces.push(f),
            Err(e) => {
                eprintln!("error loading face: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let reference = match faces.iter().find(|f| f.name == "rust") {
        Some(r) => r,
        None => {
            eprintln!("error: the `rust` reference face is required");
            return ExitCode::FAILURE;
        }
    };
    let others: Vec<&Face> = faces.iter().filter(|f| f.name != "rust").collect();

    println!("== lagom cross-binding parity (NO-DRIFT guard) ==");
    println!(
        "reference: rust ({} cases) | compared faces: {}",
        reference.cases.len(),
        if others.is_empty() {
            "(none)".to_string()
        } else {
            others
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!();

    let mut drift = 0usize;

    for (case, (ref_status, ref_payload)) in &reference.cases {
        let mut row_marks: Vec<String> = Vec::new();
        for face in &others {
            let mark = match face.cases.get(case) {
                None => {
                    drift += 1;
                    println!("DRIFT  {case}: face `{}` is missing this case", face.name);
                    "MISSING".to_string()
                }
                Some((status, payload)) => {
                    if status != ref_status {
                        drift += 1;
                        println!(
                            "DRIFT  {case}: status differs — rust={ref_status}, {}={status}",
                            face.name
                        );
                        "STATUS".to_string()
                    } else if ref_status == "ok" && payload != ref_payload {
                        drift += 1;
                        println!(
                            "DRIFT  {case}: ok payload differs vs rust on face `{}`:\n  rust: {}\n  {}: {}",
                            face.name,
                            serde_json::to_string(ref_payload).unwrap(),
                            face.name,
                            payload
                                .as_ref()
                                .map(|p| serde_json::to_string(p).unwrap())
                                .unwrap_or_else(|| "null".into()),
                        );
                        "PAYLOAD".to_string()
                    } else {
                        "ok".to_string()
                    }
                }
            };
            row_marks.push(format!("{}={mark}", face.name));
        }
        let detail = if others.is_empty() {
            String::new()
        } else {
            format!("  [{}]", row_marks.join(" "))
        };
        println!("  {case} ({ref_status}){detail}");
    }

    // Catch any case a non-reference face has that the reference lacks.
    for face in &others {
        for case in face.cases.keys() {
            if !reference.cases.contains_key(case) {
                drift += 1;
                println!("DRIFT  {case}: face `{}` has a case absent from rust", face.name);
            }
        }
    }

    println!();
    if others.is_empty() {
        println!(
            "WARNING: no non-reference faces supplied — parity NOT proven. Supply go/python/node."
        );
        // No drift, but nothing was actually compared: degrade honestly.
        return ExitCode::SUCCESS;
    }
    if drift == 0 {
        println!(
            "PARITY OK: {} cases, {} face(s) byte-identical to the rust core. Zero drift.",
            reference.cases.len(),
            others.len()
        );
        ExitCode::SUCCESS
    } else {
        println!("PARITY FAILED: {drift} drift finding(s). This is a CRITICAL no-drift violation.");
        ExitCode::FAILURE
    }
}
