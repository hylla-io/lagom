# lagom — shipped feature inventory (0.1.0)

Every feature lagom 0.1.0 ships — what it does and **why** — cross-checked
against `SPEC.md` §§3-13. The "DEFERRED" section lists spec capability
explicitly out of the first slice; "KNOWN GAPS / OPEN QA FINDINGS" lists gaps
the QA review surfaced against the shipped surface.

---

## 1. The `Policy` engine — `lagom-core` (SPEC §3, §4, §5)

The transport-less, side-effect-free transform engine. One canonical model, four
operations; compiles to native and (by construction) wasm. Done and green.

- **`Policy` model** (SPEC §3) — the projection spec as data: per-tool presence
  (keep/drop), rename, description policy; per-arg pin / constrain / default /
  passthrough. *Why:* a single model both front-ends produce and the only thing
  the engine consumes, so the typed builder and `lagom.toml` cannot drift.
- **`project(upstream_defs, policy) -> projected_defs`** (SPEC §4.1) — forward
  transform on the `inputSchema`: pin deletes the property from `properties` +
  `required`; constrain-set sets `enum`; range sets `minimum`/`maximum`; pattern
  sets `pattern`; drop omits the tool. *Why:* the transforms are lossless-by-
  construction and structurally *prevent* invalid calls — the agent cannot form
  a call it cannot make, killing the call→fail→retry loop.
- **`rewrite(projected_call, policy) -> upstream_call | Reject`** — inverse
  transform: injects pinned values the agent never saw, supplies defaults, and
  rejects constraint violations. *Why:* the upstream always receives a complete,
  valid call (SPEC §5.1 call-contract guarantee).
- **`merge(base, overlay) -> Policy`** — **narrow-only** composition (SPEC §5.2).
  The overlay may only drop / tighten / pin; any widening (re-add a dropped
  tool, loosen a constraint, unpin) is a load-time `MergeError`. *Why:* this
  merge **is** the sandbox — it makes integrator-authored sealed bounds a
  guarantee, not a suggestion.
- **`validate(policy, upstream_defs) -> Ok | DriftError`** (SPEC §5.3) — checks
  every policy reference against the live upstream surface. *Why:* a vanished
  tool/arg fails loud rather than silently forwarding a broken call.
- **Determinism** (SPEC §5.4) — minting invokes no LLM and no nondeterministic
  input. *Why:* the precondition for byte-identical refire (§8.2).

## 2. Description transforms (SPEC §4.2)

- **Tier 1 — integrator override**: explicit slim text via `lagom.toml`
  `description` (or the builder). *Why:* preferred, fully authored, deterministic.
- **Tier 2 — addendum fallback**: original text + a deterministic note from the
  schema diff. *Why:* when no override is supplied, the agent still learns the
  restrictions without lagom ever contradicting the schema.
- **Tier 0 — shipped skill, not a runtime LLM call** (delivered via the
  `lagom-slim-docs` skill, §12). *Why:* lagom never calls a model itself in
  0.1.0; doc-slimming is host-driven and committed as a Tier-1 override.

## 3. `lagom.toml` config face — `lagom-config` (SPEC §6)

- **Flat human surface** (`TomlPolicy` → core `Policy`): default presence,
  per-tool presence/rename/description, per-arg pin/default/enum/min/max/pattern,
  with mutually-exclusive arg transforms enforced at lowering. *Why:* one model,
  two front-ends (§6.1) that cannot drift; the TOML carries only the flat surface
  while rich transforms stay in the builder (§6.2).
- **JSON Schema for `lagom.toml`** (`json_schema`) — for editor validation
  (§6.2). *Why:* authors get inline feedback before runtime.
- **Layered discovery by precedence** (`discover` / `load_discovered`, §6.3):
  explicit `--config` → `./lagom.toml` → project-root `lagom.toml` →
  `$XDG_CONFIG_HOME/lagom/lagom.toml`, composed via the narrow-only merge.
  *Why:* end-user overlay narrows the integrator base within sealed bounds.
