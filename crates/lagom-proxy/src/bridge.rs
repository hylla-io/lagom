//! The stdio bridge: spawn the upstream child and shuttle JSON-RPC between the
//! harness (this process's stdio) and the child, transforming `tools/list`
//! responses and `tools/call` requests through [`lagom_core`] in between
//! (`SPEC.md` §7.1, §10).
//!
//! ## Wire format
//!
//! MCP stdio transport is **newline-delimited JSON-RPC**: each message is one
//! JSON object on its own line, with no embedded newlines, and `stderr` carries
//! only logging (MCP spec, `2025-11-25` transports). We frame with
//! [`tokio::io::AsyncBufReadExt::read_line`] on each side and write
//! `serialize + '\n'`.
//!
//! ## Selective interception
//!
//! Almost everything is forwarded **verbatim** (passthrough, `CONTEXT.md`). Two
//! message shapes are intercepted:
//!
//! - **`tools/list` request (downstream → upstream)** — forwarded, but its `id`
//!   is recorded so the matching *response* can be projected. Re-projection on a
//!   `notifications/tools/list_changed` happens the same way: the harness
//!   re-issues `tools/list` and we re-project that response.
//! - **`tools/call` request (downstream → upstream)** — [`lagom_core::rewrite`]
//!   is applied immediately. On [`lagom_core::Reject`] we synthesize a JSON-RPC
//!   error result straight back downstream **without calling upstream**,
//!   annotated with what lagom did (`SPEC.md` §9.1); otherwise the rewritten
//!   params are forwarded.
//! - **`tools/list` response (upstream → downstream)** — if its `id` matches a
//!   recorded `tools/list` request, the `result.tools` array is projected
//!   through [`lagom_core::project`] before forwarding.
//!
//! The harness's own `initialize`, notifications, resources, prompts and
//! pagination pass through unchanged (`SPEC.md` §10).
//!
//! ## Lifecycle
//!
//! Before the bridge starts, lagom runs its **own** MCP lifecycle handshake with
//! the upstream during the drift probe (MCP spec, `2025-11-25` lifecycle):
//! `initialize` request → response → `notifications/initialized`, *then*
//! `tools/list`. Strict real upstreams reject pre-initialize traffic, so the
//! handshake is mandatory for a real serve/validate to succeed. The harness's
//! own `initialize` later in the session is still forwarded verbatim. The
//! buffered upstream reader used for the handshake is threaded straight into the
//! pump so no upstream bytes read past the probe response are dropped.
//!
//! ## Concurrency
//!
//! Two independent tasks run the duplex bridge: one pumps downstream → upstream,
//! the other upstream → downstream. They share the pending-`tools/list` id set
//! and the downstream writer (both tasks write to it) and the audit log behind
//! [`Mutex`]es. Either direction closing ends the session.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;

use lagom_audit::{AuditEvent, AuditLog};
use lagom_core::{ToolCall, ToolDef, project, rewrite, validate};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, stdin, stdout};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use super::{ProxyError, ResolvedPolicy};

/// JSON-RPC error code lagom returns when it rejects a `tools/call` before it
/// reaches the upstream. `-32001` is in the server-defined range the JSON-RPC
/// 2.0 spec reserves for implementation-defined errors.
const REJECT_ERROR_CODE: i64 = -32001;

/// The MCP protocol version lagom's drift probe negotiates with the upstream
/// during the `initialize` handshake (MCP spec, `2025-11-25` lifecycle).
const PROBE_PROTOCOL_VERSION: &str = "2025-11-25";

/// A shared, async writer used by both pump tasks for the downstream side.
type SharedWriter = Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>;
/// The set of in-flight `tools/list` request ids awaiting a response to project.
type PendingListIds = Arc<Mutex<HashSet<Value>>>;
/// The optional audit log, shared across tasks.
type SharedAudit = Arc<Mutex<Option<AuditLog>>>;

/// A live proxy instance: a spawned upstream child plus the resolved policy used
/// to project its surface. Dropping it tears the child down (`SPEC.md` §10).
///
/// The child's stdio is taken at spawn-and-validate time: the upstream writer
/// ([`Self::up_stdin`]) and the **already-buffered** upstream reader
/// ([`Self::up_reader`]) are held here so the same reader used for the
/// `initialize` + `tools/list` drift probe is threaded straight into the pump —
/// no throwaway [`BufReader`] is created over the child's stdout twice, so bytes
/// the probe buffered past its response newline are never dropped.
#[derive(Debug)]
pub struct Server {
    resolved: ResolvedPolicy,
    child: Child,
    /// The upstream child's stdin, taken at spawn time.
    up_stdin: ChildStdin,
    /// The buffered upstream reader, threaded from the drift probe into the pump
    /// so probe read-ahead is preserved.
    up_reader: BufReader<ChildStdout>,
    /// The upstream tool surface probed at mint time, recorded once into the
    /// audit log at session start as [`AuditEvent::OriginalDefs`] (`SPEC.md`
    /// §8.2, §9.3).
    upstream_defs: Vec<ToolDef>,
}

