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
//! Almost everything is forwarded **verbatim** (passthrough, `CONTEXT.md`).
//! Three message shapes are intercepted:
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
//! - **any upstream → downstream message carrying `result.tools`** — the array
//!   is projected through [`lagom_core::project`] before forwarding. Under MCP
//!   `2025-11-25` a `tools` array under `result` occurs *only* in a `tools/list`
//!   result, so this **shape** — not id correlation — is what makes a message
//!   authority-bearing.
//!
//! The harness's own `initialize`, notifications, resources, prompts and
//! pagination pass through unchanged (`SPEC.md` §10).
//!
//! ## Fail closed on the tool surface
//!
//! Projection used to be gated on id correlation alone: a response whose `id`
//! was not found in the pending `tools/list` set fell through to a verbatim
//! forward. That default is an **authority bypass**, and the mismatch is
//! *downstream-controlled* — no misbehaving upstream required. A gated agent
//! sends `{"id":2.0,"method":"tools/list"}`; a spec-compliant JS/TS server
//! round-trips the number as `2` (or stringifies it as `"2"`); under
//! [`serde_json`] `Number(Float(2.0))`, `Number(PosInt(2))` and `String("2")`
//! are mutually unequal, the lookup misses, and the *entire* projection is
//! skipped — dropped tools reappear and pinned arguments are exposed.
//!
//! Two independent defences, both required:
//!
//! 1. **Normalised correlation** ([`id_key`]) — equal-valued ids in different
//!    JSON representations map to one key, so the ordinary case correlates.
//! 2. **Shape-gated projection** ([`carries_tool_surface`]) — a message that
//!    carries a tool surface is projected *whether or not* it correlated.
//!    Correlation now only decides whether the event is expected (silent) or
//!    anomalous (loud on stderr); it never decides whether to narrow.
//!
//! Projection is a *narrowing* transform, so applying it to a message we cannot
//! correlate is the safe direction; refusing to forward would instead turn
//! benign id-representation drift from a compliant server into a dead session.
//!
//! **Exactly what is guaranteed** (stated narrowly on purpose — an absolute
//! "nothing can forward a raw surface" invariant would be read as covering
//! paths it does not): for an upstream line that [`serde_json`] parses into a
//! single JSON **object**, if [`carries_tool_surface`] holds then that object is
//! projected before it is written downstream, correlated or not, and the arm
//! that cannot re-serialise the projected value forwards
//! [`PROJECTION_FAILED_LINE`] instead of falling back to the original bytes.
//! Correlation state can therefore never decide whether the surface is narrowed.
//!
//! **Every path that sentence does NOT cover**, enumerated rather than implied:
//!
//! - **Top-level array (JSON-RPC batch).** On a [`Value::Array`] both
//!   `msg.get("id")` and `msg.pointer("/result/tools")` yield `None` (`result`
//!   is not a numeric index), so a batched `tools/list` result evaluates the
//!   gate to `false` and *would* be forwarded raw. [`pump_upstream`] therefore
//!   **drops** top-level arrays before the gate runs — the surface never reaches
//!   the agent. Reachability is **unverified**: treat the drop as
//!   defence-in-depth, not a closed live attack path. Batching was permitted by
//!   exactly one revision — added in MCP `2025-03-26` (changelog PR #228),
//!   removed again in `2025-06-18` (PR #416); `2024-11-05` never allowed it. And
//!   although the harness's own `initialize` is forwarded verbatim, lagom's probe
//!   has *already* completed `initialize` at [`PROBE_PROTOCOL_VERSION`] before
//!   that forward happens, and MCP forbids re-initialising, so a strict upstream
//!   would error rather than renegotiate down. Kept regardless: one `is_array`
//!   check that fails closed needs no proven exploit to justify it.
//! - **A line that is not valid JSON.** Still forwarded verbatim, because real
//!   servers log to stdout. A downstream parser more lenient than [`serde_json`]
//!   could in principle read an unprojected response out of such a line.
//!   Accepted and scoped: the *downstream* direction rejects that class
//!   ([`invalid_json_rejected`]), the upstream one does not.
//! - **A tool surface that is not at `/result/tools`.** The gate is a shape
//!   assertion about MCP `2025-11-25` responses, not a search; anything under
//!   another pointer is passthrough by construction.
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
//! [`Mutex`]es.
//!
//! **Teardown.** The *first* direction to close ends the session — a
//! [`tokio::select!`], not a `join!`: the loser is aborted and the child is then
//! terminated and reaped by [`reap`] before [`Server::run_with`] returns. `join!`
//! awaited BOTH, which an executed probe showed to be a live defect: an upstream
//! that answered the drift probe and then ignored stdin EOF kept lagom running
//! after downstream EOF, and kept the child alive with it, because
//! `kill_on_drop` cannot fire while the future that owns the [`Child`] is still
//! suspended. The cost of aborting the loser, stated rather than implied: a
//! message it had already read can be dropped, and a partially-written
//! downstream line (see [`write_line`]) can be truncated — accepted, because the
//! peer on that side is the one that just went away.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

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

/// Budget for reading **one** drift-probe response ([`read_probe_response`]).
///
/// Bounds the whole phase, not each `read_line`: a chatty upstream that emits log
/// lines forever would keep resetting a per-line bound and hang startup anyway.
/// Without any bound the loop was unbounded, so a silent upstream hung
/// `spawn_and_validate` with no diagnostic at all.
///
/// 10s, chosen against the outer bound rather than picked round: the two probe
/// phases (`initialize`, `tools/list`) give a 20s worst case, under the
/// `startup_timeout_sec=25` that lagom's own e2e harness scripts hand the proxy
/// (`e2e/bin/run-codex.sh:44`), so lagom fails with its own message instead of
/// the harness timing lagom out with none. Not configurable: no timeout field is
/// plumbed through [`ResolvedPolicy`] today.
const PROBE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the upstream child may take to exit on its own at session end before
/// lagom escalates to `SIGKILL` ([`reap`]).
///
/// MCP's stdio shutdown sequence is: close the child's stdin, wait for it to
/// exit, then signal (MCP spec, `2025-11-25` transports). [`pump_downstream`]
/// already closes stdin on its way out, so this is that "wait" step — bounded,
/// because an upstream is free to ignore EOF and one demonstrably does.
///
/// 2s: teardown is interactive and the harness is already gone, so latency here
/// is user-visible, yet a cooperative server that flushes state on EOF gets far
/// more than the milliseconds it needs. The bound is safe to keep short because
/// an unblockable `SIGKILL` follows immediately.
const CHILD_EXIT_GRACE: Duration = Duration::from_secs(2);