- **Multi-profile** (`.lagom/<name>.toml` via `profiles`/`resolve_profile`,
  §6.4). *Why:* apps that ship many named projections.
- **Never edits harness config** — config loading only reads. *Why:* §6.5
  harness-agnostic; lagom stays a paste-in drop-in.

## 4. CLI face — `lagom-cli` (SPEC §7.1, §6.5, §12)

- **`lagom serve --config <f> [--audit <path>] [--run-id <id>] -- <upstream…>`**
  — stdio proxy: spawn the upstream child, drift-validate, serve the projection
  until the session ends. With `--audit` the session routes through
  `lagom_proxy::serve_audited`, opening an append-only JSONL log that records the
  original upstream defs + resolved policy at session start and every
  rewrite/rejection thereafter, tagged with `--run-id` (default `run`). Without
  the flag, behavior is unchanged (no audit log). *Why:* harness-agnostic
  drop-in; `--audit` makes the §8.2 refire/traceability trace actually persist
  from a shipped face.
- **`lagom emit --config <f> --name <k> -- <upstream…>`** — prints the harness
  stdio-server snippet (`mcpServers` block running `lagom serve`). *Why:* §6.5 —
  lagom never edits `.mcp.json`/`settings.json`; it prints the exact entry to
  paste, self-contained and reproducible.
- **`lagom validate --config <f> -- <upstream…>`** — spawns the upstream, fetches
  its live `tools/list`, checks every policy reference; non-zero exit on drift.
  *Why:* §5.3 fail-loud before serving.
- **`lagom emit-skills [dir]`** — writes the shipped skill markdown into a chosen
  directory. *Why:* §12 — delivers the doc-slimming/dynamic-mint guidance
  harness-agnostically (this supersedes the spec's `describe --suggest`; see
  KNOWN GAPS).
- **Non-panicking exit codes** — each subcommand returns an `ExitCode` so drift
  exits non-zero cleanly. *Why:* scriptable, fail-loud (§5.3, §9.1).

## 5. stdio proxy + minting — `lagom-proxy` (SPEC §7.1, §8, §10)

- **stdio JSON-RPC bridge** — spawns the upstream as a child, pumps downstream
  ↔ upstream, applying `project` to `tools/list` responses and `rewrite` to
  `tools/call` requests; rejections annotated back to the agent. *Why:* §10
  first slice is stdio-only, spawn-the-upstream, with crashes loud.
- **Surface-uniform passthrough** (§10) — resources, prompts, `initialize`,
  notifications, pagination forwarded unchanged. *Why:* 0.1.0 narrows tools
  only; the transform model is surface-uniform so the rest passes through.
- **Drift-probe at startup** (`spawn_and_validate` / `probe_tools`, §5.3) —
  fetches the live `tools/list` and validates before serving. *Why:* refuse to
  serve a drifted policy.
- **Minting + refire machinery** (`mint`, `MintRecord`, `ResolvedPolicy`,
  `serve_audited`, `refire`, §8.2) — resolve a stack of policy sources into one
  resolved policy, record provenance + resolved policy, re-mint an identical
  server from a record. *Why:* ephemeral per-agent projections must be
  reproducible (refire from failure) and traceable. **NOTE:** present as library
  API only — no shipped face wires it. See KNOWN GAPS.
- **`UpstreamCommand`** — carries the upstream launch command/args/env from
  config (§10). *Why:* lagom owns spawning the child.

## 6. Audit log — `lagom-audit` (SPEC §9.3)

- **Append-only `AuditEvent` log** with `Rewrite`, `Rejection`, `OriginalDefs`,
  and `ResolvedPolicy` variants, JSONL round-trip tested. *Why:* §9.3 source of
  refire + traceability; §9.1 never-swallow (every rewrite/rejection recorded
  and annotated). **NOTE:** only `Rewrite`/`Rejection` are written in any
  production path; `OriginalDefs`/`ResolvedPolicy` are never produced live, and
  no shipped face opens an audit log. See KNOWN GAPS.

