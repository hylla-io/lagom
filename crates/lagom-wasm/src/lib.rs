//! # lagom-wasm
//!
//! The wasm32 face of [`lagom_core`] (`SPEC.md` §7.3, ADR-0001 "Go is the
//! problem child"). Native embedding of the Rust core into Go would need cgo +
//! a C toolchain + per-platform archives, which breaks `go get`. Instead the
//! transport-less core is compiled to `wasm32-unknown-unknown` and run via
//! `wazero` (a pure-Go wasm runtime, no cgo) from the `lagom-go` module.
//!
//! Only the **transport-less** operations are exposed — [`project`],
//! [`rewrite`], [`merge`], [`validate`]. wasm is pure compute: it cannot spawn
//! the upstream child or do stdio, so `mint_stdio_server` / the CLI stay native
//! (ADR-0001 consequences).
//!
//! ## Memory ABI
//!
//! wasm has no native string/JSON marshalling, so every operation crosses the
//! boundary as a **UTF-8 JSON byte buffer in linear memory**, mirroring the
//! JSON-in/JSON-out contract the Python binding uses. The host (Go) drives it:
//!
//! 1. [`alloc`]`(len) -> ptr` — reserve `len` bytes; the host copies its input
//!    JSON there.
//! 2. call e.g. [`project`]`(in_ptr, in_len) -> packed` — runs the engine and
//!    returns a **packed `u64`**: `(out_ptr << 32) | out_len`. On
//!    `wasm32-unknown-unknown` a pointer is a 32-bit linear-memory offset, so it
//!    fits losslessly in the high half.
//! 3. the host reads `out_len` bytes at `out_ptr`. The first byte is a **status
//!    tag** (`0` = ok, `1` = error); the remaining bytes are the result JSON
//!    (for ok) or the engine's error message (for error). This keeps the
//!    never-swallow contract (`SPEC.md` §9.1): an engine [`Reject`] /
//!    [`MergeError`] / drift surfaces as a tagged error string, not a silent
//!    empty result.
//! 4. [`dealloc`]`(ptr, len)` — the host frees both the input buffer it
//!    allocated *and* the result buffer once it has copied the bytes out.
//!
//! All four operations take `(in_ptr, in_len)` of a single JSON object carrying
//! their named arguments (see each function), which keeps the wasm import
//! signatures uniform (one ptr/len pair in, one packed value out).
//!
//! ## Testability split
//!
//! The exported `extern "C"` functions are *thin* wrappers that only marshal the
//! linear-memory buffer; the real work lives in the `*_str` inner functions,
//! which take an input `&str` and return a `(tag, payload)` pair. The inner
//! functions (engine glue + the never-swallow tagging) are unit-tested on the
//! host triple; the linear-memory ABI itself (alloc/pack/dealloc) is only
//! meaningful on `wasm32` (32-bit pointers) and is exercised end-to-end by the
//! `lagom-go` test driving the real `.wasm` through wazero.

use lagom_core::{
    MintRecord, Policy, ToolCall, ToolDef, UpstreamCommand, merge as core_merge,
    mint as core_mint, project as core_project, refire as core_refire, rewrite as core_rewrite,
    validate as core_validate,
};
use serde::Deserialize;

/// Status tag written as the first byte of every result buffer: the operation
/// succeeded and the rest of the buffer is the result JSON.
const STATUS_OK: u8 = 0;
/// Status tag written as the first byte of every result buffer: the operation
/// failed and the rest of the buffer is a UTF-8 error message.
const STATUS_ERR: u8 = 1;

// ---------------------------------------------------------------------------
// Memory ABI (wasm linear memory; 32-bit pointers on the wasm32 target).
// ---------------------------------------------------------------------------