/// A shared, async writer used by both pump tasks for the downstream side.
type SharedWriter = Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>;
/// The set of in-flight `tools/list` request ids awaiting a response to project,
/// keyed by [`IdKey`] rather than the raw [`Value`] so a re-represented echo
/// still correlates.
type PendingListIds = Arc<Mutex<HashSet<IdKey>>>;
/// The optional audit log, shared across tasks.
type SharedAudit = Arc<Mutex<Option<AuditLog>>>;

/// Pre-serialised JSON-RPC internal error forwarded when a *projected*
/// `tools/list` result cannot be re-serialised.
///
/// A string literal, not a [`json!`] build, so this fail-closed arm cannot
/// itself fail. `id` is deliberately `null`: the id lives inside the message
/// that failed to serialise, so it is exactly the value we must not assume is
/// renderable.
///
/// Consequence, stated precisely: no MCP client correlates `id:null` to a
/// pending request, so the harness does **not** see an error for its
/// `tools/list` — that request stays unanswered until the harness's own timeout,
/// while lagom logs the serialisation failure to stderr. A hung request is the
/// accepted cost of the fail-closed direction; the unprojected surface must not
/// reach the agent. (The arm is near-unreachable: the value was just parsed from
/// JSON and projection only removes/replaces subtrees.)
const PROJECTION_FAILED_LINE: &str = concat!(
    r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"#,
    r#""message":"lagom: could not re-serialise the projected tools/list result; "#,
    r#"refusing to forward the unprojected upstream surface"}}"#
);

/// A canonical correlation key for a JSON-RPC id.
///
/// JSON-RPC 2.0 ids are string | number | null, and a compliant peer MAY echo a
/// numerically-equal id in a *different* representation (JS/TS servers routinely
/// collapse `2.0` → `2`; some stringify). Keying the pending set on the raw
/// [`Value`] made those unequal, which opened the bypass this type exists to
/// close.
///
/// Normalisation rule:
/// - integral numbers (`2`, `2.0`, `-2`, and `u64`s above [`i64::MAX`]) →
///   [`IdKey::Int`], so representation cannot split them;
/// - non-integral numbers → [`IdKey::Float`] keyed on the IEEE-754 bit pattern
///   (JSON-RPC says ids SHOULD NOT have fractional parts, so this is a
///   compatibility bucket, not a supported id shape);
/// - strings that are a *canonical* integer rendering (`"2"`, `"-2"` — but not
///   `"02"`, `"+2"`, `" 2"`) → [`IdKey::Int`], aliasing them onto the numeric
///   form;
/// - all other strings → [`IdKey::Text`].
///
/// **Limits.** Aliasing `"2"` onto `2` is deliberate over-approximation: a
/// harness that uses `2` for one request and `"2"` for another can have the
/// wrong response consume a pending entry. That is bounded and non-leaking —
/// [`project_list_response`] no-ops on a message with no `result.tools`, and the
/// real list response is still narrowed by the shape gate. Non-canonical
/// renderings stay [`IdKey::Text`] precisely to keep that aliasing minimal.
/// A stringified *float* echo (`2` → `"2.0"`) is **not** aliased; the shape gate
/// covers it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum IdKey {
    /// An integer-valued id, widened to `i128` so both `i64` and `u64` JSON
    /// integers share one namespace.
    Int(i128),
    /// A fractional numeric id, keyed on `f64::to_bits` (exact, `Hash`able, and
    /// total over NaN — which [`serde_json`] cannot produce but which costs
    /// nothing to survive).
    Float(u64),
    /// A string id that is not a canonical integer rendering.
    Text(String),
}

/// The correlation key for a JSON-RPC `id`, or `None` if it cannot be one.
///
/// `None` for `null` (JSON-RPC's non-id) and for bool/array/object (malformed):
/// refusing to key them means such a message can never *claim* a pending entry,
/// so a malformed id cannot evict a real one.
fn id_key(id: &Value) -> Option<IdKey> {
    match id {
        Value::Number(n) => Some(if let Some(i) = n.as_i64() {
            IdKey::Int(i.into())
        } else if let Some(u) = n.as_u64() {
            IdKey::Int(u.into())
        } else if let Some(f) = n.as_f64() {
            float_key(f)
        } else {
            // Unreachable without serde_json's `arbitrary_precision` feature.
            // Key on the literal text rather than panicking or (worse) silently
            // declining to correlate.
            IdKey::Text(n.to_string())
        }),
        Value::String(s) => Some(match canonical_int(s) {
            Some(i) => IdKey::Int(i),
            None => IdKey::Text(s.clone()),
        }),
        _ => None,
    }
}

/// Bucket a JSON float id: integral values join the integer namespace so `2.0`
/// and `2` correlate; anything else keeps its exact bit pattern.
///
/// The upper bound is strict because `i64::MAX as f64` rounds *up* past
/// [`i64::MAX`]; without it that float would saturate onto `i64::MAX` and alias
/// a distinct integer id.
fn float_key(f: f64) -> IdKey {
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64 {
        IdKey::Int(f as i64 as i128)
    } else {
        IdKey::Float(f.to_bits())
    }
}

/// Parse `s` as an integer **only** if `s` is its canonical rendering.
///
/// The round-trip check is the point: `"02"`, `"+2"`, `" 2"` all parse to `2`,
/// but aliasing them onto the numeric id `2` would widen the collision window
/// described on [`IdKey`] for no interop benefit.
fn canonical_int(s: &str) -> Option<i128> {
    let n: i128 = s.parse().ok()?;
    (n.to_string() == s).then_some(n)
}