## 7. Python binding — `lagom-py` (SPEC §2, §7.2)

- **`project` / `rewrite` / `merge` / `validate`** over JSON strings across the
  FFI boundary. *Why:* the compiled core as a normal Python dependency
  (PyO3/maturin abi3 wheel) — no separate binary; the "≥1 binding proving the
  embed model" the first slice requires (§10).
- **`mint_stdio_server`** — the spawned-process face for agents that drive a
  child upstream over stdio. *Why:* §7.2 spawned-agent face. (By design depends
  on `lagom-proxy`/tokio; the wasm-purity invariant applies only to the future
  wasm face, §7.3.) Takes optional `audit=<path>` / `run_id=<id>` keyword args
  that route the session through `lagom_proxy::serve_audited`, mirroring the
  CLI's `--audit`/`--run-id`: an append-only JSONL trace recording the original
  upstream defs + resolved policy + every rewrite/rejection (§8.2, §9.3).
  Without `audit`, behavior is unchanged.
- **Typed `PolicyBuilder`** (incl. `sealed()`) — author a `Policy` type-safely
  from Python. *Why:* §6.1 integrator front-end in the binding language.
- **`shipped_skills()` / `emit_skills(dir)`** — the identical embedded skill
  bytes the CLI writes. *Why:* §12 every binding bundles the skills.
- **Errors → Python `ValueError`** carrying the engine's own message. *Why:*
  §9.1 never swallow.

## 8. Shipped skills — `lagom-proxy::skills` (SPEC §12)

- **`lagom-slim-docs`** — instructs a host to use a *small* model, once at
  authoring time, to rewrite descriptions in **caveman** style (grammar-free,
  content-only; pinned/removed args appear nowhere) and commit as Tier-1
  overrides. *Why:* §4.2 Tier 0 doc-slimming without lagom calling a model.
- **`lagom-dynamic-mint`** — instructs a host to mint an **ephemeral** per-agent
  projection scoped to that agent's lifecycle and tear it down at end of life.
  *Why:* §8 mint-time dynamism / ephemeral projections.
- **Embedded once via `include_str!`**, re-exported so the CLI and every binding
  bundle identical bytes. *Why:* §12 one canonical source; skills are inert
  until a host loads them (lagom never runs an agent or model in 0.1.0).

## 9. Go binding via wasm — `lagom-wasm` + `lagom-go` (SPEC §2, §7.3, ADR-0001)

The fourth face: the transport-less core delivered to Go consumers as a normal
`go get`-able module, with the engine compiled to wasm and run in-process.

- **`lagom-wasm` — the wasm32 face of `lagom-core`** (`crates/lagom-wasm`,
  excluded from the workspace like `lagom-py`; built via `just wasm`). Compiles
  `project`/`rewrite`/`merge`/`validate` to `wasm32-unknown-unknown` behind a
  **JSON-in / JSON-out memory ABI**: exports `alloc`/`dealloc` plus one export
  per op taking `(ptr, len)` and returning a packed `(ptr << 32) | len`, with the
  result buffer prefixed by a **status tag** so engine rejects / drift surface as
  a tagged error rather than being swallowed (§9.1). *Why:* §7.3 — the pure
  compute core runs anywhere wasm runs; the tag upholds the never-swallow
  contract across the FFI boundary. ABI/engine glue is unit-tested on the host
  triple (`crates/lagom-wasm/src/lib.rs`, incl. `malformed_json_is_tagged_error`,
  `alloc_dealloc_roundtrips`).
- **`lagom-go` — the in-process Go module** (`go/`, module path
  `github.com/hylla-io/lagom/go`). `go:embed`s the built `lagom.wasm`
  (`go/internal/wasmbin/`) and instantiates it via **`wazero`** (pure Go, no
  cgo). *Why:* ADR-0001 named Go the binding problem-child — native embedding
  needs cgo + a C toolchain + per-platform archives, which breaks `go get`. Wasm
  + wazero sidesteps all of it: consumers get the whole `project`/`rewrite`/
  `merge`/`validate` engine from **one `go get`** — no separate binary, no C
  toolchain, nothing read from disk at runtime (the engine is embedded in the
  Go binary). `wazero` is the **only** non-stdlib dependency (`go/go.mod`).