impl Server {
    /// The resolved policy this server is projecting.
    pub fn resolved(&self) -> &ResolvedPolicy {
        &self.resolved
    }

    /// Run the bridge until the harness or upstream closes, wiring this process's
    /// own stdio to the child.
    ///
    /// Forwards transformed traffic; surfaces upstream crashes loudly rather than
    /// silently dropping calls. The optional `audit` records every rewrite and
    /// rejection under `run_id` (`SPEC.md` §9.3).
    pub async fn run(
        self,
        audit: Option<AuditLog>,
        run_id: impl Into<String>,
    ) -> Result<(), ProxyError> {
        self.run_with(stdin(), stdout(), audit, run_id).await
    }

    /// Run the bridge against caller-supplied downstream streams.
    ///
    /// Identical to [`Server::run`] but takes the harness side explicitly, which
    /// is what the integration tests drive (an in-memory duplex pair) instead of
    /// the process stdio.
    pub async fn run_with<DR, DW>(
        self,
        downstream_in: DR,
        downstream_out: DW,
        audit: Option<AuditLog>,
        run_id: impl Into<String>,
    ) -> Result<(), ProxyError>
    where
        DR: AsyncRead + Unpin + Send + 'static,
        DW: AsyncWrite + Unpin + Send + 'static,
    {
        let run_id = run_id.into();
        // The child's stdio was taken at spawn-and-validate time; reuse the
        // upstream writer and the *same* buffered reader the probe used so no
        // upstream bytes read past the probe response are lost.
        let Server {
            resolved,
            child: _child,
            up_stdin,
            up_reader,
            upstream_defs,
        } = self;

        let policy = Arc::new(resolved.policy.clone());
        let pending: PendingListIds = Arc::new(Mutex::new(HashSet::new()));
        let audit: SharedAudit = Arc::new(Mutex::new(audit));
        let downstream_out: SharedWriter = Arc::new(Mutex::new(Box::new(downstream_out)));

        // Record the trace head once at session start (`SPEC.md` §8.2, §9.3):
        // the upstream surface probed at mint and the resolved policy that
        // projects it. Without these, a live-run log cannot reconstruct the full
        // `run-id → original defs + resolved policy → each rewrite` trace.
        record(
            &audit,
            AuditEvent::OriginalDefs {
                run_id: run_id.clone(),
                defs: upstream_defs,
            },
        )
        .await;
        record(
            &audit,
            AuditEvent::ResolvedPolicy {
                run_id: run_id.clone(),
                policy: resolved.policy.clone(),
            },
        )
        .await;

        // downstream → upstream: rewrite tools/call (or reject), record list ids.
        let d2u = tokio::spawn(pump_downstream(
            BufReader::new(downstream_in),
            Box::new(up_stdin) as Box<dyn AsyncWrite + Unpin + Send>,
            Arc::clone(&downstream_out),
            Arc::clone(&policy),
            Arc::clone(&pending),
            Arc::clone(&audit),
            run_id,
        ));

        // upstream → downstream: project tools/list responses, forward the rest.
        // `up_reader` is the buffered reader threaded from the drift probe.
        let u2d = tokio::spawn(pump_upstream(
            up_reader,
            Arc::clone(&downstream_out),
            Arc::clone(&policy),
            Arc::clone(&pending),
        ));

        // Run both directions until one closes; then drop the child (teardown).
        let _ = tokio::join!(d2u, u2d);
        // `_child` lives until here so `kill_on_drop` tears the upstream down.
        drop(_child);
        Ok(())
    }
}