/// Whether an upstream message carries an MCP tool surface, i.e. `result.tools`
/// is an array.
///
/// This — not id correlation — is the projection gate (see the module-level
/// *Fail closed* note). Deliberately **not** also conditioned on the absence of
/// a `method`: no real MCP message carries both a `method` and a *top-level*
/// `result` (server→client requests and notifications carry `params`), so that
/// carve-out bought zero interop while leaving a shape an upstream could aim at
/// to get its full surface forwarded raw. Dropping it is strictly more
/// fail-closed.
///
/// `result.tools` being reachable at all implies `result` is an object, so
/// [`project_list_response`] cannot then fail to rewrite it.
fn carries_tool_surface(msg: &Value) -> bool {
    msg.pointer("/result/tools").is_some_and(Value::is_array)
}

/// A live proxy instance: a spawned upstream child plus the resolved policy used
/// to project its surface.
///
/// Teardown (`SPEC.md` §10) is [`Server::run_with`]'s job, via [`reap`], because
/// dropping the [`Child`] is weaker than it looks: `kill_on_drop` *signals* and
/// then leaves reaping to tokio's process driver on a documented best-effort
/// basis (tokio 1.52.3 `Command::kill_on_drop` caveats), which races the runtime
/// shutdown right behind it. Drop remains the backstop for the paths that never
/// reach `run_with` — a `Server` the caller drops unrun, or a panic — and for
/// those the best-effort caveat still applies.
///
/// The child's stdio is taken at spawn-and-validate time: the upstream writer
/// (`up_stdin`) and the **already-buffered** upstream reader (`up_reader`) are
/// held here so the same reader used for the
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
            child,
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
        let mut d2u = tokio::spawn(pump_downstream(
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
        let mut u2d = tokio::spawn(pump_upstream(
            up_reader,
            Arc::clone(&downstream_out),
            Arc::clone(&policy),
            Arc::clone(&pending),
        ));

        // The FIRST direction to close ends the session (module doc, *Teardown*).
        // This was `tokio::join!`, which awaited both: a stuck upstream that
        // ignored stdin EOF then held this future open forever after downstream
        // EOF, so `child` was never dropped, `kill_on_drop` never fired, and the
        // child outlived a `SIGTERM`ed lagom as an orphan at PPID 1.
        //
        // Each handle is polled by exactly one arm, and the loser is aborted
        // rather than awaited: awaiting a cancellation would reintroduce an
        // unbounded wait on the very task we could not trust to finish.
        let ended = tokio::select! {
            res = &mut d2u => { u2d.abort(); res }
            res = &mut u2d => { d2u.abort(); res }
        };

        // Always before returning, on every arm above: the child is the resource
        // an early return would leak.
        reap(child).await;

        // A pump task can only fail by panicking (both return `()`), and that is
        // a lagom bug — returning `Ok` would report it as a clean session end.
        // `is_cancelled` is the loser we just aborted, which is expected.
        if let Err(e) = ended
            && e.is_panic()
        {
            return Err(ProxyError::Upstream {
                source: std::io::Error::other(format!("lagom bridge pump panicked: {e}")),
            });
        }
        Ok(())
    }
}