- **In-process `project`/`rewrite`/`merge`/`validate`** — driven through the
  embedded wasm engine with no subprocess. *Why:* §2 — the binding exposes the
  transport-less face for in-process agents; spawning/stdio stay native (§7.3).
- **Proven by `go/lagom_test.go`** (`just go-test`): `project()` drops a tool and
  pins+hides an arg, `rewrite()` injects the pin and rejects an out-of-enum call
  as a tagged error, `merge()` rejects widening, `validate()` flags drift, and
  malformed input surfaces as a tagged error. The suite runs under
  `CGO_ENABLED=0` for both `go vet` and `go test`, so the `go get`-clean
  (no-cgo) guarantee is **enforced by the test path itself** — any cgo creep
  would fail the build. *Why:* the security-relevant Go face is exercised end to
  end against the same engine the Rust/Python faces use.

## 10. Build & CI tooling (ADR-0002)

- **`just ci` gate** — fmt-check + clippy `-D warnings` + test + build over the
  workspace; mirrored in `.github/workflows/ci.yml`. *Why:* language-agnostic
  runner over cargo verbs; stable gate name across the workspace.
- **`just py` / `py-check` / `py-test`** — separate recipes for the excluded
  pyo3 cdylib. *Why:* linking the extension-module needs a Python interpreter
  maturin supplies; keeping it out of the default workspace build keeps `just ci`
  green on machines without one. CI runs them in a dedicated `python` job.
- **`just wasm` / `just go-test`** — build the wasm32 face and refresh the
  committed `go/internal/wasmbin/lagom.wasm`, then run the Go binding's in-process
  wazero tests under `CGO_ENABLED=0`. *Why:* `lagom-wasm` is excluded from the
  workspace (its memory ABI is only meaningful on wasm32) and the host's rustc is
  Homebrew while the wasm std lives under rustup, so `just wasm` drives the rustup
  `stable` toolchain explicitly; `just ci` stays green with `lagom-wasm` excluded,
  as `lagom-py` is.

---

## DEFERRED (not in 0.1.0)

Spec capability explicitly out of the first slice (`SPEC.md` §10, §11, §8.3):

- **Resource / prompt narrowing** (§11) — 0.1.0 narrows tools only; resources,
  prompts, `initialize`, notifications, and pagination pass through unchanged.
- **HTTP / SSE / streamable-HTTP transport; attach-to-running upstream** (§11) —
  stdio transport only; lagom always spawns the upstream child.
- **Per-call authorization hooks** (`authorize(call) -> allow|deny|rewrite`,
  §8.3, §11) — documented in-process-only seam; kept out of the core to keep it
  wasm-clean.
- **Restart / supervision policy for the upstream child** (§11) — 0.1.0 has
  minimal lifecycle only: spawn on init, teardown on exit, crashes loud.
- **Additional bindings** (Node addon via napi-rs, Rust crate published) — the
  packaging matrix beyond Python and Go is a 0.1.x question (§13).
- **Connecting lagom directly to a model** (§4.2, §12) — explicitly deferred
  post-0.1.0; doc-slimming stays host-driven via the shipped skill.

---

## KNOWN GAPS / OPEN QA FINDINGS

Derived from the QA review against the shipped 0.1.0 surface. Ordered by
severity. These are the items that are **still genuinely open** (the slice-1 and
slice-2 resolved findings — the two High sandbox escapes, the audit wiring, the
real-upstream handshake, the BufReader read-ahead, py-in-CI, validate
type-coherence, the `oneOf` range fix, and `describe --suggest` supersession —
are folded into the feature sections above; the Go/wasm binding moved out of
DEFERRED into §9). Resolved findings are kept inline below marked `[RESOLVED]`
for traceability.

