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
- **Ephemeral mint / refire data + ops** (`MintRecord`, `PolicySources`,
  `ResolvedPolicy`, `UpstreamCommand`, `mint`, `refire`, SPEC §8.2) — the
  transport-less, serde-only record types plus a pure in-code `mint` (narrow a
  base policy by an optional per-agent dynamic overlay, recording provenance) and
  `refire` (re-mint the recorded resolved policy). *Why:* an ephemeral per-agent
  projection must be reproducible + traceable, and these types must compile to
  wasm so every binding (including Go-via-wasm) mints/refires over the same
  engine; the file-loading `mint` (resolving `config_paths` off disk) stays native
  in `lagom-proxy`, which re-exports these.
- **`Guard` — the brandable one-call helper** (`lagom_core::Guard`, SPEC §2,
  §7.2) — pairs the upstream surface with a frozen `Policy` so an integrator
  wires a slim, branded MCP in two calls: `slim_defs()` returns the projected
  downstream `tools/list` (computed once at construction) and `gate(call)`
  rewrites each incoming `tools/call` back to upstream (or rejects it), without
  the app touching the `project`/`rewrite` plumbing. *Why:* the first consumer
  (sand) needs lagom-as-a-lib to be stupid-easy and **invisible** — all branding
  (renamed tool names, override descriptions, dropped tools) lives in the
  policy the app supplies; the helper reads nothing itself. Mirrored byte-for-
  byte in every binding (NO DRIFT): `lagom.Guard` (Python), `new Guard(...)`
  (Node), `lagom.NewGuard(...)` (Go). Covered by `guard::tests` in core and a
  parity test in each binding.

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
  `lagom_proxy::serve_audited`, opening an append-only JSONL log. The CLI first
  persists a full `AuditEvent::Mint` record (resolved policy + provenance: the
  discovered/explicit config paths, dynamic inputs, and upstream command) as the
  trace head, then the bridge records the original upstream defs + resolved policy
  at session start and every rewrite/rejection thereafter, all tagged with
  `--run-id` (default `run`). Without the flag, behavior is unchanged (no audit
  log). *Why:* harness-agnostic drop-in; `--audit` makes the §8.2
  refire/traceability trace — including the full provenance — actually persist
  from a shipped face.
- **`lagom refire --record <path> [--audit <path>] [--run-id <id>] [-- <upstream…>]`**
  — re-mint an identical server from a persisted `MintRecord` (§8.2). Reads the
  record (a bare `MintRecord` JSON object, or a JSONL audit trace whose first
  `mint` event carries one), refires the **recorded resolved policy** (bypassing
  re-resolution, so the exact projection is reproduced even after the source
  config drifted), then runs the stdio proxy. Trailing `--` args override the
  recorded upstream launch command; `--audit`/`--run-id` append a fresh refirable
  trace, defaulting the run id to the record's own. *Why:* §8.2 — a recorded run
  (or a failure) is reproducible from a shipped face, not just a library call.
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
- **Minting + refire machinery** (file-loading `mint`, `mint_record`,
  `serve_audited`, §8.2) — resolve a stack of `lagom.toml` policy sources into one
  resolved policy and record its provenance. The pure record types (`MintRecord`,
  `PolicySources`, `ResolvedPolicy`, `UpstreamCommand`) and the pure `mint` /
  `refire` live in `lagom-core` (transport-less, wasm-safe) and are re-exported
  here; only the file-loading `mint` (resolving `config_paths` off disk) is
  proxy-native. *Why:* ephemeral per-agent projections must be reproducible
  (refire from failure) and traceable, and the record types must compile to wasm
  so every binding can mint/refire. **Now shipped end to end:** `lagom serve
  --audit` persists a full `Mint` record and `lagom refire --record` re-mints it
  (§4); every binding exposes pure mint/refire (§7, §9).
- **`UpstreamCommand`** — carries the upstream launch command/args/env from
  config (§10). *Why:* lagom owns spawning the child.

## 6. Audit log — `lagom-audit` (SPEC §9.3)

