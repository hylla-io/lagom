# lagom — v0.1.0 Specification

Pre-stability (0.x): breaking changes allowed. This spec captures the **full
planned capability**; §10 marks what the **first build slice** ships.

Domain language is canonical in [`CONTEXT.md`](CONTEXT.md). Architecture
rationale in [`docs/adr/`](docs/adr/). This document is the implementation spec.

---

## 1. Thesis

lagom wraps an upstream MCP server and serves a **projection** of it: the agent
sees and can call **just enough** for its job — pinned/hidden args, constrained
domains, slimmer docs — nothing more to abuse, nothing extra to bloat context.

Two differentiators over existing tool-filter/arg-whitelist proxies:

- **Pin** — fix an arg to an absolute value *and remove it from the schema*, so
  the agent never sees it (context win + security win), lagom injects it.
- **Authored slim docs** + per-agent **ephemeral projections**, deterministic
  enough to refire from failure and fully traceable.

## 2. Architecture (see ADR-0001)

One **Rust core** — a transport-less, side-effect-free transform + policy engine
— behind multiple **faces**:

- **CLI** — standalone binary; out-of-process stdio proxy; harness-agnostic
  drop-in. Wraps servers for harnesses we don't control.
- **Bindings** — the compiled core shipped as a normal project dependency
  (Python wheel via PyO3/maturin, Node/TS addon via napi-rs, Rust crate, Go via
  wasm+wazero). Exposes `project`/`rewrite`/`merge`/`validate`, the pure
  `mint`/`refire`, the brandable `Guard` helper (§7.2), and `mint_stdio_server`
  (spawned agent processes).

Dependency direction is fixed: everything depends on the pure core; nothing pure
depends on transport. The stdio server and CLI are thin adapters.

## 3. The `Policy` model (canonical)

`Policy` is the projection spec as data — the single model both front-ends
(typed builder, `lagom.toml`) produce, and the only thing the engine consumes.

A `Policy` over an upstream is, per tool:

- **presence** — keep | drop (default: keep — *passthrough*).
- **rename** — optional downstream name.
- **description** — override text | passthrough+addendum (see §4.2).
- **args**, per parameter:
  - **pin** `value` — remove from schema, inject on every call.
  - **constrain** — `enum` subset | numeric range | regex pattern.
  - **default** `value` — supply if absent (stays visible, unlike pin).
  - **forbid** — the argument must be absent: removed from the schema, and a
    call carrying it is rejected. Valid on an argument the upstream does not
    declare, so a host can prove a tool never receives it. Matches the
    **exact** argument name, as every argument rule does: `Version_Pin` or
    `version_pin ` is a different argument and passes through. The guarantee
    therefore holds for an upstream that matches argument names exactly; one
    that folds case or trims names must forbid each spelling it accepts.
  - **passthrough** — unchanged (default).

Engine API (shape, not signatures):

- `project(upstream_defs, policy) -> projected_defs`
- `rewrite(projected_call, policy) -> upstream_call | Reject`
- `merge(base, overlay) -> Policy` — **narrow-only** (see §5.2)
- `validate(policy, upstream_defs) -> Ok | DriftError`

## 4. Transforms

### 4.1 Schema (deterministic — does the real work)

The `inputSchema` is structured data; transforms are lossless-by-construction
and *prevent* invalid calls structurally (the agent cannot form a call it cannot
make, killing the call→fail→retry loop):

- **pin** → delete property from `properties` + `required`.
- **forbid** → delete a declared property from `properties` + `required`; a
  call that still carries the argument (any value, `null` included) is rejected
  by name, never stripped.
- **constrain (set)** → set `enum: [...]`.
- **constrain (range/pattern)** → `minimum`/`maximum`/`pattern`.
- **drop** → omit tool from `tools/list`.

### 4.2 Description (deterministic at runtime — authored, never runtime-LLM)

Tier ladder:

- **Tier 1 — integrator override**: explicit slim text (builder `.describe()` /
  TOML `description`). Preferred.