### Medium — sandbox / interception completeness

- **JSON-RPC batch (top-level array) bypasses the `tools/call` rewrite.**
  `pump_downstream` (`crates/lagom-proxy/src/bridge.rs`) parses each line as one
  `serde_json::Value` and routes on `method_of()`, which reads `msg.get("method")`.
  A JSON-RPC 2.0 batch is a top-level **array** of request objects — it has no
  `"method"` key, so `method_of()` returns `None` and the message falls into the
  `_` passthrough arm, written to the upstream child **unchanged**. A batched
  `tools/call` is therefore never run through `lagom_core::rewrite` (pins not
  injected, constraints/enums/patterns not enforced, dropped/renamed tools not
  blocked), and a batched `tools/list` is never projected (no id recorded → the
  response leaks the full upstream surface). Severity is **medium** not high
  because (a) MCP removed JSON-RPC batching in the `2025-06-18` spec and the
  bridge targets `2025-11-25` (single newline-delimited messages, confirmed via
  Context7), so a spec-conformant harness never sends batches; and (b) lagom is
  stdio-only with a single trusted-ish harness as the client. But lagom's value
  proposition is sandboxing a possibly-adversarial agent, and nothing
  structurally stops a non-conformant or malicious downstream from sending
  `[{...,"method":"tools/call",...}]` to slip a call past the projection. No test
  exercises a batch. *Fix direction:* detect a top-level `Value::Array` and
  either **reject** it (cleanest for `2025-11-25`, which forbids batching) or
  iterate and apply the same per-element interception. **Touches `lagom-proxy`.**

### Low — CI / QA-gate coverage

- **wasm/Go face (`lagom-wasm`, `go/`) is not wired into any CI job.**
  `.github/workflows/ci.yml` has only `check` (`just ci`) and `python` jobs; no
  job runs `just wasm` or `just go-test`. Two facets:
  - `lagom-wasm`'s host-triple unit tests (the `*_str` engine-glue + never-swallow
    tagging tests, incl. `malformed_json_is_tagged_error` and
    `alloc_dealloc_roundtrips`) are excluded from `just ci` (excluded workspace
    member). They pass out-of-band (6 passed). The wasm glue is the
    security-relevant boundary for the Go face (the malformed-JSON tagging that
    upholds §9.1 never-swallow), yet a regression in `parse()`/`dispatch()`/
    `emit()` would pass the shipped gate. This mirrors the resolved
    `lagom-py`-excluded-from-CI finding (fixed with a separate `python` job); the
    same treatment is warranted for the wasm/Go face.
  - `go/lagom_test.go` runs against the **committed** `go/internal/wasmbin/lagom.wasm`
    (1.1M), so `just go-test` passes locally with no rust+wasm toolchain
    (`CGO_ENABLED=0`, verified green). The live risk is **stale-wasm-blob drift**:
    a `lagom-core` change that should alter wasm behavior is not caught by CI
    unless someone re-runs `just wasm` + `just go-test` and re-commits the blob.
    A `go-test` CI job using the committed blob would at least guard the Go glue;
    a `wasm` build + diff step would guard the blob's freshness.

- **No shipped face persists a full `MintRecord` (provenance) for refire.** The
  audit-wiring fix (below, `[RESOLVED]` for the CLI/Python faces) writes
  `OriginalDefs` + `ResolvedPolicy` — the refire *basis* — but `mint_record()`
  (which captures `PolicySources` provenance: `config_paths` + `dynamic_inputs`)
  and `refire(record)` remain **library-only with zero non-test callers**. So the
  on-disk trace lets you reconstruct the *resolved* policy but not the
  *provenance* (base profile + overlays + dynamic inputs) the §8.2 trace
  specifies, and there is no shipped "refire from a recorded run" command. This is
  still-partial against §8.2 provenance. **Touches `lagom-proxy`/`lagom-cli`.**