/// Allocate `len` bytes of wasm linear memory and return a pointer to them.
///
/// The host calls this to obtain a buffer it then fills with input JSON before
/// invoking an operation. The allocation is a boxed slice (capacity == `len`
/// exactly) leaked to the caller; it must be returned via [`dealloc`] with the
/// same `len`.
///
/// # Safety
///
/// Exported for the wasm host (wazero). The returned pointer must be passed back
/// to [`dealloc`] with the same `len` to avoid a leak.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: u32) -> *mut u8 {
    // A boxed slice has capacity == len exactly, so [`dealloc`] can reconstruct
    // it from `(ptr, len)` without tracking a separate capacity (unlike
    // `Vec::with_capacity`, which may over-allocate and make `from_raw_parts` UB).
    let buf: Box<[u8]> = vec![0u8; len as usize].into_boxed_slice();
    Box::into_raw(buf) as *mut u8
}

/// Free `len` bytes previously returned by [`alloc`] (or by an operation's
/// result buffer, which is also a boxed-slice allocation).
///
/// # Safety
///
/// `ptr`/`len` must be exactly a pair returned by [`alloc`] (directly, or as the
/// `(ptr, len)` half of an operation's packed result) and not already freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: u32) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr`/`len` is a boxed-slice allocation from `alloc`/`emit`, whose
    // capacity equals `len`; reconstructing the `Box<[u8]>` reclaims it exactly.
    unsafe {
        let slice = std::ptr::slice_from_raw_parts_mut(ptr, len as usize);
        drop(Box::from_raw(slice));
    }
}

/// Pack an `(ptr, len)` pair into the `u64` operation result: `(ptr << 32) | len`.
///
/// On `wasm32` a pointer is a 32-bit offset, so the high half is lossless. The
/// `as u32` cast is therefore only correct on a 32-bit-pointer target — the
/// host never calls this (host tests use the `*_str` inner functions).
fn pack(ptr: *mut u8, len: u32) -> u64 {
    ((ptr as usize as u64) << 32) | (len as u64)
}

/// Build a tagged result buffer (`[tag, ..bytes]`), leak it as a boxed slice,
/// and return it packed for the host. The host reads `len` bytes at `ptr`,
/// inspects byte 0 as the status tag, then frees the buffer via [`dealloc`].
fn emit(tag: u8, bytes: &[u8]) -> u64 {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(tag);
    out.extend_from_slice(bytes);
    let len = out.len() as u32;
    // Box the slice so capacity == len exactly, matching the [`dealloc`]
    // reconstruction (which assumes `(ptr, len)` is the whole allocation).
    let ptr = Box::into_raw(out.into_boxed_slice()) as *mut u8;
    pack(ptr, len)
}

/// Read the host input buffer as a `&str`, run `f`, and `emit` its `(tag,
/// payload)` result through the memory ABI. The single place the exported
/// functions touch raw pointers.
///
/// # Safety
///
/// `ptr`/`len` must describe a live, host-allocated UTF-8 byte range for the
/// duration of the call (the host frees it only after the call returns).
unsafe fn dispatch(ptr: *const u8, len: u32, f: impl FnOnce(&str) -> (u8, String)) -> u64 {
    // SAFETY: the host guarantees `ptr`/`len` is a live byte range for the call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let (tag, payload) = match std::str::from_utf8(bytes) {
        Ok(s) => f(s),
        Err(e) => (STATUS_ERR, format!("invalid utf-8 input: {e}")),
    };
    emit(tag, payload.as_bytes())
}

// ---------------------------------------------------------------------------
// Inner engine glue (host-testable; no raw pointers). Each returns
// `(status_tag, payload)` where payload is result JSON (ok) or the engine's own
// error message (err) — never swallowing the failure (`SPEC.md` §9.1).
// ---------------------------------------------------------------------------

/// Parse `input` as `T`, mapping a malformed-JSON failure to a tagged error
/// payload so callers can `?`-style bail with `(STATUS_ERR, msg)`.
fn parse<T: for<'de> Deserialize<'de>>(label: &str, input: &str) -> Result<T, (u8, String)> {
    serde_json::from_str(input).map_err(|e| (STATUS_ERR, format!("invalid {label}: {e}")))
}