- **Append-only `AuditEvent` log** with `Mint`, `Rewrite`, `Rejection`,
  `OriginalDefs`, and `ResolvedPolicy` variants, JSONL round-trip tested. *Why:*
  §9.3 source of refire + traceability; §9.1 never-swallow (every
  rewrite/rejection recorded and annotated). The `Mint` variant carries the full
  `lagom_core::MintRecord` (resolved policy + provenance), persisted by `lagom
  serve --audit` as the trace head so a run is refirable from its own log (§8.2).
  All five variants are written by the shipped serve/refire faces.

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
- **`mint` / `refire`** — the pure ephemeral mint/refire over the core
  (`SPEC.md` §8.2): `mint(run_id, base_json, dynamic_json=None, command, args,
  env)` narrows a base by an optional per-agent overlay into a `MintRecord` JSON
  (resolved policy + provenance); `refire(record_json)` re-mints the recorded
  resolved policy. *Why:* an app builds policy as data and mints/refires per-agent
  ephemeral projections in code without `lagom.toml` files; parity with the Go and
  Node bindings (NO DRIFT). A widening overlay or malformed record raises
  `ValueError`.
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
  `project`/`rewrite`/`merge`/`validate` plus the pure `mint`/`refire`
  (`SPEC.md` §8.2) to `wasm32-unknown-unknown` behind a
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
- **In-process `project`/`rewrite`/`merge`/`validate` + `Mint`/`Refire`** —
  driven through the embedded wasm engine with no subprocess. The wasm ABI gains
  two exports — `mint` (narrow a base by an optional per-agent dynamic overlay
  into a `MintRecord`, tagged-error on a widening overlay) and `refire` (re-mint
  the recorded resolved policy) — surfaced as `lagom.Mint(ctx, runID, base,
  dynamic, upstream)` / `lagom.Refire(ctx, record)` (`SPEC.md` §8.2). *Why:* §2 —
  the binding exposes the transport-less face (now incl. the ephemeral
  mint/refire ergonomics the first consumer needs) for in-process agents;
  spawning/stdio stay native (§7.3). Parity with Python/Node (NO DRIFT).
- **Proven by `go/lagom_test.go`** (`just go-test`): `project()` drops a tool and
  pins+hides an arg, `rewrite()` injects the pin and rejects an out-of-enum call
  as a tagged error, `merge()` rejects widening, `validate()` flags drift, and
  malformed input surfaces as a tagged error. The suite runs under
  `CGO_ENABLED=0` for both `go vet` and `go test`, so the `go get`-clean
  (no-cgo) guarantee is **enforced by the test path itself** — any cgo creep
  would fail the build. *Why:* the security-relevant Go face is exercised end to
  end against the same engine the Rust/Python faces use.

## 10. Node / TypeScript binding — `lagom-node` (SPEC §2, §7.2, §10)

The fourth language face: the core as a native Node addon (napi-rs), shipped as
`@hylla-io/lagom` with a generated `index.js` + typed `index.d.ts` so it is typed
out of the box. A thin in-process skin over `lagom-core`, in byte-identical
capability parity with Python and Go (NO DRIFT).

- **`project` / `rewrite` / `merge` / `validate`** over JSON strings, same
  contract as every other binding. Engine rejects, merge widening, and drift all
  throw a JavaScript `Error` carrying the engine's own annotated message — never
  swallowed (§9.1). *Why:* the compiled core as a normal npm dependency; the
  in-process binding the first slice's TS face requires (§10).
- **`mint` / `refire`** — the pure ephemeral mint/refire over the wasm-clean core
  (`mint(runId, baseJson, dynamicJson?, command?, args?, env?)` →
  `MintRecord` JSON; `refire(recordJson)` → `ResolvedPolicy` JSON), parity with
  Go/Python. A widening overlay or malformed record throws (§8.2). *Why:* an app
  builds policy as data and mints/refires per-agent ephemeral projections in
  code.
- **`Guard`** — `new Guard(upstreamJson, policyJson)` with `slimDefs()` / `gate(callJson)`,
  the brandable one-call helper mirroring `lagom_core::Guard` exactly (NO DRIFT,
  §1, SPEC §7.4). *Why:* an app wires a slim, branded MCP in two calls without
  touching `project`/`rewrite`.
- **`mintStdioServer(policyJson, command, args?, env?, audit?, runId?)`** — the
  spawned-process face (spawn the upstream child, drift-validate, bridge JSON-RPC
  over stdio applying the projection; `audit` writes an append-only JSONL trace).
  *Why:* §7.2 spawned-agent face, mirroring the CLI and Python.
- **Typed `PolicyBuilder`** (incl. `sealed()`) — author a `Policy` type-safely
  from TS/JS. *Why:* §6.1 integrator front-end in the binding language.
- **`shippedSkills()` / `emitSkills(dir?)`** — the identical embedded skill bytes
  the CLI writes. *Why:* §12 every binding bundles the skills.
- **Built/tested via `just node-build` / `just node-test`** (excluded from the
  workspace `just ci`, like `lagom-py`/`lagom-wasm`, since the addon needs the
  napi CLI + Node toolchain). Proven in the cross-binding parity guard (§11).

## 11. Cross-binding parity guard — `parity/` (CLAUDE.md "NO DRIFT")

The automated NO-DRIFT enforcement: one shared fixture
(`parity/fixtures/cases.json`, 16 `project`/`rewrite`/`merge`/`validate`/`mint`/
`refire` cases — success **and** error variants) is run through **all four
faces** and compared against the Rust core as the reference.