/// Pump messages from the downstream harness to the upstream child.
///
/// `tools/call` requests are rewritten (or rejected straight back downstream);
/// `tools/list` request ids are recorded for response projection; everything
/// else is forwarded verbatim.
#[allow(clippy::too_many_arguments)]
async fn pump_downstream<R, W>(
    reader: BufReader<R>,
    mut upstream: W,
    downstream: SharedWriter,
    policy: Arc<lagom_core::Policy>,
    pending: PendingListIds,
    audit: SharedAudit,
    run_id: String,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(mut msg) = serde_json::from_str::<Value>(&line) else {
            // Not valid JSON-RPC; forward verbatim and let the peer decide.
            let _ = write_line(&mut upstream, &line).await;
            continue;
        };

        if msg.is_array() {
            // A JSON-RPC 2.0 batch is a top-level array with no `method` key, so
            // it would otherwise fall into the passthrough arm and reach the
            // upstream unexamined — slipping a batched `tools/call` past rewrite
            // and a batched `tools/list` past projection. MCP 2025-11-25 forbids
            // batching, so reject it loudly rather than forward it (SPEC §9.1).
            let mut w = downstream.lock().await;
            if write_value(&mut *w, &batch_rejected()).await.is_err() {
                break;
            }
            continue;
        }

        match method_of(&msg) {
            Some("tools/call") => {
                match handle_tools_call(&msg, &policy, &audit, &run_id).await {
                    Outcome::Forward(rewritten) => {
                        msg["params"] = rewritten;
                        if write_value(&mut upstream, &msg).await.is_err() {
                            break;
                        }
                    }
                    Outcome::Reject(error_result) => {
                        // Never reaches upstream; reply downstream, annotated.
                        let mut w = downstream.lock().await;
                        if write_value(&mut *w, &error_result).await.is_err() {
                            break;
                        }
                    }
                }
            }
            Some("tools/list") => {
                if let Some(id) = msg.get("id").filter(|id| !id.is_null()) {
                    pending.lock().await.insert(id.clone());
                }
                if write_value(&mut upstream, &msg).await.is_err() {
                    break;
                }
            }
            _ => {
                // Passthrough: initialize, notifications, resources, prompts, …
                if write_value(&mut upstream, &msg).await.is_err() {
                    break;
                }
            }
        }
    }
    // Closing our write half signals EOF to the child's stdin.
    let _ = upstream.shutdown().await;
}

/// Pump messages from the upstream child to the downstream harness, projecting
/// any `tools/list` response whose id we recorded.
async fn pump_upstream<R>(
    reader: BufReader<R>,
    downstream: SharedWriter,
    policy: Arc<lagom_core::Policy>,
    pending: PendingListIds,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let mut out_line = line.clone();
        if let Ok(mut msg) = serde_json::from_str::<Value>(&line) {
            let is_list_response = match msg.get("id").filter(|id| !id.is_null()) {
                Some(id) => pending.lock().await.remove(id),
                None => false,
            };
            if is_list_response && project_list_response(&mut msg, &policy) {
                out_line = serde_json::to_string(&msg).unwrap_or(line);
            }
        }
        let mut w = downstream.lock().await;
        if write_line(&mut *w, &out_line).await.is_err() {
            break;
        }
    }
}

/// The decision for a `tools/call`: forward rewritten params, or reject.
enum Outcome {
    /// Forward upstream with these rewritten `params`.
    Forward(Value),
    /// Reply this JSON-RPC error result straight back downstream.
    Reject(Value),
}

/// Apply [`rewrite`] to a `tools/call`, recording the outcome in the audit log.
async fn handle_tools_call(
    msg: &Value,
    policy: &lagom_core::Policy,
    audit: &SharedAudit,
    run_id: &str,
) -> Outcome {
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let projected_call = ToolCall::new(name, arguments);

    match rewrite(&projected_call, policy) {
        Ok(upstream_call) => {
            record(
                audit,
                AuditEvent::Rewrite {
                    run_id: run_id.to_string(),
                    projected: projected_call,
                    upstream: upstream_call.clone(),
                },
            )
            .await;
            // Rebuild params with the rewritten name + arguments, preserving any
            // other params keys (e.g. `_meta`) verbatim.
            let mut new_params = match params {
                Value::Object(map) => map,
                _ => serde_json::Map::new(),
            };
            new_params.insert("name".into(), Value::String(upstream_call.name));
            new_params.insert("arguments".into(), upstream_call.arguments);
            Outcome::Forward(Value::Object(new_params))
        }
        Err(reject) => {
            record(
                audit,
                AuditEvent::Rejection {
                    run_id: run_id.to_string(),
                    projected: projected_call,
                    reason: reject.message.clone(),
                },
            )
            .await;
            Outcome::Reject(reject_result(msg.get("id"), &reject.message))
        }
    }
}

/// Project the `result.tools` array of a `tools/list` response in place.
/// Returns `true` if it found and rewrote a tools array.
fn project_list_response(msg: &mut Value, policy: &lagom_core::Policy) -> bool {
    let Some(tools_val) = msg.pointer("/result/tools").and_then(Value::as_array) else {
        return false;
    };
    let upstream: Vec<ToolDef> = tools_val.iter().filter_map(wire_to_tooldef).collect();
    let projected = project(&upstream, policy);
    let projected_val: Vec<Value> = projected.iter().map(tooldef_to_wire).collect();
    if let Some(result) = msg.get_mut("result").and_then(Value::as_object_mut) {
        result.insert("tools".into(), Value::Array(projected_val));
        return true;
    }
    false
}