- **Tier 2 — addendum fallback**: original text + a deterministic note generated
  from the schema diff ("Restricted: `artifact` is fixed; `path` ∈ {a,b}.").
  Brand-free so lagom stays invisible; never contradicts the schema.
- **Tier 0 — shipped skill, NOT a runtime LLM call** (`SPEC.md` §13): lagom
  ships a loadable skill that instructs a *host* orchestrator to use a small
  model to diff the full upstream def against the slim projected surface and
  author **caveman** descriptions (grammar-free, content-only, every
  unnecessary token removed — a pinned arg appears nowhere). The output is
  committed as a Tier-1 override. lagom itself never calls an LLM in v0.1.0.

Never: runtime LLM rewriting (breaks refire determinism) or regex prose surgery
(mangles meaning). Original description always retained in the audit log.

## 5. Guarantees & invariants

- **5.1 Call-contract guarantee**: a required param may be removed from the
  projected schema **only if** lagom supplies its value (pin). `forbid` on an
  argument the upstream marks `required` is drift (§5.3). The upstream always
  receives a valid call.
- **5.2 Monotonic narrowing (sealed bounds)**: `merge(base, overlay)` lets the
  overlay (end-user) only **narrow** — drop, tighten, pin, forbid. Any widening
  (re-add a dropped tool, loosen a constraint, unpin, un-forbid, or forbid a
  pinned argument) is a **load-time error**, not a silent ignore. This merge
  *is* the sandbox enforcement.
- **5.3 Drift safety**: `validate` checks every policy reference against the live
  upstream `tools/list` at startup/mint. A vanished param/tool → **fail loud**,
  refuse to serve. No silent broken forwarding.
- **5.4 Determinism**: minting invokes no LLM and no nondeterministic input, so
  the same `(profile, overlays, dynamic inputs)` produce a byte-identical
  resolved policy — the precondition for refire (§8.2).

## 6. Config

- **6.1 Two front-ends, one model**: typed **builder** in the binding language
  (integrators; type-safe; home of dynamic mint-time scoping) and **`lagom.toml`**
  (CLI face + end-user narrowing). Both produce `Policy`; they cannot drift.
- **6.2 Format**: TOML, branded `lagom.toml`. Carries only the flat human
  surface (which tools, which args pinned/constrained/forbidden, description
  overrides).
  Rich transforms live in the builder. A JSON Schema for `lagom.toml` ships for
  editor validation.
- **6.3 Layering + discovery**: integrator base profile (by name/path) ←
  end-user overlay, discovered by precedence: explicit `--config` →
  `./lagom.toml` → project root → `$XDG_CONFIG_HOME/lagom/`. Composed via the
  narrow-only `merge` (§5.2).
- **6.4 Multi-profile**: single `lagom.toml` default; `.lagom/` dir of named
  profiles for apps shipping many.
- **6.5 Harness integration**: lagom **never edits** `.mcp.json`/`settings.json`.
  `lagom emit` prints the exact stdio-server snippet to paste into any harness.
- **6.6 Policy from the host**: lagom holds no database and needs no file. A
  host keeps policies with its own settings and hands them over as a **policy
  document** — strict, versioned JSON (`lagom_core::document`, and
  `policy_from_document`/`policy_to_document` on every binding):
  `{"lagom_policy": 1, "default_presence": "drop", "tools": {…}}`. The format
  key is read first, so a newer format fails by name; `default_presence` is
  required, so a stored sealed ceiling can never silently become passthrough;
  unknown keys are refused; a key repeated in any object, at any depth, is
  refused with its JSON Pointer, since a plain JSON read keeps only the last
  value and could drop a `forbid` or a seal. Writing a document refuses a
  policy with no `default_presence` rather than store an assumed `keep`.
  Only text is checked for repeats: `document::from_value` takes a value a JSON
  reader has already merged, so a host holding text calls `document::parse`.
  A JSON Schema ships (`policy_document_schema`).
  `lagom_config::parse_str` lowers `lagom.toml` text a host keeps anywhere.
  For a spawned server, the host mints and writes the one generated record
  `lagom refire --record` reads — config discovery never runs there.
  `lagom_core::VERSION` lets a host check the binary it launches against the
  library it linked.