- **Four face runners** writing `parity/out/<face>.json`: `just rust-parity`
  (canonical reference, `lagom-core` directly), `just go-parity` (lagom-go via the
  committed wasm blob, `CGO_ENABLED=0`), `just py-parity` (the wheel in a
  throwaway uv venv), `just node-parity` (the napi addon). *Why:* prove every
  shipped face computes the same answer as the core.
- **Comparator** (`parity/runners/compare`, `just parity-compare`) asserts per
  case: (1) identical ok/err **classification** across faces, and (2) for ok
  cases, **byte-identical** result JSON; any divergence is a CRITICAL drift bug
  that fails the gate non-zero. Error-message *framing* (`ValueError` vs JS
  `Error` vs `lagom: <op>: <msg>` vs Rust `Display`) is a documented, legitimate
  per-binding difference, so err-case payload text is deliberately compared by
  classification only, not byte text. *Why:* byte-identity (all faces serialize
  the same `lagom_core` `BTreeMap`-backed serde types) catches any serializer- or
  behavior-level regression; classification upholds the never-swallow contract
  (§9.1) without forcing artificial cross-language message uniformity.
- **`just parity`** builds + runs all four faces then the comparator. *Why:* one
  command proves zero drift before a slice closes. **Current result: 16 cases × 3
  compared faces (Go/Py/Node) byte-identical to the Rust reference — zero drift.**
- **Caveat (inherited):** `go-parity` embeds the *committed*
  `go/internal/wasmbin/lagom.wasm`; if `lagom-core` changed, run `just wasm`
  first to refresh the blob or the Go face compares a stale engine (the same
  stale-blob caveat as `go-test`, tracked in KNOWN GAPS and noted in
  `parity/README.md` + the `just parity` recipe comment).

## 12. Build & CI tooling (ADR-0002)

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
- **`just node-build` / `just node-test` / `just node-check`** — build the napi
  `.node` addon + generated `index.js`/`index.d.ts`, smoke-test the real Node
  import (a `project()` drop + pin), and fmt+clippy the binding crate. *Why:*
  `lagom-node` is excluded from the workspace `just ci` (the addon needs the napi
  CLI + Node toolchain); it is built/tested by its own recipes, like `lagom-py`.
- **`just parity` (+ per-face `rust-/go-/py-/node-parity`, `parity-compare`)** —
  the cross-binding NO-DRIFT guard (§11). *Why:* a single local gate proves all
  four faces stay byte-identical to the core. (Local-only today; not yet a CI job
  — see KNOWN GAPS.)

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
- **A *published* Rust crate** — the binding matrix beyond Python, Node/TS, and
  Go is a 0.1.x packaging question (§13). (The Node/TS napi-rs binding **now
  ships** in 0.1.0 — see §10 — and is no longer deferred.)
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
DEFERRED into §9, and the Node/TS binding + cross-binding parity guard landed as
§10/§11). The lagom-finish synthesis also resolved the High **Node-README
doc-drift** finding (the per-binding Node README was missing Guard + mint/refire)
and the GONOSUMCHECK README inaccuracy — both folded into the docs below.
Resolved findings are kept inline below marked `[RESOLVED]` for traceability.

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

- **Cross-binding parity guard (`just parity`) is not wired into any CI job.**
  `.github/workflows/ci.yml` has only `check` (`just ci`) and `python` jobs; the
  NO-DRIFT parity matrix (§11) is a local gate only. A `parity` CI job would catch
  binding drift on every push. Left out of scope for the lagom-finish task (no CI
  edits requested); flagged to schedule alongside the already-tracked wasm/Go
  CI-coverage gap above — both are the same "the non-default faces are not yet in
  CI" gap. Stale-wasm-blob risk applies here too: `go-parity` embeds the committed
  blob, so run `just wasm` first when `lagom-core` changed (noted in
  `parity/README.md` + the recipe comment). No drift observed (the committed blob
  is fresh; `just parity` is green, 16 cases × 3 faces byte-identical).

- **Parity err-cases compared by classification only, not byte text.** For error
  cases the comparator asserts the ok/err classification matches across faces but
  does **not** byte-compare the error message, because each binding legitimately
  frames the engine's message in its own idiom (Python `ValueError`, JS `Error`,
  Go `lagom: <op>: <msg>`, Rust `Display`). This is an intentional, documented
  design choice (`parity/README.md`), not a coverage gap — the never-swallow
  contract and the classification are both verified, and the runners capture
  enough to extend the comparator to exact text if ever required.

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

### Low — docs completeness & cross-binding hygiene