/// Convert one MCP wire tool object (`{name, description?, inputSchema}`) into a
/// [`ToolDef`]. lagom-core's [`ToolDef`] uses `input_schema`; the wire uses the
/// MCP-canonical `inputSchema`, so the field is mapped here at the boundary.
fn wire_to_tooldef(wire: &Value) -> Option<ToolDef> {
    let name = wire.get("name").and_then(Value::as_str)?.to_string();
    let description = wire
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let input_schema = wire
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object"}));
    Some(ToolDef {
        name,
        description,
        input_schema,
    })
}

/// Render a [`ToolDef`] back to an MCP wire tool object, preserving the
/// `inputSchema` spelling and omitting an absent description.
fn tooldef_to_wire(def: &ToolDef) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(def.name.clone()));
    if let Some(desc) = &def.description {
        obj.insert("description".into(), Value::String(desc.clone()));
    }
    obj.insert("inputSchema".into(), def.input_schema.clone());
    Value::Object(obj)
}

/// Build a JSON-RPC error response for a rejected call, annotated per `SPEC.md`
/// §9.1 so the agent sees exactly what lagom did.
fn reject_result(id: Option<&Value>, reason: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.cloned().unwrap_or(Value::Null),
        "error": {
            "code": REJECT_ERROR_CODE,
            "message": format!("rejected by lagom: {reason}"),
        }
    })
}

/// Append an audit event if a log is wired in; a write failure is logged to
/// stderr but does not abort the bridge (the call has already been decided).
async fn record(audit: &SharedAudit, event: AuditEvent) {
    let mut guard = audit.lock().await;
    if let Some(log) = guard.as_mut()
        && let Err(e) = log.record(&event)
    {
        eprintln!("lagom: audit write failed: {e}");
    }
}

/// The JSON-RPC `method` of a message, if it is a request/notification.
fn method_of(msg: &Value) -> Option<&str> {
    msg.get("method").and_then(Value::as_str)
}

/// The JSON-RPC error returned downstream when a batch (top-level array) is
/// received. MCP 2025-11-25 forbids batching; lagom refuses rather than forward
/// it past projection.
fn batch_rejected() -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": {
            "code": -32600,
            "message": "lagom: JSON-RPC batching is not supported (forbidden by MCP 2025-11-25)"
        }
    })
}

/// Write a serialized JSON value as one newline-delimited line.
async fn write_value<W: AsyncWrite + Unpin>(w: &mut W, msg: &Value) -> std::io::Result<()> {
    let line = serde_json::to_string(msg).map_err(std::io::Error::other)?;
    write_line(w, &line).await
}

