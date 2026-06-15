# lagom

**lagom** takes an existing MCP server and serves a *narrowed* view of it —
fewer tools, pinned or hidden arguments, constrained domains, slimmer tool docs —
so an agent sees and can do **just enough** (Swedish *lagom*) for its job:
nothing more to abuse, nothing extra to bloat its context.

It is a thin, transport-less Rust core behind multiple **faces**:

- a standalone **CLI** that runs an out-of-process stdio proxy — a drop-in for
  any harness that reads `.mcp.json` / `settings.json` (Claude Code, etc.);
- **language bindings** shipping the compiled core as a normal project
  dependency, for in-process and spawned-process agents: **Python**
  (PyO3/maturin), **Node/TypeScript** (napi-rs), and **Go** (the core compiled to
  wasm and run in-process via wazero — pure Go, no cgo, one `go get`).

What makes lagom different from a plain tool-filter / arg-whitelist proxy:

- **Pin** — fix an argument to an absolute value *and remove it from the
  schema*, so the agent never sees it. lagom injects it on every call. Context
  win and security win at once.
- **Sealed bounds** — an integrator authors a base projection; an end-user may
  narrow *further* but can never widen it. The merge is the sandbox.
- **Authored slim docs + ephemeral projections** — deterministic enough to
  refire from failure and fully traceable. lagom never calls an LLM at runtime.

See [`SPEC.md`](SPEC.md) for the full design (and the 0.1.0 scope boundaries),
[`CONTEXT.md`](CONTEXT.md) for the canonical glossary, and
[`docs/adr/`](docs/adr/) for architecture rationale. A complete,
spec-cross-checked feature inventory lives in [`FEATURES.md`](FEATURES.md).

---

## Install

### CLI

Build the binary from the workspace:

```sh
cargo build --release -p lagom-cli
# binary at ./target/release/lagom
```

Or run it straight from the workspace during development:

```sh
cargo run -p lagom-cli -- --help
```

### Python binding