- **No `lagom.toml` audit key.** Auditing is reachable only via `lagom serve
  --audit`/`--run-id` and the Python `mint_stdio_server` `audit=`/`run_id=`
  kwargs; the JSON schema and `TomlPolicy` carry no audit/run-id surface. This is
  an accepted design choice (the flag/kwarg was the cleaner surface), not a
  regression — listed for completeness as the lone sub-item under the otherwise
  resolved shipped-audit-face gap.

- **Audit-write failure is logged-and-continued (accepted, not an escape).**
  `record()` (`crates/lagom-proxy/src/bridge.rs`) swallows an `AuditLog::record()`
  error to stderr and continues the bridge. This is **correct** for the security
  posture: the audit log is observability/traceability (§9.3), not the
  enforcement path — rewrite/reject decisions are already made before `record()`
  runs, so a log-write failure cannot widen the sandbox. The one residual: with
  `--audit` enabled, a silently-failing log undermines the §8.2 refire/trace
  guarantee (a run can appear complete while its on-disk trace is partial). That
  is a completeness/observability gap, not a sandbox escape. No action required
  for sandbox integrity.

- **Constraint rejection rides as a JSON-RPC error object, not an `isError` tool
  result.** An out-of-enum `tools/call` is returned as a JSON-RPC error (code
  `-32001`, message annotated per §9.1) rather than a `tools/call` result with
  `isError=true`. This is fully annotated and never-swallowed, so it is
  **correct**, not a defect. Observation only: some MCP clients surface tool-level
  failures more naturally via an `isError` result content block; if interop with a
  strict client ever shows the rejection is not displayed to the agent, consider
  whether constraint rejections should ride as `isError` tool results. No action
  required for 0.1.0 — the annotation reaches the driver intact.

- **`cmd_validate` routes a production path through proxy `test_support`.**
  `lagom-proxy::test_support` is documented as "not part of the stable API —
  exposed only so the bridge round-trip tests can inject a duplex harness", yet
  the shipped `lagom validate` subcommand calls
  `lagom_proxy::test_support::spawn_and_validate`
  (`crates/lagom-cli/src/app.rs`). Functionally correct and green, but the module
  doc now understates its use (a non-test, shipped caller depends on it).
  *Fix direction:* promote `spawn_and_validate` to a non-`test_support` `pub fn`,
  or amend the module doc. Doc-accuracy only. **Touches `lagom-proxy`.**

- **`bridge.rs` ballooned to 635 LOC (was 509), nearing the god-module
  watch-point.** Slice 2 added +126 LOC — all cohesive transport-lifecycle work
  (the MCP `initialize`/`notifications/initialized` handshake in `probe` /
  `read_probe_response`, the shared-`BufReader` read-ahead fix in
  `spawn_and_validate`, and the `OriginalDefs`/`ResolvedPolicy` audit recording in
  `Server::run_with`). It is **not** scope creep into a new responsibility (it
  stays within the single transport adapter, ADR-0001) and remains cohesive, so
  acceptable for 0.1.0. But the file now spans three concern clusters across 18
  declarations: (1) the JSON-RPC pump (`pump_downstream`/`pump_upstream`/
  `handle_tools_call`), (2) the spawn/probe/handshake lifecycle
  (`spawn_and_validate`/`spawn_child`/`probe`/`read_probe_response`), and (3)
  wire/projection helpers (`project_list_response`/`wire_to_tooldef`/
  `tooldef_to_wire`/`reject_result`). The watch-point is closer to tripping;
  execute the split (pump module vs spawn/probe-lifecycle module) before or
  alongside the deferred HTTP/SSE transport (§11) so `bridge.rs` does not become a
  god module.

### Resolved (folded into the feature sections; kept for traceability)