## 7. Faces in detail

- **7.1 CLI**: `lagom serve --config <f>` (stdio proxy: spawn upstream child,
  serve projected surface; `--audit <path>` persists a full mint record +
  trace, §8.2); `lagom refire --record <path>` (re-mint an identical server from
  a persisted mint record, §8.2); `lagom emit` (print harness snippet); `lagom
  validate` (policy vs upstream); `lagom emit-skills [dir]` (write the shipped
  skill markdown into a chosen directory — §12). *Superseded:* the originally
  planned `lagom describe --suggest` (dev-time doc aid) is **not shipped**; its
  intent — authoring slim/caveman descriptions — is delivered by the
  `lagom-slim-docs` skill (§12), emitted via `emit-skills`, which keeps lagom
  free of any built-in LLM call (§4.2 Tier 0).
- **7.2 Bindings**: `project`, `rewrite`, `merge`, `validate`, the pure
  ephemeral `mint(run_id, base, dynamic) -> MintRecord` / `refire(record) ->
  ResolvedPolicy` (§8.2), the brandable **`Guard`** helper (§7.4), and
  `mint_stdio_server(policy)`. Typed `Policy` builder per language. Shipped
  bindings: **Python** (PyO3/maturin wheel) and **Node/TS** (napi-rs addon, with
  generated `index.d.ts` so it is typed out of the box) for in-process and
  spawned-process agents, plus the **Go** binding (§7.3, wasm+wazero, pure-compute
  ops + an in-language `Guard`). All bindings are a thin skin over the one core
  and hold **byte-identical capability parity (NO DRIFT)** — proven by the
  cross-binding parity guard (`parity/`, run via `just parity`).
- **7.3 wasm / Go (shipped)**: the transport-less core
  (`project`/`rewrite`/`merge`/`validate`) compiled to `wasm32-unknown-unknown`
  (`lagom-wasm` crate) and run via **wazero** in Go (no cgo). Pure compute only —
  no spawning/stdio (those stay native: CLI + `mint_stdio_server`). The wasm face
  exposes a **wasm ABI**: a JSON-in/JSON-out memory contract — exported
  `alloc`/`dealloc` plus one export per op taking `(ptr, len)` and returning a
  packed `(ptr << 32) | len`, the result buffer prefixed with a **status tag** so
  engine rejects/drift surface as a tagged error, never swallowed (§9.1). The
  `lagom-go` module (`github.com/hylla-io/lagom/go`) `go:embed`s the built
  `.wasm` so a consumer gets the whole engine from a single `go get` — no
  separate binary, no C toolchain.
- **7.4 `Guard` — the brandable one-call helper**: pairs an upstream tool
  surface with a frozen `Policy` so an integrator wires a slim, **branded** MCP
  in two calls — `slim_defs()` returns the projected (branded) downstream
  `tools/list` (computed once at construction), and `gate(call)` rewrites each
  incoming `tools/call` back to upstream (or rejects it) — without the app ever
  touching the `project`/`rewrite` plumbing or threading the policy through each
  call. All branding (renamed tool names, override descriptions, dropped tools)
  lives in the `Policy` the app supplies; the helper reads nothing itself
  (no `lagom.toml`, no disk, no env), so lagom stays invisible (§2, the first
  consumer `sand` needs lagom-as-a-lib to be stupid-easy). Shipped in every
  binding with byte-identical behavior (NO DRIFT): `lagom_core::Guard` (Rust),
  `lagom.Guard` (Python), `new Guard(...)` (Node/TS), `lagom.NewGuard(...)` (Go).
  The wasm face exposes no `Guard` export — the JSON-in/JSON-out ABI cannot hand
  back a Rust struct — so the Go binding **re-implements `Guard` in-language**
  over the wasm `project`/`rewrite` primitives, preserving the four-binding
  parity (this is intentional layering, not a missing wasm export).

## 8. Dynamic policy & ephemeral projections

