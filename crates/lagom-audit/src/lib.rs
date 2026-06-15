//! # lagom-audit
//!
//! The append-only JSONL audit log (`SPEC.md` §9.3): it records the original
//! upstream tool defs, the resolved policy a mint produced, and every rewrite
//! and rejection. This log is the source of both **refire** (§8.2 — re-mint
//! from the recorded resolved policy) and **traceability** (`run-id → mint
//! record → each rewritten call + original`).
//!
//! On-disk shape is JSONL: one [`AuditEvent`] per line, append-only, never
//! rewritten. The exact default path is configurable (`SPEC.md` §13); the log
//! itself takes an explicit path.
//!
//! The log accepts events as plain data; it bakes in no clock. Timestamps, when
//! present, live inside the [`AuditEvent`] payloads the caller constructs, so the
//! writer is fully deterministic and its tests need no time source.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use lagom_core::{MintRecord, Policy, ToolCall, ToolDef};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Anything that can go wrong opening or writing the audit log.
#[derive(Debug, Error)]
pub enum AuditError {
    /// The log file could not be opened or written.
    #[error("audit log `{path}`: {source}")]
    Io {
        /// The log path involved.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// An event could not be serialized to JSON.
    #[error("serializing audit event: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// A single append-only audit record. One JSON object per JSONL line.
///
/// Every event carries the `run_id` it belongs to so the trace can be
/// reconstructed by filtering on it (`SPEC.md` §8.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    /// The full mint record — resolved policy **plus** provenance (the base
    /// profile, overlay config paths, dynamic inputs, and upstream command) —
    /// persisted at session start so the run can be **refired** from its own
    /// trace (`SPEC.md` §8.2). The richest refire basis: where `OriginalDefs` and
    /// `ResolvedPolicy` capture what the run saw, `Mint` captures the provenance
    /// that produced it.
    Mint {
        /// The recorded mint, including its `run_id`.
        record: MintRecord,
    },
    /// The original upstream tool surface, captured at mint time.
    OriginalDefs {
        /// The run this mint belongs to.
        run_id: String,
        /// The full upstream `tools/list` surface as advertised.
        defs: Vec<ToolDef>,
    },
    /// The fully-resolved policy a mint produced — the basis for refire.
    ResolvedPolicy {
        /// The run this mint belongs to.
        run_id: String,
        /// The resolved projection spec.
        policy: Policy,
    },
    /// A call that was rewritten and forwarded upstream.
    Rewrite {
        /// The run this call belongs to.
        run_id: String,
        /// The call as the agent issued it (projected surface).
        projected: ToolCall,
        /// The call as forwarded upstream (pins/defaults injected).
        upstream: ToolCall,
    },
    /// A call rejected before reaching the upstream (`SPEC.md` §9.1).
    Rejection {
        /// The run this call belongs to.
        run_id: String,
        /// The call as the agent issued it.
        projected: ToolCall,
        /// Why lagom rejected it — the [`lagom_core::Reject`] message text
        /// (`Reject` is not serializable, so its `message` is captured here).
        reason: String,
    },
}

/// An open append-only audit log.
///
/// Wraps a file opened in create+append mode behind a [`BufWriter`]; each
/// [`record`](AuditLog::record) writes one JSON line and flushes it, so a record
/// is durable before the call returns.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl AuditLog {
    /// Open (creating if absent) the append-only JSONL log at `path`.
    ///
    /// Existing content is never truncated — records append to the end of the
    /// file. The parent directory must already exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AuditError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| AuditError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            path,
            writer: BufWriter::new(file),
        })
    }

    /// The path this log writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event as a JSON line.
    ///
    /// Serializes `event` to a single line and appends it; flushes so the record
    /// is durable before returning.
    pub fn record(&mut self, event: &AuditEvent) -> Result<(), AuditError> {
        let line = serde_json::to_string(event)?;
        let io = |source| AuditError::Io {
            path: self.path.clone(),
            source,
        };
        self.writer.write_all(line.as_bytes()).map_err(io)?;
        self.writer.write_all(b"\n").map_err(io)?;
        self.writer.flush().map_err(io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use lagom_core::policy::Policy;
    use lagom_core::tooldef::{ToolCall, ToolDef};
    use lagom_core::{PolicySources, ResolvedPolicy, UpstreamCommand};
    use serde_json::json;

    use super::*;

    /// A sample mint record for the [`AuditEvent::Mint`] coverage.
    fn sample_mint() -> MintRecord {
        let upstream = UpstreamCommand {
            command: "srv".into(),
            args: vec!["-y".into()],
            env: vec![],
        };
        MintRecord {
            run_id: "run-1".into(),
            sources: PolicySources {
                config_paths: vec!["lagom.toml".into()],
                upstream: upstream.clone(),
                dynamic_inputs: serde_json::Value::Null,
            },
            resolved: ResolvedPolicy {
                policy: Policy::default(),
                upstream,
            },
        }
    }

    /// A unique scratch path under the OS temp dir, removed if it already exists.
    fn temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = format!(
            "lagom-audit-{tag}-{:?}-{}",
            std::thread::current().id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        p.push(format!("{nonce}.jsonl"));
        let _ = fs::remove_file(&p);
        p
    }

    /// One sample of each [`AuditEvent`] variant, sharing a run id.
    fn sample_events() -> Vec<AuditEvent> {
        let run_id = "run-1".to_string();
        let def = ToolDef::new(
            "fs.read",
            Some("read a file".to_string()),
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        );
        let policy = Policy::default();
        let projected = ToolCall::new("read", json!({"path": "a.txt"}));
        let upstream = ToolCall::new("fs.read", json!({"path": "a.txt", "root": "/repo"}));
        vec![
            AuditEvent::Mint {
                record: sample_mint(),
            },
            AuditEvent::OriginalDefs {
                run_id: run_id.clone(),
                defs: vec![def],
            },
            AuditEvent::ResolvedPolicy {
                run_id: run_id.clone(),
                policy,
            },
            AuditEvent::Rewrite {
                run_id: run_id.clone(),
                projected: projected.clone(),
                upstream,
            },
            AuditEvent::Rejection {
                run_id,
                projected,
                reason: "path must be one of {a.txt, b.txt}".to_string(),
            },
        ]
    }

    #[test]
    fn event_serialization_round_trips() {
        for event in sample_events() {
            let line = serde_json::to_string(&event).expect("serialize");
            assert!(
                !line.contains('\n'),
                "a JSONL record must be a single line: {line}"
            );
            let back: AuditEvent = serde_json::from_str(&line).expect("deserialize");
            assert_eq!(event, back, "round-trip must be lossless");
        }
    }

    #[test]
    fn each_variant_tags_its_kind() {
        let cases = [
            (
                AuditEvent::Mint {
                    record: sample_mint(),
                },
                "mint",
            ),
            (
                AuditEvent::OriginalDefs {
                    run_id: "r".into(),
                    defs: vec![],
                },
                "original_defs",
            ),
            (
                AuditEvent::ResolvedPolicy {
                    run_id: "r".into(),
                    policy: Policy::default(),
                },
                "resolved_policy",
            ),
            (
                AuditEvent::Rewrite {
                    run_id: "r".into(),
                    projected: ToolCall::new("t", json!({})),
                    upstream: ToolCall::new("t", json!({})),
                },
                "rewrite",
            ),
            (
                AuditEvent::Rejection {
                    run_id: "r".into(),
                    projected: ToolCall::new("t", json!({})),
                    reason: "no".into(),
                },
                "rejection",
            ),
        ];
        for (event, kind) in cases {
            let value: serde_json::Value =
                serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
            assert_eq!(value["kind"], kind, "kind tag for {event:?}");
        }
    }

    #[test]
    fn open_records_one_line_per_event() {
        let path = temp_path("one-line");
        let events = sample_events();
        let mut log = AuditLog::open(&path).expect("open");
        assert_eq!(log.path(), path.as_path());
        for event in &events {
            log.record(event).expect("record");
        }
        drop(log);

        let contents = fs::read_to_string(&path).expect("read back");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), events.len(), "one JSONL line per event");
        for (line, event) in lines.iter().zip(&events) {
            let back: AuditEvent = serde_json::from_str(line).expect("parse line");
            assert_eq!(&back, event);
        }
        fs::remove_file(&path).ok();
    }

    #[test]
    fn open_appends_and_never_truncates() {
        let path = temp_path("append");
        let all = sample_events();
        let (first, second) = all.split_at(2);

        // First session writes the first batch and closes.
        let mut log = AuditLog::open(&path).expect("open 1");
        for event in first {
            log.record(event).expect("record 1");
        }
        drop(log);

        // Re-opening the same path must append, not truncate.
        let mut log = AuditLog::open(&path).expect("open 2");
        for event in second {
            log.record(event).expect("record 2");
        }
        drop(log);

        let contents = fs::read_to_string(&path).expect("read back");
        let parsed: Vec<AuditEvent> = contents
            .lines()
            .map(|l| serde_json::from_str(l).expect("parse"))
            .collect();
        assert_eq!(parsed, all, "both batches present in order");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn record_flushes_so_concurrent_reads_see_data() {
        // record() flushes; the line is readable before the log is dropped.
        let path = temp_path("flush");
        let event = AuditEvent::Rejection {
            run_id: "r".into(),
            projected: ToolCall::new("t", json!({})),
            reason: "denied".into(),
        };
        let mut log = AuditLog::open(&path).expect("open");
        log.record(&event).expect("record");

        let contents = fs::read_to_string(&path).expect("read before drop");
        assert_eq!(contents.lines().count(), 1);
        let back: AuditEvent = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(back, event);
        drop(log);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn open_errors_when_parent_dir_is_missing() {
        let mut path = std::env::temp_dir();
        path.push("lagom-audit-no-such-dir-xyz");
        path.push("nested.jsonl");
        let err = AuditLog::open(&path).expect_err("missing parent dir must error");
        match err {
            AuditError::Io { path: p, .. } => assert_eq!(p, path),
            other => panic!("expected Io error, got {other:?}"),
        }
    }
}