/// Terminate **and reap** the upstream child: bounded wait for a self-exit after
/// stdin close, then `SIGKILL`.
///
/// What this guarantees, narrowly: on return, `wait()` has succeeded for the
/// direct child pid unless the `eprintln!` below fired, so lagom cannot exit
/// leaving that pid running or zombied.
///
/// What it does **not** cover, enumerated rather than implied:
///
/// - **Grandchildren.** `SIGKILL` goes to the child pid, not to a process group,
///   so processes the upstream itself spawned (a wrapper script's own children)
///   survive and reparent. Killing the group would need a process-group session
///   lagom does not set up.
/// - **Uninterruptible waits.** A child wedged in kernel `D` state cannot be
///   killed by any signal; `Child::kill` awaits it, so teardown blocks with it.
/// - **`Server`s that never reach [`Server::run_with`]** — see the [`Server`]
///   docs; those still rely on `kill_on_drop`'s best-effort reaping.
async fn reap(mut child: Child) {
    // `pump_downstream` closed the child's stdin on its way out, which is MCP's
    // stdio shutdown signal, so this is the spec's "wait for it to exit" step.
    // `Child::wait` is documented cancel-safe (tokio 1.52.3), so losing this race
    // to the timer cannot lose the exit status for the `kill` below.
    match tokio::time::timeout(CHILD_EXIT_GRACE, child.wait()).await {
        Ok(Ok(_status)) => return,
        Ok(Err(e)) => {
            eprintln!("lagom: waiting for the upstream child failed: {e}; sending SIGKILL");
        }
        Err(_elapsed) => {
            eprintln!(
                "lagom: upstream child did not exit within {}s of stdin close; sending SIGKILL",
                CHILD_EXIT_GRACE.as_secs()
            );
        }
    }
    // `kill` = `start_kill` + `wait`, i.e. signal *and* reap; `start_kill` is
    // `Ok` on an already-exited child (tokio 1.52.3), so the grace-path race is
    // not an error.
    if let Err(e) = child.kill().await {
        // Loud, never swallowed: this is exactly the "orphaned child" outcome
        // this function exists to prevent, so it must not be silent.
        eprintln!("lagom: could not kill and reap the upstream child: {e}");
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
            // Not valid JSON. Forwarding it verbatim would let a payload lagom
            // cannot parse reach an upstream whose parser is more lenient —
            // slipping an unrewritten `tools/call` past the sandbox (a parser-
            // differential bypass). Reject loudly instead, mirroring the batch
            // rejection (SPEC §9.1); MCP stdio requires strict line-delimited
            // JSON, so a compliant harness never hits this.
            let mut w = downstream.lock().await;
            if write_value(&mut *w, &invalid_json_rejected())
                .await
                .is_err()
            {
                break;
            }
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
                // Record the *normalised* id (see [`IdKey`]) so the response is
                // still recognised when the upstream echoes an equal-valued id
                // in another representation. An unkeyable id (null/bool/array/
                // object) is not recorded at all; its response is caught by the
                // shape gate in `pump_upstream` instead, so nothing leaks.
                if let Some(key) = msg.get("id").and_then(id_key) {
                    pending.lock().await.insert(key);
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
/// every message that carries a tool surface.
///
/// Correlation against the pending `tools/list` ids is *diagnostic only*: an
/// uncorrelated tool surface is still projected (and reported on stderr). See the
/// module-level *Fail closed on the tool surface* note for why the correlation
/// gate had to go.
///
/// A top-level array is **dropped, not forwarded**: it is the one shape that
/// defeats both the id lookup and the surface gate, so it is handled before
/// either runs (see the batch bullet in that note).
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
        let out_line = match serde_json::from_str::<Value>(&line) {
            Ok(mut msg) => {
                if msg.is_array() {
                    // A JSON-RPC batch defeats BOTH defences at once: on an
                    // array `get("id")` is None (so `correlated` is false) and
                    // `pointer("/result/tools")` is None (so the shape gate is
                    // false, `result` not being a numeric index), which means a
                    // batched `tools/list` result would take the passthrough arm
                    // and hand the agent the full unprojected surface — dropped
                    // tools and pinned args included. Mirrors the downstream
                    // batch rejection: MCP 2025-11-25 forbids batching, so drop
                    // it loudly rather than project element-wise for a shape no
                    // compliant upstream may emit.
                    //
                    // Dropping (rather than answering) is deliberate: the
                    // elements' ids are the upstream's, and synthesising a
                    // response per element would fabricate results lagom never
                    // saw. Cost: the harness's matching request stays unanswered
                    // until its own timeout. Fail-closed direction chosen over a
                    // live session, because the alternative is the leak itself.
                    eprintln!(
                        "lagom: DROPPED a batched (top-level array) upstream message; \
                         MCP 2025-11-25 forbids JSON-RPC batching and forwarding it \
                         would have bypassed tool-surface projection"
                    );
                    continue;
                }
                // Correlation is looked up ONLY for responses. Server→client
                // requests/notifications (`elicitation/create`,
                // `sampling/createMessage`, …) number their ids in a *separate*
                // namespace that commonly restarts at 0/1, so keying them here
                // would let one silently evict a pending `tools/list` entry and
                // manufacture the "UNCORRELATED tool surface" warning that is
                // supposed to signal an anomaly.
                let correlated = if msg.get("method").is_some() {
                    false
                } else {
                    match msg.get("id").and_then(id_key) {
                        Some(key) => pending.lock().await.remove(&key),
                        None => false,
                    }
                };
                if (correlated || carries_tool_surface(&msg))
                    && project_list_response(&mut msg, &policy)
                {
                    if !correlated {
                        // Loud, because it means either an upstream whose id
                        // representation drifted or an unsolicited surface — and
                        // the pre-fix code forwarded this case raw.
                        eprintln!(
                            "lagom: projected an UNCORRELATED tool surface (id {}); \
                             forwarding it unprojected would have leaked the full \
                             upstream surface",
                            msg.get("id").unwrap_or(&Value::Null)
                        );
                    }
                    // Never fall back to `line` on failure: that is precisely the
                    // raw-surface leak this arm exists to prevent.
                    serde_json::to_string(&msg).unwrap_or_else(|e| {
                        eprintln!("lagom: could not re-serialise projected tools/list: {e}");
                        PROJECTION_FAILED_LINE.to_string()
                    })
                } else {
                    line
                }
            }
            // Not JSON: not a JSON-RPC response and not a tool surface. Real
            // servers log to stdout, so this stays verbatim passthrough (see the
            // module-level residual note).
            Err(_) => line,
        };
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

/// The JSON-RPC error returned downstream when a line is not valid JSON. lagom
/// never forwards bytes it could not parse: an upstream with a more lenient
/// parser could otherwise be reached with an unrewritten payload
/// (parser-differential bypass, `SPEC.md` §9.1).
fn invalid_json_rejected() -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": {
            "code": -32700,
            "message": "lagom: line is not valid JSON; refusing to forward unparsed bytes upstream"
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
pub async fn spawn_and_validate(resolved: ResolvedPolicy) -> Result<Server, ProxyError> {
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
            // `kill` = SIGKILL + `wait`, so the child is reaped here rather than
            // left to `kill_on_drop`'s best-effort reaping. Discarding its error
            // is deliberate: the probe failure below is the diagnosis the caller
            // needs, and a kill error cannot be repaired here — `reap` explains
            // the residuals this shares (grandchildren, `D` state).
            let _ = child.kill().await;
            return Err(e);
        }
    };
    if let Err(drift) = validate(&resolved.policy, &upstream_defs) {
        // Same reason as the probe arm above: reap now, report the drift.
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
///
/// Bounded by [`PROBE_RESPONSE_TIMEOUT`], which is what makes a *silent* upstream
/// distinguishable from a slow one: the loop itself has no exit condition other
/// than the wanted response or EOF, so an upstream that connects, says nothing and
/// never closes stdout used to hang `spawn_and_validate` forever with no
/// diagnostic. The bound wraps the whole loop, so skipped lines cannot extend it.
async fn read_probe_response(
    reader: &mut BufReader<ChildStdout>,
    id: &str,
    phase: &str,
) -> Result<Value, ProxyError> {
    let read = async {
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
    };
    // `TimedOut` (not `UnexpectedEof`): callers branch on the typed kind, and a
    // silent-but-open upstream is a different fault from one that hung up.
    tokio::time::timeout(PROBE_RESPONSE_TIMEOUT, read)
        .await
        .unwrap_or_else(|_elapsed| {
            Err(ProxyError::Upstream {
                source: std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "upstream did not answer the drift probe `{phase}` within {}s",
                        PROBE_RESPONSE_TIMEOUT.as_secs()
                    ),
                ),
            })
        })
}

#[cfg(test)]
mod tests {
    //! Authority tests for the upstream direction.
    //!
    //! These drive the **real** [`pump_downstream`] / [`pump_upstream`] tasks and
    //! the **real** shared `pending` set over in-memory duplex pipes; only the
    //! peer processes are replaced by pipes, so the upstream stub is a data
    //! provider, never a stand-in for lagom logic. No child process is spawned,
    //! which lets a test choose the exact id bytes a real server would echo.

    use std::collections::BTreeMap;

    use lagom_core::{ArgPolicy, Policy, Presence, ToolPolicy};
    use tokio::io::DuplexStream;

    use super::*;
    use crate::UpstreamCommand;

    /// The upstream surface the stub advertises: one tool that must survive with
    /// a pinned arg hidden, one that must vanish. Any unprojected leak is
    /// therefore visible two independent ways.
    fn upstream_tools() -> Value {
        json!([
            {
                "name": "search",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "artifact": {"type": "string"},
                        "query": {"type": "string"}
                    }
                }
            },
            {"name": "secret", "inputSchema": {"type": "object"}}
        ])
    }

    /// Drop `secret`, pin `search.artifact`.
    fn narrowing_policy() -> Policy {
        let mut args = BTreeMap::new();
        args.insert("artifact".to_string(), ArgPolicy::Pin(json!("hylla")));
        let mut tools = BTreeMap::new();
        tools.insert(
            "search".to_string(),
            ToolPolicy {
                args,
                ..Default::default()
            },
        );
        tools.insert(
            "secret".to_string(),
            ToolPolicy {
                presence: Some(Presence::Drop),
                ..Default::default()
            },
        );
        Policy {
            default_presence: Presence::Keep,
            tools,
        }
    }

    /// A `tools/list` result line with the given raw id JSON, as an upstream
    /// would emit it.
    fn list_result(raw_id: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":{raw_id},"result":{{"tools":{}}}}}"#,
            upstream_tools()
        )
    }

    /// Assert a downstream `tools/list` response was actually projected.
    fn assert_projected(resp: &Value) {
        let tools = resp
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("response has no result.tools: {resp}"));
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(
            !names.contains(&"secret"),
            "AUTHORITY BYPASS: dropped tool reached the agent: {resp}"
        );
        assert!(names.contains(&"search"), "kept tool must survive: {resp}");
        let search = tools.iter().find(|t| t["name"] == "search").unwrap();
        assert!(
            search.pointer("/inputSchema/properties/artifact").is_none(),
            "AUTHORITY BYPASS: pinned arg exposed in the projected schema: {resp}"
        );
    }

    /// Both production pumps wired over duplex pipes, sharing one `pending` set
    /// and one downstream writer exactly as [`Server::run_with`] does.
    struct Wired {
        to_proxy: DuplexStream,
        from_proxy: BufReader<DuplexStream>,
        upstream_rx: BufReader<DuplexStream>,
        upstream_tx: DuplexStream,
        /// The very set the two production pumps share, so a test can observe
        /// correlation bookkeeping (eviction) directly instead of inferring it
        /// from stderr.
        pending: PendingListIds,
    }

    fn wire(policy: Policy) -> Wired {
        let (to_proxy, proxy_down_in) = tokio::io::duplex(64 * 1024);
        let (proxy_down_out, from_proxy) = tokio::io::duplex(64 * 1024);
        let (proxy_up_out, upstream_rx) = tokio::io::duplex(64 * 1024);
        let (upstream_tx, proxy_up_in) = tokio::io::duplex(64 * 1024);

        let policy = Arc::new(policy);
        let pending: PendingListIds = Arc::new(Mutex::new(HashSet::new()));
        let down: SharedWriter = Arc::new(Mutex::new(Box::new(proxy_down_out)));
        let audit: SharedAudit = Arc::new(Mutex::new(None));

        tokio::spawn(pump_downstream(
            BufReader::new(proxy_down_in),
            proxy_up_out,
            Arc::clone(&down),
            Arc::clone(&policy),
            Arc::clone(&pending),
            audit,
            "test-run".to_string(),
        ));
        tokio::spawn(pump_upstream(
            BufReader::new(proxy_up_in),
            down,
            policy,
            Arc::clone(&pending),
        ));

        Wired {
            to_proxy,
            from_proxy: BufReader::new(from_proxy),
            upstream_rx: BufReader::new(upstream_rx),
            upstream_tx,
            pending,
        }
    }

    impl Wired {
        /// Downstream harness → proxy.
        async fn agent_send(&mut self, msg: Value) {
            let line = format!("{}\n", serde_json::to_string(&msg).unwrap());
            self.to_proxy.write_all(line.as_bytes()).await.unwrap();
            self.to_proxy.flush().await.unwrap();
        }

        /// The next raw line the proxy delivered to the harness.
        async fn agent_recv(&mut self) -> String {
            let mut line = String::new();
            let n = self.from_proxy.read_line(&mut line).await.unwrap();
            assert!(n > 0, "proxy closed before answering");
            line.trim_end_matches('\n').to_string()
        }

        /// The next line the proxy forwarded upstream.
        async fn upstream_recv(&mut self) -> Value {
            let mut line = String::new();
            let n = self.upstream_rx.read_line(&mut line).await.unwrap();
            assert!(n > 0, "proxy closed the upstream side");
            serde_json::from_str(&line).unwrap()
        }

        /// Upstream stub → proxy, byte-exact (so a test controls the id literal).
        async fn upstream_send_raw(&mut self, line: &str) {
            self.upstream_tx.write_all(line.as_bytes()).await.unwrap();
            self.upstream_tx.write_all(b"\n").await.unwrap();
            self.upstream_tx.flush().await.unwrap();
        }
    }

    /// Round-trip a `tools/list` whose request id and echoed id are written
    /// exactly as given, asserting the response was projected.
    async fn assert_echo_variant_projected(request_id: Value, echoed_id_raw: &str) {
        let mut w = wire(narrowing_policy());
        w.agent_send(json!({
            "jsonrpc": "2.0", "id": request_id, "method": "tools/list", "params": {}
        }))
        .await;
        let forwarded = w.upstream_recv().await;
        assert_eq!(
            forwarded["method"],
            json!("tools/list"),
            "the request itself is forwarded verbatim"
        );
        w.upstream_send_raw(&list_result(echoed_id_raw)).await;
        let resp: Value = serde_json::from_str(&w.agent_recv().await).unwrap();
        assert_projected(&resp);
    }

    #[tokio::test]
    async fn float_id_echoed_as_integer_is_still_projected() {
        // The agent-controlled trigger: `2.0` out, `2` back — what a spec-
        // compliant JS/TS server does to a JSON number. Pre-fix the pending set
        // held `Number(Float(2.0))`, the lookup for `Number(PosInt(2))` missed,
        // and the raw surface was forwarded.
        assert_echo_variant_projected(json!(2.0), "2").await;
    }

    #[tokio::test]
    async fn integer_id_echoed_as_float_is_still_projected() {
        // The mirror case: `2` out, `2.0` back.
        assert_echo_variant_projected(json!(2), "2.0").await;
    }

    #[tokio::test]
    async fn integer_id_echoed_as_string_is_still_projected() {
        // A stringifying upstream: `3` out, `"3"` back.
        assert_echo_variant_projected(json!(3), "\"3\"").await;
    }

    #[tokio::test]
    async fn string_id_echoed_as_integer_is_still_projected() {
        // And the mirror: `"4"` out, `4` back.
        assert_echo_variant_projected(json!("4"), "4").await;
    }

    #[tokio::test]
    async fn uncorrelated_tool_surface_is_projected_not_leaked() {
        // Nothing can correlate — no `tools/list` was ever sent. Pre-fix this
        // fell straight through to a verbatim forward, handing the agent the
        // full upstream surface without any misbehaviour on the agent's part
        // being required.
        let mut w = wire(narrowing_policy());
        w.upstream_send_raw(&list_result("\"never-requested\""))
            .await;
        let resp: Value = serde_json::from_str(&w.agent_recv().await).unwrap();
        assert_projected(&resp);
    }

    #[tokio::test]
    async fn unkeyable_request_id_still_yields_a_projected_response() {
        // A bool id is malformed JSON-RPC, so it is never recorded as pending;
        // the shape gate must catch its response anyway.
        let mut w = wire(narrowing_policy());
        w.agent_send(json!({
            "jsonrpc": "2.0", "id": true, "method": "tools/list", "params": {}
        }))
        .await;
        let _ = w.upstream_recv().await;
        w.upstream_send_raw(&list_result("true")).await;
        let resp: Value = serde_json::from_str(&w.agent_recv().await).unwrap();
        assert_projected(&resp);
    }

    #[tokio::test]
    async fn unrelated_upstream_traffic_passes_through_byte_identically() {
        // Regression guard on the *other* side of the fix: shape-gating must not
        // start touching ordinary traffic. (This case also passed pre-fix — it is
        // evidence the fix does not over-reach, not evidence of the bypass.)
        let mut w = wire(narrowing_policy());
        for raw in [
            // notification: no id at all
            r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#,
            // tools/call result: has an id, carries no tool surface
            r#"{"jsonrpc":"2.0","id":9,"result":{"content":[{"type":"text","text":"ok"}]}}"#,
            // resources/list result: a `resources` array, not `tools`
            r#"{"jsonrpc":"2.0","id":10,"result":{"resources":[{"uri":"file:///a"}]}}"#,
            // prompts/list result
            r#"{"jsonrpc":"2.0","id":11,"result":{"prompts":[{"name":"p"}]}}"#,
            // passthrough error
            r#"{"jsonrpc":"2.0","id":12,"error":{"code":-32601,"message":"nope"}}"#,
            // server→client request that merely *mentions* a tools array
            r#"{"jsonrpc":"2.0","id":13,"method":"sampling/createMessage","params":{"result":{"tools":[]}}}"#,
            // `result.tools` present but not an array
            r#"{"jsonrpc":"2.0","id":14,"result":{"tools":"not-an-array"}}"#,
            // non-JSON stdout noise from a chatty upstream
            r#"upstream log: listening"#,
        ] {
            w.upstream_send_raw(raw).await;
            assert_eq!(
                w.agent_recv().await,
                raw,
                "unrelated upstream line must pass through byte-identically"
            );
        }
    }

    #[tokio::test]
    async fn batched_upstream_surface_is_dropped_not_forwarded() {
        // The residual bypass the id-correlation fix did NOT close. On a
        // top-level array `get("id")` and `pointer("/result/tools")` are both
        // None, so pre-fix the batch evaluated `correlated || carries_tool_surface`
        // to false and took the passthrough arm — handing the agent the raw
        // surface (dropped tool `secret` + pinned `search.artifact`). Whether a
        // live session can reach the shape is UNVERIFIED (see the batch bullet in
        // the module doc: only MCP 2025-03-26 ever allowed batching, and lagom's
        // probe has already completed `initialize` before the harness's own is
        // forwarded); this locks the fail-closed drop regardless.
        let mut w = wire(narrowing_policy());
        let batch = format!("[{}]", list_result("2"));
        w.upstream_send_raw(&batch).await;

        // A sentinel proves the drop *deterministically*: if the batch had been
        // forwarded it would arrive first, so the next downstream line being the
        // sentinel means the batch never reached the agent.
        let sentinel = r#"{"jsonrpc":"2.0","id":99,"result":{"content":[]}}"#;
        w.upstream_send_raw(sentinel).await;
        let got = w.agent_recv().await;
        assert_eq!(
            got, sentinel,
            "AUTHORITY BYPASS: a batched upstream message reached the agent: {got}"
        );
        assert!(
            !got.contains("secret") && !got.contains("artifact"),
            "AUTHORITY BYPASS: batched surface leaked: {got}"
        );
    }

    #[tokio::test]
    async fn server_request_reusing_a_pending_id_does_not_evict_it() {
        // Server→client requests number ids in their own namespace, so an
        // `elicitation/create` with id 2 must not consume the pending
        // `tools/list` id 2. Non-leaking either way (the shape gate still
        // narrows), but eviction manufactures the "UNCORRELATED tool surface"
        // warning that is meant to signal an anomaly.
        let mut w = wire(narrowing_policy());
        w.agent_send(json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
        }))
        .await;
        let _ = w.upstream_recv().await;
        assert!(
            w.pending.lock().await.contains(&IdKey::Int(2)),
            "the tools/list id must be pending before the server request arrives"
        );

        let server_req =
            r#"{"jsonrpc":"2.0","id":2,"method":"elicitation/create","params":{"message":"hi"}}"#;
        w.upstream_send_raw(server_req).await;
        // Receiving the passthrough line means the pump already ran its
        // correlation step for that message, so `pending` is settled here.
        assert_eq!(
            w.agent_recv().await,
            server_req,
            "a server→client request must pass through untouched"
        );
        assert!(
            w.pending.lock().await.contains(&IdKey::Int(2)),
            "a method-bearing server request must not evict a pending tools/list id"
        );

        // And the real response still correlates (and is still narrowed).
        w.upstream_send_raw(&list_result("2")).await;
        let resp: Value = serde_json::from_str(&w.agent_recv().await).unwrap();
        assert_projected(&resp);
        assert!(
            !w.pending.lock().await.contains(&IdKey::Int(2)),
            "the real response must be the message that consumes the pending id"
        );
    }

    #[tokio::test]
    async fn aliased_id_collision_still_projects_the_real_surface() {
        // Documents the accepted limit on `IdKey`: `2` and `"2"` alias, so a
        // tools/call response can consume the pending tools/list entry. The
        // consequence must be *nothing but* a lost correlation — the tools/call
        // result passes through untouched and the real list response is still
        // narrowed by the shape gate. (Also passes pre-fix, where no aliasing
        // existed; its job is to bound the new aliasing's blast radius.)
        let mut w = wire(narrowing_policy());
        w.agent_send(json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
        }))
        .await;
        let _ = w.upstream_recv().await;

        let call_result =
            r#"{"jsonrpc":"2.0","id":"2","result":{"content":[{"type":"text","text":"ok"}]}}"#;
        w.upstream_send_raw(call_result).await;
        assert_eq!(
            w.agent_recv().await,
            call_result,
            "an aliased non-list response must still pass through untouched"
        );

        w.upstream_send_raw(&list_result("2")).await;
        let resp: Value = serde_json::from_str(&w.agent_recv().await).unwrap();
        assert_projected(&resp);
    }

    #[test]
    fn id_key_aliases_equal_values_across_representations() {
        // Unit-level statement of the normalisation rule. (New symbol, so it
        // could not have run pre-fix; the round-trip tests above carry the
        // fails-before evidence.)
        let two = IdKey::Int(2);
        assert_eq!(id_key(&json!(2)), Some(two.clone()));
        assert_eq!(id_key(&json!(2.0)), Some(two.clone()));
        assert_eq!(id_key(&json!("2")), Some(two));
        assert_eq!(id_key(&json!(-2)), Some(IdKey::Int(-2)));
        assert_eq!(id_key(&json!("-2")), Some(IdKey::Int(-2)));
        assert_eq!(id_key(&json!(-0.0)), Some(IdKey::Int(0)));
        // u64 above i64::MAX shares the integer namespace.
        assert_eq!(
            id_key(&json!(u64::MAX)),
            Some(IdKey::Int(i128::from(u64::MAX)))
        );
    }

    #[test]
    fn id_key_keeps_non_canonical_and_non_numeric_ids_distinct() {
        // Non-canonical renderings deliberately do NOT alias onto `2`.
        for s in ["02", "+2", " 2", "2 ", "2.0", "", "abc"] {
            assert_eq!(
                id_key(&json!(s)),
                Some(IdKey::Text(s.to_string())),
                "{s:?} must stay a text key"
            );
        }
        // Fractional ids keep their exact bit pattern.
        assert_eq!(id_key(&json!(2.5)), Some(IdKey::Float(2.5f64.to_bits())));
        assert_ne!(id_key(&json!(2.5)), id_key(&json!(2)));
        // Not correlatable at all.
        assert_eq!(id_key(&Value::Null), None);
        assert_eq!(id_key(&json!(true)), None);
        assert_eq!(id_key(&json!([2])), None);
        assert_eq!(id_key(&json!({"a": 2})), None);
    }

    #[test]
    fn projection_failed_line_is_a_valid_single_line_error_response() {
        // Hand-written literal (deliberately, so the fail-closed arm cannot
        // itself fail) — so its shape must be asserted, not assumed.
        let v: Value = serde_json::from_str(PROJECTION_FAILED_LINE)
            .expect("the fail-closed line must be valid JSON");
        assert_eq!(v["jsonrpc"], json!("2.0"));
        assert_eq!(v["id"], Value::Null);
        assert_eq!(v["error"]["code"], json!(-32603));
        assert!(v["error"]["message"].as_str().unwrap().contains("lagom:"));
        assert!(
            v.get("result").is_none(),
            "must carry no tool surface of its own"
        );
        assert!(
            !PROJECTION_FAILED_LINE.contains('\n'),
            "newline-delimited framing forbids an embedded newline"
        );
    }

    #[test]
    fn carries_tool_surface_only_matches_a_response_tools_array() {
        assert!(carries_tool_surface(
            &json!({"id": 1, "result": {"tools": []}})
        ));
        // No id: a malformed but still authority-bearing response.
        assert!(carries_tool_surface(&json!({"result": {"tools": []}})));
        // A `method` is NOT an exemption: real requests/notifications put their
        // payload under `params`, so a top-level `result.tools` alongside a
        // `method` is malformed-but-authority-bearing and must still narrow.
        assert!(carries_tool_surface(
            &json!({"method": "x", "result": {"tools": []}})
        ));
        // …while the real server→client request shape (payload under `params`)
        // stays untouched.
        assert!(!carries_tool_surface(
            &json!({"method": "sampling/createMessage", "params": {"result": {"tools": []}}})
        ));
        assert!(!carries_tool_surface(
            &json!({"id": 1, "result": {"tools": "no"}})
        ));
        assert!(!carries_tool_surface(&json!({"id": 1, "result": {}})));
        assert!(!carries_tool_surface(
            &json!({"id": 1, "error": {"code": -1}})
        ));
    }

    // ---- Teardown: real child processes, no pipes standing in for them --------
    //
    // These two spawn actual upstream children, because the defect they lock is
    // about process lifetime: a pipe cannot be orphaned at PPID 1.

    /// An upstream stub that answers the drift probe and then **ignores stdin
    /// EOF** — the stuck upstream an executed probe used to leave orphaned.
    ///
    /// `sh` consumes exactly the three lines the probe writes (`initialize`, the
    /// `notifications/initialized`, `tools/list`), answers the two requests under
    /// lagom's dedicated probe ids, then `exec sleep`s: stdout stays open (so
    /// [`pump_upstream`] can never reach EOF) and `sleep` treats stdin EOF as
    /// nothing at all.
    fn stuck_upstream() -> UpstreamCommand {
        let init = concat!(
            r#"{"jsonrpc":"2.0","id":"lagom-init-probe","result":{"protocolVersion":"2025-11-25","#,
            r#""capabilities":{},"serverInfo":{"name":"stuck-stub","version":"0"}}}"#
        );
        let list = concat!(
            r#"{"jsonrpc":"2.0","id":"lagom-drift-probe","result":{"tools":"#,
            r#"[{"name":"search","inputSchema":{"type":"object"}}]}}"#
        );
        UpstreamCommand {
            command: "sh".into(),
            args: vec![
                "-c".into(),
                format!(
                    "read _l; printf '%s\\n' '{init}'; read _l; read _l; \
                     printf '%s\\n' '{list}'; exec sleep 300"
                ),
            ],
            env: vec![],
        }
    }

    /// An upstream that spawns, never writes a byte, and never closes stdout.
    fn silent_upstream() -> UpstreamCommand {
        UpstreamCommand {
            command: "sleep".into(),
            args: vec!["300".into()],
            env: vec![],
        }
    }

    /// The OS-reported process state for `pid`, or `None` if no such process
    /// exists.
    ///
    /// `ps` rather than `libc::kill(pid, 0)` because lagom-proxy has no `libc`
    /// dependency — and because `kill(pid, 0)` *succeeds* for a zombie, so it
    /// cannot tell "reaped" from "signalled but leaked". `ps` reports `Z` for the
    /// leak. Residual: pids are reusable, so `None` is conclusive only in
    /// practice, not in principle.
    fn process_state(pid: u32) -> Option<String> {
        let out = std::process::Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
            .expect("`ps` must be available to check for a leaked child");
        if !out.status.success() {
            return None;
        }
        let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!state.is_empty()).then_some(state)
    }

    #[tokio::test]
    async fn downstream_eof_ends_the_session_and_reaps_a_stuck_upstream() {
        let server = spawn_and_validate(ResolvedPolicy {
            policy: Policy::passthrough(),
            upstream: stuck_upstream(),
        })
        .await
        .expect("the stub answers the drift probe, so spawn+validate must succeed");
        let pid = server
            .child
            .id()
            .expect("the stub must still be running after the probe");

        // Downstream EOF holds *by construction*: the write half is dropped
        // before the read half is handed to the pump, so `next_line` can only
        // return `Ok(None)`. No sleep and no ordering assumption — the upstream
        // direction can never finish (the stub holds stdout open forever), so the
        // only way `run_with` can return is the first-to-finish teardown.
        let (down_writer, down_in) = tokio::io::duplex(1024);
        drop(down_writer);
        let (down_out, _down_rx) = tokio::io::duplex(64 * 1024);

        // A failure detector, not a synchroniser: the pass path returns after
        // roughly `CHILD_EXIT_GRACE` (the stub ignores EOF, so it is SIGKILLed),
        // while the pre-fix `tokio::join!` never returned at all.
        let ran = tokio::time::timeout(
            Duration::from_secs(30),
            server.run_with(down_in, down_out, None, "test-run"),
        )
        .await
        .expect(
            "downstream EOF must end the session: `tokio::join!` waited on the \
             stuck upstream forever, leaving lagom and the child alive",
        );
        ran.expect("a session ended by downstream EOF is not an error");

        assert_eq!(
            process_state(pid),
            None,
            "ORPHANED CHILD: the upstream outlived the session (state `Z` = \
             killed but never reaped, anything else = still running)"
        );
    }

    #[tokio::test]
    async fn a_silent_upstream_fails_the_probe_loudly_instead_of_hanging() {
        // Deterministic without timing luck: `sleep` cannot write and does not
        // exit, so the probe read has exactly ONE reachable outcome whatever the
        // scheduler does — there is no race to lose. Pre-fix `read_probe_response`
        // had no bound at all, and this test hung forever instead of failing.
        //
        // Cost, stated because it is real: this burns a full
        // `PROBE_RESPONSE_TIMEOUT` (10s) of wall clock. Collapsing it needs
        // `tokio/test-util`'s `start_paused` clock (whose auto-advance skips an
        // idle timer), and that dev-feature is not enabled for this crate.

        let err = spawn_and_validate(ResolvedPolicy {
            policy: Policy::passthrough(),
            upstream: silent_upstream(),
        })
        .await
        .expect_err("a silent upstream must fail the probe, not hang startup");

        let ProxyError::Upstream { source } = &err else {
            panic!("a probe timeout must surface as ProxyError::Upstream, got {err:?}");
        };
        assert_eq!(
            source.kind(),
            std::io::ErrorKind::TimedOut,
            "the typed kind is what callers branch on: {source}"
        );
        let msg = source.to_string();
        assert!(
            msg.contains("`initialize`") && msg.contains("10s"),
            "the error must name the stalled phase and the budget: {msg}"
        );
    }
}