- **8.1 Mint-time dynamism (primary)**: the orchestrator computes a per-agent
  projection at spawn (e.g. `path` constrained to the exact files this agent may
  touch), mints a server fixed for that agent's life. Works on every face.
- **8.2 Ephemeral projection**: minted for one agent + one run, then discarded.
  A mint records its **resolved policy** + **provenance** (id, base profile,
  overlays, dynamic inputs). **Refire** = re-mint from the recorded resolved
  policy → identical server. **Trace** = append-only log links `run-id → mint
  record → each rewritten call + original`.
- **8.3 Per-call hooks (deferred, post-0.1.0)**: an in-process-only
  `authorize(call) -> allow|deny|rewrite` callback for policy that changes
  mid-run. Documented seam; not in the 0.1.0 core (keeps core wasm-clean).

## 9. Error semantics & audit

- **9.1 Never swallow**: constraint violations and upstream errors pass back to
  the agent **annotated** with what lagom did ("rejected: `path` must be one of
  {a,b}").
- **9.2 Defense in depth**: prevent via schema (§4.1) → explain via description
  (§4.2) → annotate if it still errors (§9.1).
- **9.3 Audit log**: append-only; holds original tool defs, the resolved policy,
  and every rewrite/rejection. Source of refire + traceability.

## 10. 0.1.0 build slice (scope boundaries)

In the first slice:

- Narrow **tools only**; transparently **passthrough** resources, prompts,
  `initialize`, notifications, pagination (the transform model is
  surface-uniform; resources/prompts narrowing is an additive later slice).
- **stdio transport only**.
- lagom **spawns the upstream** as a child process (config carries its launch
  command/args/env); minimal lifecycle (spawn on init, teardown on exit, crashes
  loud).
- Faces: CLI + the Python binding proving the embed model; the **Node/TS binding
  (napi-rs) ships in this slice** at full capability parity with Python; the
  **wasm/Go face (§7.3) is shipped in this slice** — the core compiled to wasm32
  and embedded in the `lagom-go` module via wazero (no cgo). Cross-binding parity
  (Rust/Go/Py/TS) is guarded by `parity/` (`just parity`).

## 11. Deferred (planned, not in first slice)

- Resource / prompt narrowing.
- HTTP / SSE / streamable-HTTP transport; attach-to-running upstream.
- Per-call authorization hooks (§8.3).
- Restart/supervision policy for upstream child.

## 12. Shipped skills (no LLM in lagom)

lagom ships loadable skill documents — embedded in the binary (`include_str!`)
and bundled in every binding — that teach a *host* orchestrator agent how to use
lagom well. lagom never connects an agent or calls a model itself in v0.1.0; the
skills are inert markdown until a host loads them. A CLI/binding command emits
them into the consumer's repo at a path the user chooses (lagom does not manage
how a harness discovers skills — harness-agnostic, like `lagom emit`).

- **`lagom-slim-docs`** — instructs the host to use a *small* model to compare
  the full upstream tool def against the slim projected surface and rewrite each
  description in **caveman** style (grammar-free, content-only), stripping every
  unnecessary token; removed/pinned args appear nowhere. Output is committed as a
  Tier-1 override (deterministic at runtime; §4.2 Tier 0).
- **`lagom-dynamic-mint`** — instructs the host to mint an **ephemeral**
  projection scoped to a single consuming agent's lifecycle (e.g. constrain
  `path` to the exact files that agent may touch), wire it into that agent, and
  tear it down at end of life (§8).

Connecting lagom directly to a model is explicitly deferred (post-0.1.0,
possibly an opt-in).

## 13. Open questions / unknowns

- Exact `lagom.toml` schema surface (key names) — settle at builder design.
- Audit log on-disk shape (default JSONL at a configurable path) — settle at §9.
- Binding packaging matrix beyond the shipped set (Python wheel, Node/TS addon,
  and Go-via-wasm ship in 0.1.0; a *published* Rust crate is 0.1.x).

*Resolved since first draft:* MCP wire = hand-rolled selective-interception
proxy (not `rmcp`); adapter I/O = tokio; doc-slimming = shipped skill (§12), not
a built-in LLM command.