/// Write a pre-serialized line followed by `\n`, then flush.
async fn write_line<W: AsyncWrite + Unpin>(w: &mut W, line: &str) -> std::io::Result<()> {
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

/// Serve a stdio proxy for an already-[`mint`](super::mint)ed resolved policy.
///
/// Spawns the upstream child, validates the policy against its live
/// `tools/list` (`SPEC.md` §5.3), then bridges this process's stdio until the
/// session ends. No audit log; see [`serve_audited`] to wire one in.
pub async fn serve(resolved: ResolvedPolicy) -> Result<(), ProxyError> {
    serve_audited(resolved, None, "run").await
}

/// Like [`serve`] but records every rewrite/rejection to `audit` under `run_id`.
pub async fn serve_audited(
    resolved: ResolvedPolicy,
    audit: Option<AuditLog>,
    run_id: impl Into<String>,
) -> Result<(), ProxyError> {
    let server = spawn_and_validate(resolved).await?;
    server.run(audit, run_id).await
}

/// Spawn the upstream child, perform the MCP `initialize`/`initialized`
/// handshake, fetch its live `tools/list`, and validate the policy against it
/// (`SPEC.md` §5.3). On drift, fail loud — the child is killed and no bridge is
/// started.
///
/// The child's stdin and (already-buffered) stdout are taken here and carried on
/// the returned [`Server`]: the same reader used for the handshake/probe is
/// threaded into the pump, so any upstream bytes the probe buffered past the
/// `tools/list` response newline are preserved (no read-ahead loss).
pub(crate) async fn spawn_and_validate(resolved: ResolvedPolicy) -> Result<Server, ProxyError> {
    let mut child = spawn_child(&resolved)?;
    let mut up_stdin = child.stdin.take().expect("child spawned with piped stdin");
    let up_stdout = child
        .stdout
        .take()
        .expect("child spawned with piped stdout");
    let mut up_reader = BufReader::new(up_stdout);

    let upstream_defs = match probe(&mut up_stdin, &mut up_reader).await {
        Ok(defs) => defs,
        Err(e) => {
            let _ = child.kill().await;
            return Err(e);
        }
    };
    if let Err(drift) = validate(&resolved.policy, &upstream_defs) {
        let _ = child.kill().await;
        return Err(ProxyError::Drift(
            drift.into_iter().map(|d| d.message).collect(),
        ));
    }
    Ok(Server {
        resolved,
        child,
        up_stdin,
        up_reader,
        upstream_defs,
    })
}

/// Spawn the upstream MCP server child with piped stdio.
fn spawn_child(resolved: &ResolvedPolicy) -> Result<Child, ProxyError> {
    let mut cmd = Command::new(&resolved.upstream.command);
    cmd.args(&resolved.upstream.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for (k, v) in &resolved.upstream.env {
        cmd.env(k, v);
    }
    cmd.spawn()
        .map_err(|source| ProxyError::Upstream { source })
}

/// JSON-RPC id lagom uses for the drift-probe `initialize` request.
const INIT_PROBE_ID: &str = "lagom-init-probe";
/// JSON-RPC id lagom uses for the drift-probe `tools/list` request.
const LIST_PROBE_ID: &str = "lagom-drift-probe";

/// Perform the MCP `initialize`/`notifications/initialized` handshake, then issue
/// a one-off `tools/list`, reading the advertised tool surface for drift
/// validation (`SPEC.md` §5.3).
///
/// Real upstreams may reject or ignore `tools/list` before the lifecycle
/// handshake (MCP spec, `2025-11-25` lifecycle), so lagom completes the handshake
/// first: it sends `initialize`, waits for the matching response, then sends the
/// `notifications/initialized` notification before probing tools.
///
/// `reader` is the **same** buffered reader the pump will use, so any bytes the
/// upstream emits right after its probe response (e.g. a queued notification or
/// batched output) stay in the buffer rather than being discarded with a
/// throwaway reader. Dedicated probe ids keep the probe traffic from colliding
/// with the harness's own requests; unrelated lines (e.g. server log
/// notifications) are skipped.
async fn probe<W>(
    upstream: &mut W,
    reader: &mut BufReader<ChildStdout>,
) -> Result<Vec<ToolDef>, ProxyError>
where
    W: AsyncWrite + Unpin,
{
    // 1. initialize request → wait for the matching response.
    let init = json!({
        "jsonrpc": "2.0",
        "id": INIT_PROBE_ID,
        "method": "initialize",
        "params": {
            "protocolVersion": PROBE_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "lagom", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    write_value(upstream, &init)
        .await
        .map_err(|source| ProxyError::Upstream { source })?;
    let _ = read_probe_response(reader, INIT_PROBE_ID, "initialize").await?;

    // 2. notifications/initialized → no response expected.
    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    write_value(upstream, &initialized)
        .await
        .map_err(|source| ProxyError::Upstream { source })?;

    // 3. tools/list → read back the advertised surface.
    let list = json!({
        "jsonrpc": "2.0",
        "id": LIST_PROBE_ID,
        "method": "tools/list",
        "params": {}
    });
    write_value(upstream, &list)
        .await
        .map_err(|source| ProxyError::Upstream { source })?;
    let msg = read_probe_response(reader, LIST_PROBE_ID, "tools/list").await?;

    let line = msg.to_string();
    let tools = msg
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| ProxyError::Upstream {
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("upstream `tools/list` had no result.tools array: {line}"),
            ),
        })?;
    Ok(tools.iter().filter_map(wire_to_tooldef).collect())
}

/// Read lines from `reader` until one is a JSON-RPC response carrying the probe
/// `id`, returning the parsed message. Lines before it (server logs, unrelated
/// notifications) are skipped; EOF before the response is a loud error.
async fn read_probe_response(
    reader: &mut BufReader<ChildStdout>,
    id: &str,
    phase: &str,
) -> Result<Value, ProxyError> {
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|source| ProxyError::Upstream { source })?;
        if n == 0 {
            return Err(ProxyError::Upstream {
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("upstream closed before answering the drift probe `{phase}`"),
                ),
            });
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg.get("id").and_then(Value::as_str) != Some(id) {
            continue;
        }
        return Ok(msg);
    }
}