/// Serialize `value` to a `(STATUS_OK, json)` result, or a tagged error.
fn ok_json<T: serde::Serialize>(value: &T) -> (u8, String) {
    match serde_json::to_string(value) {
        Ok(json) => (STATUS_OK, json),
        Err(e) => (STATUS_ERR, e.to_string()),
    }
}

/// JSON envelope for [`project`]: the upstream tool defs + the policy.
#[derive(Deserialize)]
struct ProjectArgs {
    upstream: Vec<ToolDef>,
    policy: Policy,
}

/// JSON envelope for [`rewrite`]: the projected call + the policy.
#[derive(Deserialize)]
struct RewriteArgs {
    call: ToolCall,
    policy: Policy,
}

/// JSON envelope for [`merge`]: the integrator base + the end-user overlay.
#[derive(Deserialize)]
struct MergeArgs {
    base: Policy,
    overlay: Policy,
}

/// JSON envelope for [`validate`]: the policy + the upstream tool defs.
#[derive(Deserialize)]
struct ValidateArgs {
    policy: Policy,
    upstream: Vec<ToolDef>,
}

/// JSON envelope for [`mint`]: the in-code base policy, an optional dynamic
/// narrowing overlay, the run id, and the upstream launch command.
#[derive(Deserialize)]
struct MintArgs {
    run_id: String,
    base: Policy,
    #[serde(default)]
    dynamic: Option<Policy>,
    upstream: UpstreamCommand,
}

/// JSON envelope for [`refire`]: the persisted mint record to re-mint.
#[derive(Deserialize)]
struct RefireArgs {
    record: MintRecord,
}

/// Engine glue for [`project`]: parse `{"upstream","policy"}`, project, emit the
/// projected `[ToolDef...]` (`SPEC.md` §3, §4).
fn project_str(input: &str) -> (u8, String) {
    let args: ProjectArgs = match parse("project args", input) {
        Ok(a) => a,
        Err(e) => return e,
    };
    ok_json(&core_project(&args.upstream, &args.policy))
}

/// Engine glue for [`rewrite`]: parse `{"call","policy"}`, rewrite, emit the
/// upstream `ToolCall`, or a tagged reject message (`SPEC.md` §4, §9).
fn rewrite_str(input: &str) -> (u8, String) {
    let args: RewriteArgs = match parse("rewrite args", input) {
        Ok(a) => a,
        Err(e) => return e,
    };
    match core_rewrite(&args.call, &args.policy) {
        Ok(upstream) => ok_json(&upstream),
        Err(reject) => (STATUS_ERR, reject.to_string()),
    }
}

/// Engine glue for [`merge`]: parse `{"base","overlay"}`, narrow-only merge,
/// emit the composed `Policy`, or a tagged widening error (`SPEC.md` §5.2).
fn merge_str(input: &str) -> (u8, String) {
    let args: MergeArgs = match parse("merge args", input) {
        Ok(a) => a,
        Err(e) => return e,
    };
    match core_merge(&args.base, &args.overlay) {
        Ok(merged) => ok_json(&merged),
        Err(merge_err) => (STATUS_ERR, merge_err.to_string()),
    }
}

/// Engine glue for [`validate`]: parse `{"policy","upstream"}`, validate, emit
/// `null` on success or a tagged drift error (`SPEC.md` §5.3).
fn validate_str(input: &str) -> (u8, String) {
    let args: ValidateArgs = match parse("validate args", input) {
        Ok(a) => a,
        Err(e) => return e,
    };
    match core_validate(&args.policy, &args.upstream) {
        Ok(()) => (STATUS_OK, "null".to_string()),
        Err(drift) => {
            let messages: Vec<String> = drift.into_iter().map(|d| d.message).collect();
            (
                STATUS_ERR,
                format!("policy drift vs upstream: {}", messages.join("; ")),
            )
        }
    }
}