- **No `missing_docs` lint is gated anywhere.** Current docs are complete (a
  forced `RUSTDOCFLAGS=-W missing_docs cargo doc` over the public crates yields
  zero warnings), but no crate declares `#![warn(missing_docs)]` /
  `#![deny(missing_docs)]` and neither `just ci` nor CI runs rustdoc with the
  lint. A newly-added public item in `lagom-core` (the single source of behavior)
  could ship undocumented and pass the gate, silently breaking the NO-DRIFT
  "docs full at all times" invariant. Process/gate gap, not a current defect.
  *Fix direction:* add `#![warn(missing_docs)]` to at least `lagom-core` and/or a
  `RUSTDOCFLAGS='-D missing_docs' cargo doc` step to the gate.

- **Rust intra-doc link syntax leaks into the generated TS JSDoc and Python
  docstrings.** The napi-generated `crates/lagom-node/index.d.ts` JSDoc (and the
  underlying shared `///` comments the Python binding surfaces) carry Rust
  intra-doc markup that does not resolve outside rustdoc — e.g.
  `` [`lagom_core::Guard`] ``, `` [`keep`](Self::keep) ``, `` [`refire`] `` — so a
  TS editor / TypeDoc and the Python `__doc__` render the raw `[...](Self::...)`
  text rather than links. Cosmetic, not a missing-doc. *Fix direction:* in the
  napi/pyo3 doc comments prefer plain prose names ("the `keep` method",
  "`lagom_core::Guard`") over intra-doc link syntax; keep real intra-doc links in
  `lagom-core` rustdoc where they resolve.

### Low — security re-attack (inherited)

- **No oversized-input test on the wasm ABI.** `go/lagom.go` bounds-checks the
  packed `(ptr, len)` result so an out-of-range pointer fails loud, but no test
  asserts that behavior. The check exists; coverage of the failure path does not.

- **Pattern-constraint regexes are applied verbatim.** `lagom_core::rewrite`
  tests a `Constraint::Pattern` regex as authored; a loose, unanchored regex can
  admit path traversal. lagom enforces exactly what the policy says — anchoring is
  the policy author's responsibility, so a consumer like `sand` must anchor the
  regexes it generates (`^…$`). Documented expectation, not a core defect.

### Resolved (folded into the feature sections; kept for traceability)

- **[RESOLVED — §10/docs] The per-binding Node README omitted `Guard` and
  `mint`/`refire` (doc-parity drift vs the Go README).**
  `crates/lagom-node/README.md` documented the four pure ops, `PolicyBuilder`,
  `mintStdioServer`, the shipped skills, and branding, but had **no** `Guard`
  section and **no** `mint`/`refire` section — while the Go README and the root
  README both documented them, and the root README points Node consumers at the
  per-binding README "for the full Node API". Per CLAUDE.md "Binding parity &
  docs — NO DRIFT" this was a drift bug. *Fixed:* added a "The brandable one-call
  helper — `Guard`" section (`new Guard` / `slimDefs` / `gate`) and an "Ephemeral
  mint & refire" section (`mint` / `refire`) to `crates/lagom-node/README.md`,
  mirroring the Go README against the real TS surface, and added `mint`/`refire`/
  `Guard` to the binding's import example.

- **[RESOLVED — README] Root README referenced the non-existent Go env var
  `GONOSUMCHECK`.** *Fixed:* the `GOPRIVATE` note now says `GOPRIVATE` provides the
  default for `GONOPROXY` and `GONOSUMDB` (skipping the public proxy + checksum
  DB), dropping the bogus `GONOSUMCHECK`.

### Resolved (slice-1 / slice-2; kept for traceability)

- **[RESOLVED — §5/§4] No shipped face persisted a full `MintRecord`
  (provenance) for refire, and there was no "refire from a recorded run"
  command.** `mint_record()` / `refire()` were library-only with zero non-test
  callers. *Fixed:* the mint-record data types (`MintRecord`, `PolicySources`,
  `ResolvedPolicy`, `UpstreamCommand`) and the pure `mint`/`refire` moved into
  `lagom-core` (transport-less, wasm-safe; `lagom-proxy` re-exports them, keeping
  its file-loading `mint`). `lagom serve --audit` now persists a full
  `AuditEvent::Mint { record }` (resolved policy + provenance: config paths +
  dynamic inputs + upstream command) as the trace head; the new `lagom refire
  --record <path>` re-mints that exact recorded projection (reading a bare
  `MintRecord` JSON or a JSONL trace's first `mint` event), even after the source
  config drifted. Every binding exposes the pure mint/refire over the wasm-clean
  core (Go `Mint`/`Refire`, Python/Node `mint`/`refire`) for in-code per-agent
  ephemeral projections. Covered by `refire_from_record_serves_the_recorded_projection`
  + the serve-audit `mint`-provenance assertions (`crates/lagom-cli/tests/cli.rs`),
  `mint_then_refire_round_trips` in each binding, and the new
  `lagom-core`/`lagom-wasm` mint/refire unit tests.

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
