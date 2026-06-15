# lagom

lagom takes existing MCP servers and serves *narrowed* views of them — fewer
tools, pinned or hidden arguments, slimmer tool docs — so an agent sees and can
do **just enough** (Swedish *lagom*) for its job: nothing more to abuse, nothing
extra to bloat its context.

## Language

**Upstream server**:
The real, full-capability MCP server that lagom wraps. lagom does not author it;
the dev does. lagom only re-presents it.
_Avoid_: backend, origin, source server (use "upstream").

**Projection**:
The narrowed view of an upstream server that lagom serves downstream — the
result of applying a transform to the upstream's tool surface. The unit of
lagom's value.
_Avoid_: filter, view, slice (use "projection").

**Capability surface**:
The set of tools, args, and docs an agent can actually see and call through a
projection. lagom's job is to make this surface minimal and sealed.

**Pin**:
To fix a tool argument to an absolute value server-side and remove it from the
tool's input schema, so the agent never sees or sets it. Distinct from
*whitelisting* an arg (which leaves it visible and agent-settable).
_Avoid_: lock, freeze, fix (use "pin").

**Narrow**:
To make a projection strictly smaller — drop tools, pin/hide args, shrink docs.
Projections may only ever narrow, never widen (monotonic). A downstream consumer
can narrow further but can never recover capability the dev removed.
_Avoid_: restrict, limit, scope-down (use "narrow").

**Profile**:
A named, reusable projection definition authored by a dev (e.g. "read-only
hylla over repos X and Y"). What end-users select and further narrow.

**Minting**:
Producing a concrete lagom proxy instance from a profile/config at runtime —
e.g. an orchestrator minting a per-subagent projection with file-scoped args.
_Avoid_: spawning, generating (use "minting" for the projection-creation sense).

**Integrator**:
A developer who builds an app or harness on top of a lagom binding and authors
the base narrowing (profiles) for that app. The first of two authority tiers.
_Avoid_: developer, vendor (use "integrator" when distinguishing from end-user).

**End-user**:
A user of an integrator's app who may narrow *further* within the integrator's
sealed bounds, but can never widen them. The second authority tier.
_Avoid_: user, consumer (use "end-user" when the authority tier matters).

**Sealed bounds**:
The integrator-authored ceiling on a projection. End-user narrowing may only
lower this ceiling, never raise it. The invariant that makes the sandbox a
guarantee rather than a suggestion.

**Passthrough**:
lagom's default for anything a policy does not explicitly drop, pin, constrain,
or rewrite: it is forwarded unchanged. lagom is transparent except where told
otherwise. _Avoid_: default-allow, forward (use "passthrough").

**Ephemeral projection**:
A projection minted for a single agent and a single run, then discarded. Must
still be reproducible (refire from failure) and traceable — so a mint records
its fully-resolved policy plus provenance.
_Avoid_: temporal, throwaway, one-shot (use "ephemeral").

**Addendum**:
A deterministic note lagom appends to a passed-through tool description, stating
the restrictions it applied (e.g. "`artifact` is fixed; `path` ∈ {a, b}"). The
fallback when the integrator supplies no slim description override.
_Avoid_: footnote, annotation (use "addendum").

**Caveman docs**:
Tool descriptions rewritten for an LLM consumer: grammar-free, content-only,
every unnecessary token removed; pinned/removed args appear nowhere. The target
style of the `lagom-slim-docs` skill's output, committed as a Tier-1 override.
_Avoid_: terse docs, minified docs (use "caveman docs").

**Skill (lagom skill)**:
A loadable markdown document lagom ships (embedded in the binary, bundled in
bindings) that teaches a *host* orchestrator how to use lagom — e.g.
`lagom-slim-docs`, `lagom-dynamic-mint`. lagom never runs an agent or model
itself in v0.1.0; skills are inert until a host loads them.
_Avoid_: plugin, prompt, template (use "skill").

**Face**:
One of lagom's delivery surfaces over the single Rust core: the CLI (out-of-process
stdio proxy), the language **bindings** (Python wheel, Go module), and the wasm
face. None is primary; all consume the same transport-less core (ADR-0001).
_Avoid_: frontend, adapter (use "face" for the delivery-surface sense).

**wasm ABI**:
The JSON-in / JSON-out memory contract the `lagom-wasm` face exposes so a wasm
host (wazero in Go) can drive the core: exported `alloc`/`dealloc` plus one
export per op taking `(ptr, len)` and returning a packed `(ptr << 32) | len`,
the result buffer prefixed with a **status tag** so an engine reject/drift
surfaces as a tagged error rather than being swallowed (SPEC §9.1).
_Avoid_: wasm interface, FFI shim (use "wasm ABI").

**Go binding (lagom-go)**:
The Go module (`github.com/hylla-io/lagom/go`) that `go:embed`s the core compiled
to wasm and runs it in-process via **wazero** (pure Go, no cgo) — so a consumer
gets the whole `project`/`rewrite`/`merge`/`validate` engine from a single
`go get`, no separate binary and no C toolchain.
_Avoid_: Go bridge, cgo binding (use "Go binding" / "lagom-go").

## Resolved architecture (see docs/adr/ for rationale)

- The **CLI face** is an **out-of-process stdio proxy binary**. Topology:
  `harness → lagom (stdio JSON-RPC) → upstream server (child proc)`. It wraps
  servers for harnesses we don't control.
- The **bindings are in-process**: one Rust core compiled and shipped as a normal
  project dependency (Python wheel via PyO3/maturin; Go module via wasm + wazero).
  They expose `project`/`rewrite`/`merge`/`validate` (in-process agents) and
  `mint_stdio_server` (spawned agent processes). They are thin faces over the
  core, not reimplementations.