The binding is built with [maturin](https://www.maturin.rs/) into an `abi3`
wheel and installed by **path** (the PyPI name `lagom` is taken by an unrelated
library):

```sh
just py        # builds the wheel + runs the smoke test in a throwaway uv env
```

To install into your own environment:

```sh
maturin build --release --manifest-path crates/lagom-py/Cargo.toml --out dist
pip install dist/*.whl
```

```python
import lagom  # exposes project, rewrite, merge, validate, mint, refire,
               # mint_stdio_server, PolicyBuilder, Guard, shipped_skills,
               # emit_skills
```

The tool defs are read leniently, so you can pass the **raw upstream
`tools/list`** straight in with no field-mapping shim: each tool's schema is
accepted under either the MCP-native camelCase `inputSchema` or snake_case
`input_schema`, and a missing or `null` schema normalizes to `{}`. The projected
output is always canonical snake_case `input_schema`.

### Go (`go get`)

The Go binding is the core compiled to wasm and run in-process via
[wazero](https://wazero.io) — **pure Go, no cgo, no separate binary**. The
`.wasm` is `go:embed`-ed, so a single `go get` delivers the whole
`project`/`rewrite`/`merge`/`validate` + `Mint`/`Refire` + `Guard` engine:

```sh
go get github.com/hylla-io/lagom/go
```

```go
import lagom "github.com/hylla-io/lagom/go"
```

The module path is `github.com/hylla-io/lagom/go` (the binding lives in the `go/`
subdirectory of the repo). `wazero` is the **only** non-stdlib dependency.

**Private module — `GOPRIVATE`.** Until the GitHub repo is public, tell the Go
toolchain to skip the public proxy and checksum database for it, and make sure
`git` can authenticate to the private repo:

```sh
go env -w GOPRIVATE=github.com/hylla-io/*
# fetch over SSH (or a token) instead of the unauthenticated https proxy:
git config --global url."git@github.com:hylla-io/".insteadOf "https://github.com/hylla-io/"
```

`GOPRIVATE` provides the default for `GONOPROXY` and `GONOSUMDB` for the matched
prefix, so `go get` skips the public module proxy and will not try to verify the
module against `sum.golang.org`.

### TypeScript / Node (`npm`)

The Node binding is a native addon (via [napi-rs](https://napi.rs)) shipping a
generated `index.js` + `index.d.ts`, so it is fully typed out of the box:

```sh
npm install @hylla-io/lagom
```

```ts
import { project, rewrite, merge, validate, mint, refire, Guard, PolicyBuilder }
  from "@hylla-io/lagom";
```

To build from source instead: `just node-build` (compiles the `.node` addon and
emits the typed `index.js` / `index.d.ts`). See
[`crates/lagom-node/README.md`](crates/lagom-node/README.md) for the full Node
API.

> **One core, four faces — zero drift.** Every binding (Rust, Python, Node, Go)
> is a thin skin over the same `lagom-core`, with a byte-identical JSON-in /
> JSON-out contract. A capability present in one binding is present in all of
> them.

---

## Quickstart

### 1. Wrap a server

lagom spawns the upstream MCP server as a child process. The upstream launch
command is passed as trailing args after `--`:

```sh
lagom serve -- npx -y @some/mcp-server
```

With no `lagom.toml`, lagom is a transparent **passthrough** — every tool, arg,
and description forwarded unchanged. The value comes from the policy.

### 2. Write a `lagom.toml`

The flat human surface: which tools to keep/drop, which args to pin/constrain,
and Tier-1 description overrides. A worked example:

```toml
# Sealed allowlist: drop everything not named below.
default-presence = "drop"

[tools.search]
presence = "keep"
description = "Search committed artifacts. query is the only knob."

# Pin `artifact` to a fixed value: it disappears from the schema and lagom
# injects it on every call — the agent never sees or sets it.
[tools.search.args.artifact]
pin = "github.com/hylla-io/lagom@main"

# Constrain `limit` to an inclusive range; the agent can still choose within it.
[tools.search.args.limit]
min = 1
max = 50

[tools.read_file]
presence = "keep"
rename = "read"   # expose under a different downstream name

# Constrain `path` to a regex the agent cannot escape.
[tools.read_file.args.path]
pattern = "^src/"
```

Per argument, exactly one of `pin`, `default`, `enum`, `min`/`max`, or `pattern`
applies. A JSON Schema for `lagom.toml` is produced by `lagom_config::json_schema`
for editor validation.

Validate the policy against the live upstream before serving — drift (a vanished
tool or arg) fails loud:

```sh
lagom validate --config lagom.toml -- npx -y @some/mcp-server
```

Then serve the projection:

```sh
lagom serve --config lagom.toml -- npx -y @some/mcp-server
```

Config is discovered by precedence when `--config` is omitted: `./lagom.toml` →
project-root `lagom.toml` → `$XDG_CONFIG_HOME/lagom/lagom.toml`. An end-user
overlay is composed onto the integrator base via the **narrow-only** merge:
narrowing wins, widening is a load-time error.

### 3. `lagom emit` — wire it into a harness

lagom **never edits** your harness config. `lagom emit` prints the exact
stdio-server snippet to paste into `.mcp.json` / `settings.json`:

```sh
lagom emit --config lagom.toml --name hylla -- npx -y @some/mcp-server
```

```json
{
  "mcpServers": {
    "hylla": {
      "command": "lagom",
      "args": ["serve", "--config", "lagom.toml", "--", "npx", "-y", "@some/mcp-server"]
    }
  }
}
```

### Shipped skills

lagom bundles two loadable skill documents (inert markdown that teaches a *host*
orchestrator how to use lagom — lagom never runs them or calls a model):

```sh
lagom emit-skills ./skills    # writes lagom-slim-docs + lagom-dynamic-mint
```

The Python binding bundles the identical bytes via `lagom.emit_skills(dir)`.

---

## Embed as a library: the `Guard` one-call helper

When an app embeds lagom (rather than running the CLI), it wires a slim,
**branded** MCP in two calls with `Guard` — lagom stays invisible. Given the
app's own full tool defs plus a policy (built however the app likes — a builder,
the app's own config, a JSON literal; lagom reads nothing itself), a `Guard`:

- exposes **`slimDefs` / `slim_defs()`** — the projected, branded tool defs to
  register as the downstream `tools/list`; and
- gates every incoming `tools/call` through **`gate(call)`** — returning the
  rewritten upstream call (pins injected, name mapped back) or an annotated
  error — so the app never touches the `project` / `rewrite` plumbing.

**Branding (lagom invisible):** the policy carries everything the agent sees —
`rename` sets the app's *own* tool names, an `override` description sets the
app's *own* docs, and `default_presence` / `drop` decide the surface. lagom's
name appears only in `go.mod` / `pip` / `npm`, never to the agent.

The same shape ships in every binding (Rust [`lagom_core::Guard`], Python
`lagom.Guard`, Node `new Guard(...)`, Go `lagom.NewGuard(...)`) with identical
behavior (NO DRIFT). Go (the first consumer's language) example:

```go
import lagom "github.com/hylla-io/lagom/go"

// `policy` is the app's branded projection as JSON — built from the app's own
// config; lagom never reads it from disk. Here: keep `search` under the app's
// own name `find`, pin `artifact` (hidden), drop everything else.
g, err := lagom.NewGuard(ctx, upstreamDefsJSON, policyJSON)
if err != nil { /* drift / malformed input — fail loud */ }

// Register g.SlimDefs() as your server's tools/list (already shows `find`, not
// `search`; no `artifact`; no lagom anywhere).
toolsList := g.SlimDefs()

// Gate each incoming tools/call: maps `find` -> `search`, injects the pin, or
// returns an annotated error to relay to the agent.
upstreamCall, err := g.Gate(ctx, incomingCallJSON)
```

Python and Node mirror it:

```python
g = lagom.Guard(upstream_json, policy_json)
tools_list = g.slim_defs()
upstream_call = g.gate(incoming_call_json)   # raises ValueError on reject
```

```ts
const g = new lagom.Guard(upstreamJson, policyJson);
const toolsList = g.slimDefs();
const upstreamCall = g.gate(incomingCallJson); // throws on reject
```

---

## Branding — keep lagom invisible

lagom is designed to disappear. An app that embeds a binding presents its **own**
MCP — its own server name, its own tool names, its own docs — and the agent never
learns lagom is in the path. The only place the name `lagom` appears is the
dependency line (`go get` / `pip` / `npm`); it is never in `tools/list`, never in
a tool description, never in an error the agent sees.

Branding is **data, not code** — it lives entirely in the `Policy` the app
supplies. lagom reads nothing itself (no `lagom.toml`, no env, no disk); the app
parses its own config and hands lagom a `Policy`:

| What the agent sees | How the policy sets it |
| --- | --- |
| the app's **own tool names** | `rename` (e.g. upstream `search` → `find`) |
| the app's **own short docs** | `description` override (Tier-1 slim text) |
| **which tools exist at all** | `default_presence = "drop"` + per-tool `keep` |
| **no hidden args** | `pin` (removed from the schema, injected server-side) |

Practical checklist for an invisible integration:

1. Start from a **sealed** policy (`default_presence = "drop"` / `PolicyBuilder.sealed()`)
   so nothing leaks by accident — allowlist only what the agent needs.
2. `rename` every kept tool to the app's vocabulary; the agent never sees the
   upstream name (and `gate`/`rewrite` maps it back transparently).
3. `describe` each tool with the app's own slim text (or let the deterministic
   addendum state the restrictions). Never echo lagom.
4. Expose the surface through `Guard` — `slimDefs()` is the branded `tools/list`,
   `gate()` is the branded call path. The app never touches `project`/`rewrite`.
5. `lagom` stays out of the server name you register with the harness — that name
   is the app's.

`lagom.toml` is **only** for people running the standalone `lagom` CLI. An app
embedding a binding does not use `lagom.toml`; it builds the `Policy` in code or
from its own branded config (`docs/SAND_LAGOM_HANDOFF.md` §3).

---

## Multiple MCPs / multiple agents

lagom narrows **one** upstream per projection. Two independent axes compose
cleanly from that primitive — no special multi-server mode required.

### Many upstreams for one agent — compose N wrapped servers

To give one agent slim views of several upstreams, wrap each upstream
**separately** and register all of them. Each wrapper is its own slim,
independently-branded server; the agent's harness sees N entries in its
`.mcp.json`:

```json
{
  "mcpServers": {
    "repo": {
      "command": "lagom",
      "args": ["serve", "--config", "repo.toml", "--", "npx", "-y", "@some/repo-mcp"]
    },
    "search": {
      "command": "lagom",
      "args": ["serve", "--config", "search.toml", "--", "npx", "-y", "@some/search-mcp"]
    }
  }
}
```

Each `lagom serve` owns one upstream child and one policy. There is no single
config that fans out across several upstreams in 0.1.0 — composition *is* the
mechanism (one wrapper per upstream). When embedding a binding, the equivalent is
one `Guard` (or one `mint_stdio_server`) per upstream surface.

### Many agents — one binary, a different policy each

A policy is **data**, not a compiled artifact, so the *same* lagom binary/binding
serves every agent — each just gets a different `Policy`. An orchestrator mints a
per-agent projection at spawn (e.g. constrain `path` to the exact files that
agent may touch), with no rebuild:

```go
// One embedded engine; per-agent policy computed at dispatch time.
for _, agent := range agents {
    policyJSON := app.PolicyFor(agent)            // app's branding + per-agent scope
    g, err := lagom.NewGuard(ctx, upstreamJSON, policyJSON)
    // ... serve g.SlimDefs() / g.Gate(...) as this agent's slim MCP.
}
```

Per-agent **narrowing within sealed bounds** is enforced by `merge`/`mint`: an
integrator authors a base ceiling, and each agent's overlay may only narrow it
further (drop, tighten, pin) — any widening fails loud (`SPEC.md` §5.2). For
ephemeral per-agent runs that must be reproducible, mint a `MintRecord` and
refire it (next section). This is exactly how the first consumer (`sand`) slims
each spawned agent's MCP — see [`docs/SAND_LAGOM_HANDOFF.md`](docs/SAND_LAGOM_HANDOFF.md).

A **runnable proof** of this whole consumption story — a branded
[mcp-go](https://github.com/mark3labs/mcp-go) server with lagom invisible, two
ephemeral profiles yielding two slim surfaces from one binary, refire reproducing
a surface, and two upstreams composed for one agent — lives in
[`go/examples/`](go/examples) and runs via `just examples` (it drives everything
through the mcp-go in-process client).

---

## Ephemeral projections: mint & refire

An orchestrator mints a **per-agent ephemeral projection** at spawn — narrowed
for one agent and one run, then discarded — and records its **resolved policy +
provenance** as a `MintRecord` so the exact run can be **refired** (re-minted
into a byte-identical server) from failure or for audit (`SPEC.md` §8.2).
Minting invokes no LLM and reads nothing nondeterministic, so identical inputs
mint a byte-identical record.

### CLI

`lagom serve --audit <path>` persists a full `MintRecord` (resolved policy +
config-path provenance + upstream command) as the first line of the append-only
JSONL trace, alongside the original defs and every rewrite/rejection. `lagom
refire` re-mints that exact projection from the record — even if the source
`lagom.toml` has since changed:

```sh
# Serve, persisting a refirable trace tagged with a run id.
lagom serve --config lagom.toml --audit run.jsonl --run-id agent-7 -- npx -y @some/mcp-server

# Refire the recorded projection (reads the `mint` event from the trace, or a
# bare MintRecord JSON file). Trailing `--` args override the recorded upstream.
lagom refire --record run.jsonl --audit refire.jsonl
```

### Bindings (mint in code)

An app that builds policy as data (not `lagom.toml` files) mints in code from a
sealed **base** narrowed by a per-agent **dynamic** overlay (e.g. `path`
constrained to the files that agent may touch). The overlay may only narrow; a
widening attempt fails loud. The record is plain JSON — persist it anywhere and
hand it back to `refire`. Identical shape in every binding (NO DRIFT):

```go
// Go: mint a per-agent record, persist it, later refire the exact projection.
record, err := lagom.Mint(ctx, "agent-7", baseJSON, dynamicJSON, upstreamJSON)
resolved, err := lagom.Refire(ctx, record) // {"policy","upstream"} ready to serve
```

```python
record = lagom.mint("agent-7", base_json, dynamic_json, command="npx", args=["-y", "@some/mcp-server"])
resolved = lagom.refire(record)   # raises ValueError on a malformed record
```

```ts
const record = lagom.mint("agent-7", baseJson, dynamicJson, "npx", ["-y", "@some/mcp-server"]);
const resolved = lagom.refire(record); // throws on a malformed record
```

The Go/Python/Node `mint`/`refire` are pure (the wasm-clean core), so they run
in-process in every binding. Spawning the refired child stays native: the Go
binding hands `resolved` to the app's own server, while the CLI's `lagom refire`
spawns it directly.

---

## Build & test

The canonical gate is [`just`](https://just.systems):

```sh
just ci          # fmt-check + clippy -D warnings + test + build (the gate)
just py          # build + smoke-test the Python wheel (excluded from `just ci`)
just node-test   # build + smoke-test the Node/TypeScript addon (excluded)
just wasm        # build the wasm32 core + refresh the Go binding's embedded blob
just go-test     # in-process wazero tests for the Go binding (CGO_ENABLED=0)
just examples    # runnable mcp-go consumption proof (branded server, multi-agent
                 # ephemeral profiles, multi-mcp) via the mcp-go in-process client
just --list      # all recipes
```

The Python, Node, and Go bindings are excluded from `just ci` (each needs an
extra toolchain — a Python interpreter, the napi CLI, the wasm target) and are
built/tested by their own recipes above.

## License

[MIT](LICENSE).