/// Engine glue for [`mint`]: parse `{"run_id","base","dynamic","upstream"}`,
/// narrow `base` by the optional `dynamic` overlay, emit the recorded
/// [`MintRecord`] (resolved policy + provenance), or a tagged widening error
/// (`SPEC.md` §8.2, §5.2).
fn mint_str(input: &str) -> (u8, String) {
    let args: MintArgs = match parse("mint args", input) {
        Ok(a) => a,
        Err(e) => return e,
    };
    match core_mint(args.run_id, &args.base, args.dynamic.as_ref(), args.upstream) {
        Ok(record) => ok_json(&record),
        Err(merge_err) => (STATUS_ERR, merge_err.to_string()),
    }
}

/// Engine glue for [`refire`]: parse `{"record"}`, re-mint the recorded resolved
/// policy, emit the [`ResolvedPolicy`](lagom_core::ResolvedPolicy) (`SPEC.md`
/// §8.2). Pure: no re-resolution, so the exact recorded projection is reproduced.
fn refire_str(input: &str) -> (u8, String) {
    let args: RefireArgs = match parse("refire args", input) {
        Ok(a) => a,
        Err(e) => return e,
    };
    ok_json(&core_refire(&args.record))
}

// ---------------------------------------------------------------------------
// Exported wasm functions: thin ABI wrappers over the `*_str` glue.
// ---------------------------------------------------------------------------

/// Project an upstream tool surface through a policy (`SPEC.md` §3, §4).
///
/// Input JSON: `{"upstream": [ToolDef...], "policy": Policy}`. Result JSON: the
/// projected `[ToolDef...]` array.
///
/// # Safety
///
/// `ptr`/`len` must describe a host-allocated UTF-8 buffer (see the module ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn project(ptr: *const u8, len: u32) -> u64 {
    // SAFETY: forwarded contract from the module ABI; host owns the buffer.
    unsafe { dispatch(ptr, len, project_str) }
}

/// Rewrite a projected call back into the upstream call, or return a tagged
/// error on reject (`SPEC.md` §4, §9).
///
/// Input JSON: `{"call": ToolCall, "policy": Policy}`. Result JSON: the upstream
/// `ToolCall` with pinned/default args injected; a reject surfaces as a tagged
/// error message.
///
/// # Safety
///
/// `ptr`/`len` must describe a host-allocated UTF-8 buffer (see the module ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rewrite(ptr: *const u8, len: u32) -> u64 {
    // SAFETY: forwarded contract from the module ABI; host owns the buffer.
    unsafe { dispatch(ptr, len, rewrite_str) }
}

/// Narrow-only merge of an end-user overlay onto an integrator base
/// (`SPEC.md` §5.2).
///
/// Input JSON: `{"base": Policy, "overlay": Policy}`. Result JSON: the composed
/// `Policy`; any widening surfaces as a tagged error (the sandbox enforcement).
///
/// # Safety
///
/// `ptr`/`len` must describe a host-allocated UTF-8 buffer (see the module ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn merge(ptr: *const u8, len: u32) -> u64 {
    // SAFETY: forwarded contract from the module ABI; host owns the buffer.
    unsafe { dispatch(ptr, len, merge_str) }
}

/// Validate a policy against the live upstream tool surface (`SPEC.md` §5.3).
///
/// Input JSON: `{"policy": Policy, "upstream": [ToolDef...]}`. On success the
/// result JSON is `null`; on drift a tagged error carries every per-reference
/// drift message joined with `; ` — serving must be refused.
///
/// # Safety
///
/// `ptr`/`len` must describe a host-allocated UTF-8 buffer (see the module ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn validate(ptr: *const u8, len: u32) -> u64 {
    // SAFETY: forwarded contract from the module ABI; host owns the buffer.
    unsafe { dispatch(ptr, len, validate_str) }
}

/// Mint an ephemeral projection in code: narrow a base policy by an optional
/// dynamic overlay and record its provenance (`SPEC.md` §8.1, §8.2).
///
/// Input JSON: `{"run_id": str, "base": Policy, "dynamic": Policy|null,
/// "upstream": UpstreamCommand}`. Result JSON: the [`MintRecord`] (resolved
/// policy + provenance) to persist for refire; a widening overlay surfaces as a
/// tagged error (the sandbox enforcement, `SPEC.md` §5.2).
///
/// # Safety
///
/// `ptr`/`len` must describe a host-allocated UTF-8 buffer (see the module ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mint(ptr: *const u8, len: u32) -> u64 {
    // SAFETY: forwarded contract from the module ABI; host owns the buffer.
    unsafe { dispatch(ptr, len, mint_str) }
}