- **[RESOLVED — §1] `merge()` let an overlay pin escape a base `Pattern`
  constraint.** `value_satisfies()` returned `true` unconditionally for
  `Constraint::Pattern`, so a base regex (e.g. `path ^src/`) did not stop a
  lower-authority overlay pinning `path="/etc/passwd"`, widening the integrator's
  sealed ceiling (§5.2). *Fixed:* a pin tightening a base `Pattern` must itself
  match the base regex (`crates/lagom-core/src/merge.rs` `value_satisfies` →
  `Constraint::Pattern` now compiles the regex and tests the pinned literal);
  a non-matching pin is rejected as a `MergeError`.
- **[RESOLVED — §1] Rename target could collide with another kept tool.**
  Renaming `danger`→`safe` (with a real `safe` kept) silently routed `safe` calls
  into the dangerous upstream tool. *Fixed:* `validate()`
  (`crates/lagom-core/src/validate.rs`) now rejects any rename target that equals
  another surviving projected name (`projected name … collides`), covered by
  `rename_onto_another_kept_tool_is_collision`.
- **[RESOLVED — §6] Audit log never recorded `OriginalDefs`/`ResolvedPolicy` in
  any production path.** *Fixed:* `Server::run_with` records `AuditEvent::OriginalDefs`
  (probed upstream surface) + `AuditEvent::ResolvedPolicy` once at session start,
  ahead of the `Rewrite`/`Rejection` stream. Covered by
  `handshake_runs_and_audit_records_original_defs_and_resolved_policy`.
- **[RESOLVED — §4/§7] No shipped face opened an audit log.** *Fixed:* `lagom
  serve --audit <path> [--run-id <id>]` and the Python `mint_stdio_server`
  `audit`/`run_id` kwargs both route through `lagom_proxy::serve_audited`, writing
  an append-only JSONL trace (`OriginalDefs` + `ResolvedPolicy` + every
  `Rewrite`/`Rejection`). Covered end-to-end by
  `serve_with_audit_writes_trace_after_roundtrip` (`crates/lagom-cli/tests/cli.rs`).
- **[RESOLVED — §5] Drift probe issued `tools/list` before the MCP `initialize`
  handshake.** *Fixed:* `probe` performs the full MCP lifecycle (initialize +
  await response + `notifications/initialized`) before probing `tools/list` per
  `2025-11-25` (Context7-verified); the fake fixture is now strict, so a
  successful round-trip proves the handshake ran.
- **[RESOLVED — §5] Drift-probe `BufReader` read-ahead could discard upstream
  bytes.** *Fixed:* `spawn_and_validate` takes the child's stdout once into a
  `BufReader<ChildStdout>` and threads that same reader onto `Server` and
  `pump_upstream`; the probe reads via `read_line` (not a consuming `Lines`
  iterator) to keep the buffer intact.
- **[RESOLVED — §10] Python binding excluded from the CI gate.** *Fixed:*
  `.github/workflows/ci.yml` gains a `python` job (uv + maturin) running
  `just py-check` + `just py-test` + `just py`, so a pyo3 regression fails CI.
- **[RESOLVED — §1] `validate()` did not check pin/constraint value coherence
  against upstream arg types.** *Fixed:* `validate()` checks every literal it
  supplies (`Pin`/`Default` value, each `Constrain(Enum)` member) against the
  upstream property's JSON-Schema `type` (and `enum` domain when present), with
  `integer` accepting an integral float; each incoherence is a `DriftError`.
  Covered by `pin_with_wrong_type_is_drift` et al.
- **[RESOLVED — §3] `lagom.toml` JSON schema `oneOf` rejected a two-sided min/max
  range.** *Fixed:* the `min`/`max` branches collapse into one range branch
  (`anyOf: [required min, required max]`), so a one- or two-sided range matches
  exactly one `oneOf` branch. Covered by `two_sided_range_matches_exactly_one_branch`
  et al.
- **[RESOLVED — §4] `lagom describe --suggest` not implemented.** Intentionally
  superseded by the `lagom-slim-docs` skill + `emit-skills` (§12). *Fixed:*
  `SPEC.md` §7.1 lists `emit-skills` as shipped and marks `describe --suggest` as
  superseded (not shipped).