/// Refire a persisted mint record: re-mint the recorded resolved policy
/// (`SPEC.md` §8.2).
///
/// Input JSON: `{"record": MintRecord}`. Result JSON: the
/// [`ResolvedPolicy`](lagom_core::ResolvedPolicy) ready to serve — the exact
/// projection the original run had, reproduced without re-resolution.
///
/// # Safety
///
/// `ptr`/`len` must describe a host-allocated UTF-8 buffer (see the module ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn refire(ptr: *const u8, len: u32) -> u64 {
    // SAFETY: forwarded contract from the module ABI; host owns the buffer.
    unsafe { dispatch(ptr, len, refire_str) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upstream() -> serde_json::Value {
        json!([
            {
                "name": "search",
                "description": "Search.",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "artifact": {"type": "string"},
                        "query": {"type": "string"}
                    },
                    "required": ["artifact", "query"]
                }
            },
            {
                "name": "write_file",
                "description": "Write.",
                "input_schema": {"type": "object", "properties": {}}
            }
        ])
    }

    /// A sealed policy keeping `search` and pinning `artifact` drops `write_file`
    /// and prunes the pinned property.
    #[test]
    fn project_drops_tool_and_pins_arg() {
        let policy = json!({
            "default_presence": "drop",
            "tools": {
                "search": {"presence": "keep", "args": {"artifact": {"pin": "hylla"}}}
            }
        });
        let input = json!({"upstream": upstream(), "policy": policy}).to_string();
        let (tag, payload) = project_str(&input);
        assert_eq!(tag, STATUS_OK, "payload: {payload}");

        let defs: Vec<ToolDef> = serde_json::from_str(&payload).unwrap();
        assert_eq!(defs.len(), 1, "write_file must be dropped");
        assert_eq!(defs[0].name, "search");
        let props = &defs[0].input_schema["properties"];
        assert!(props.get("artifact").is_none(), "pinned arg must be pruned");
        assert!(props.get("query").is_some());
    }

    /// Rewrite injects the pin; a constraint violation comes back tagged-error.
    #[test]
    fn rewrite_injects_pin_and_rejects_constraint() {
        let policy = json!({
            "tools": {
                "search": {
                    "args": {
                        "artifact": {"pin": "hylla"},
                        "query": {"constrain": {"enum": ["a", "b"]}}
                    }
                }
            }
        });
        let ok_in =
            json!({"call": {"name": "search", "arguments": {"query": "a"}}, "policy": policy})
                .to_string();
        let (tag, payload) = rewrite_str(&ok_in);
        assert_eq!(tag, STATUS_OK, "payload: {payload}");
        let call: ToolCall = serde_json::from_str(&payload).unwrap();
        assert_eq!(call.arguments["artifact"], json!("hylla"));

        let bad_in =
            json!({"call": {"name": "search", "arguments": {"query": "z"}}, "policy": policy})
                .to_string();
        let (tag, _) = rewrite_str(&bad_in);
        assert_eq!(tag, STATUS_ERR, "out-of-enum query must reject");
    }

    /// A widening overlay is rejected by the narrow-only merge (tagged error).
    #[test]
    fn merge_rejects_widening() {
        let base = json!({"default_presence": "drop"});
        let overlay = json!({"tools": {"search": {"presence": "keep"}}});
        let input = json!({"base": base, "overlay": overlay}).to_string();
        let (tag, _) = merge_str(&input);
        assert_eq!(tag, STATUS_ERR, "re-adding a dropped tool is widening");
    }

    /// Validate fails loud (tagged error) on a vanished tool, ok (`null`) clean.
    #[test]
    fn validate_flags_drift_and_passes_clean() {
        let drifted = json!({"tools": {"ghost": {"args": {"x": {"pin": 1}}}}});
        let input = json!({"policy": drifted, "upstream": upstream()}).to_string();
        let (tag, _) = validate_str(&input);
        assert_eq!(tag, STATUS_ERR, "ghost tool is drift");

        let clean = json!({"tools": {"search": {"args": {"artifact": {"pin": "x"}}}}});
        let input = json!({"policy": clean, "upstream": upstream()}).to_string();
        let (tag, payload) = validate_str(&input);
        assert_eq!(tag, STATUS_OK, "payload: {payload}");
        assert_eq!(payload, "null");
    }

    /// Malformed JSON surfaces as a tagged error, never a panic.
    #[test]
    fn malformed_json_is_tagged_error() {
        let (tag, payload) = project_str("not json");
        assert_eq!(tag, STATUS_ERR);
        assert!(payload.contains("invalid project args"), "msg: {payload}");
    }

    /// `mint` narrows the base by the dynamic overlay and `refire` reproduces the
    /// recorded resolved policy byte-for-byte — the ephemeral mint/refire loop
    /// over the wasm ABI (`SPEC.md` §8.2).
    #[test]
    fn mint_then_refire_round_trips() {
        let mint_in = json!({
            "run_id": "agent-7",
            "base": {"default_presence": "keep", "tools": {"search": {"presence": "keep"}}},
            "dynamic": {"default_presence": "keep", "tools": {"search": {"presence": "drop"}}},
            "upstream": {"command": "srv", "args": ["-y"], "env": []}
        })
        .to_string();
        let (tag, payload) = mint_str(&mint_in);
        assert_eq!(tag, STATUS_OK, "payload: {payload}");
        let record: MintRecord = serde_json::from_str(&payload).unwrap();
        assert_eq!(record.run_id, "agent-7");
        assert_eq!(
            record.resolved.policy.tools["search"].presence,
            Some(lagom_core::Presence::Drop),
            "dynamic overlay must drop search"
        );

        let refire_in = json!({"record": record}).to_string();
        let (tag, refired) = refire_str(&refire_in);
        assert_eq!(tag, STATUS_OK, "payload: {refired}");
        assert_eq!(
            refired,
            serde_json::to_string(&record.resolved).unwrap(),
            "refire reproduces the recorded resolved policy byte-for-byte"
        );
    }

    /// A widening dynamic overlay is rejected by `mint` (tagged error) — the
    /// sandbox enforcement, even on the in-code mint path (`SPEC.md` §5.2).
    #[test]
    fn mint_rejects_widening_overlay() {
        let mint_in = json!({
            "run_id": "r",
            "base": {"default_presence": "keep", "tools": {"search": {"presence": "drop"}}},
            "dynamic": {"default_presence": "keep", "tools": {"search": {"presence": "keep"}}},
            "upstream": {"command": "srv"}
        })
        .to_string();
        let (tag, _) = mint_str(&mint_in);
        assert_eq!(tag, STATUS_ERR, "re-keeping a dropped tool is widening");
    }

    /// `alloc`/`dealloc` round-trips a buffer without corrupting memory: write a
    /// pattern, read it back, free it. (Pointer *packing* is wasm32-only and is
    /// covered end-to-end by the Go/wazero test; here we only prove the
    /// allocator pair is sound on the host triple.)
    #[test]
    fn alloc_dealloc_roundtrips() {
        let n = 37u32;
        let ptr = alloc(n);
        assert!(!ptr.is_null());
        // SAFETY: alloc reserved `n` bytes at `ptr`.
        let slice = unsafe { std::slice::from_raw_parts_mut(ptr, n as usize) };
        for (i, b) in slice.iter_mut().enumerate() {
            *b = i as u8;
        }
        for (i, b) in slice.iter().enumerate() {
            assert_eq!(*b, i as u8);
        }
        // SAFETY: ptr/n is the exact allocation from `alloc`.
        unsafe { dealloc(ptr, n) };
    }
}
