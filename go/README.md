# lagom — Go binding

The [lagom](https://github.com/hylla-io/lagom) engine as an in-process Go
dependency. lagom wraps an upstream MCP server and serves a **narrowed
projection** of it — drop tools, pin/hide args, constrain domains, slim docs — so
an agent sees and can do **just enough** (`SPEC.md` §1).

The Rust core (`lagom-core`) is compiled to wasm and run in-process via
[wazero](https://wazero.io) — **pure Go, no cgo, no separate binary**. The
`.wasm` is `go:embed`-ed, so a single `go get` delivers the whole engine. This
binding is in **capability parity** with the Python and Node bindings — the same
JSON-in / JSON-out contract, byte-for-byte (NO DRIFT). It is the first
consumer's (`sand`) language; see [`../docs/SAND_LAGOM_HANDOFF.md`](../docs/SAND_LAGOM_HANDOFF.md).

## Install (`go get`)

```sh
go get github.com/hylla-io/lagom/go
```

```go
import lagom "github.com/hylla-io/lagom/go"
```

`wazero` is the only non-stdlib dependency. The wasm engine is embedded in your
binary — nothing is read from disk at runtime, and no upstream process is spawned
by this binding (spawning/stdio stay native: the CLI and the Python/Node
`mint_stdio_server`).

### Private module — `GOPRIVATE`

Until the GitHub repo is public, point the toolchain away from the public proxy
and checksum DB and let `git` authenticate to the private repo:

```sh
go env -w GOPRIVATE=github.com/hylla-io/*
git config --global url."git@github.com:hylla-io/".insteadOf "https://github.com/hylla-io/"
```

## The four pure operations

All take and return **JSON** (`[]byte`): the upstream tool defs, the policy, and
tool calls, matching every other lagom binding. An engine reject, a widening
merge, or a drift failure surfaces as a Go `error` carrying the engine's own
annotated message — never swallowed (`SPEC.md` §9.1).

```go
ctx := context.Background()

// Forward transform: narrow the upstream tools/list surface through a policy.
slim, err := lagom.Project(ctx, upstreamDefsJSON, policyJSON)

// Inverse transform: re-inject pins the agent never saw; reject bad calls.
upstreamCall, err := lagom.Rewrite(ctx, incomingCallJSON, policyJSON)

// Narrow-only merge (the sandbox enforcement): widening returns an error.
composed, err := lagom.Merge(ctx, integratorBaseJSON, endUserOverlayJSON)

// Drift check against the live upstream surface: error on a stale reference.
err = lagom.Validate(ctx, policyJSON, upstreamDefsJSON)
```

The package-level functions use a lazily-initialized shared `Engine`. Construct
dedicated `Engine`s with `lagom.New(ctx)` for parallelism (each owns one wasm
linear memory, so its calls are serialized under a mutex).

## Typed policy authoring — `PolicyBuilder`

Build a `Policy` fluently in Go (parity with the Python/TS builders):

```go
pol, _ := lagom.SealedPolicyBuilder(). // drop everything unless kept
	Keep("search").
	Rename("search", "find").             // expose under your own name
	Describe("find", "search the project").// Tier-1 slim description
	Pin("search", "artifact", "hylla").    // fix + hide an arg
	ConstrainEnum("search", "query", []any{"a", "b"}).
	Build()                                // -> Policy JSON for Project/NewGuard/…
```

`NewPolicyBuilder()` starts in passthrough mode (keep all). You can also build the
Policy JSON directly — the lib reads whatever you pass.

## The brandable one-call helper — `Guard`

When an app embeds lagom (rather than running the CLI), it wires a slim,
**branded** MCP in two calls with `Guard` — lagom stays invisible. All branding
(renamed tool names, override descriptions, dropped tools) lives in the `Policy`
the app supplies; the helper reads nothing itself.

```go
// `policyJSON` is the app's branded projection, built from the app's own config.
// Here: keep upstream `search` under the app's own name `find`, pin (hide)
// `artifact`, drop everything else.
g, err := lagom.NewGuard(ctx, upstreamDefsJSON, policyJSON)
if err != nil { /* drift / malformed input — fail loud */ }

// Register g.SlimDefs() as your server's tools/list — already shows `find`, not
// `search`; no `artifact`; no `lagom` anywhere.
toolsList := g.SlimDefs()

// Gate each incoming tools/call: maps `find` -> `search`, injects the pin, or
// returns an annotated error to relay to the agent.
upstreamCall, err := g.Gate(ctx, incomingCallJSON)
```

Use `lagom.NewGuardWith(ctx, engine, ...)` to bind a dedicated `Engine`.

## Ephemeral mint & refire (`SPEC.md` §8.2)

Mint a per-agent projection from a sealed **base** narrowed by a per-agent
**dynamic** overlay, persist the record anywhere, and refire the exact
projection later — even after the source config has changed. Pure: identical
inputs mint a byte-identical record.

```go
record, err := lagom.Mint(ctx, "agent-7", baseJSON, dynamicJSON, upstreamJSON)
// ... persist `record` (plain JSON) wherever you like ...
resolved, err := lagom.Refire(ctx, record) // {"policy","upstream"} ready to serve
```

The overlay may only **narrow** the base; any widening returns an error (the
sandbox enforcement, `SPEC.md` §5.2).

## Branding (lagom invisible)

An app embedding this binding picks its **own** server name, tool names
(`rename`), and slim docs (`description` override). The only place the name
`lagom` shows is `go.mod`; the projected surface is fully the integrator's own
(`../docs/SAND_LAGOM_HANDOFF.md` §3).

## Many upstreams, many agents

- **Many upstreams for one agent:** build one `Guard` (or one
  `mint_stdio_server` on the CLI/Python/Node side) per upstream surface, and
  register each as its own slim server. Composition is the mechanism.
- **Many agents:** a `Policy` is data, so the *same* embedded engine serves every
  agent — each gets a different policy (e.g. `path` scoped to the files that
  agent may touch) with no rebuild.

## Worked example — a branded mcp-go server ([`examples/`](examples))

[`examples/`](examples) is a runnable, end-to-end proof of the first consumer's
(`sand`) story, built on [mcp-go](https://github.com/mark3labs/mcp-go) and driven
through its **in-process** client/server (no stdio, no network). It lives in its
own Go module so this binding keeps `wazero` as its only non-stdlib dependency.

- [`branded.go`](examples/branded.go) — `NewBrandedServer` turns a lagom `Guard`
  into a real mcp-go server: `Guard.SlimDefs()` becomes the registered
  `tools/list` (the app's own names + docs, the pinned args gone), and every
  incoming `tools/call` is `Guard.Gate()`-d back to the upstream — **lagom is
  invisible to the agent**, and a gate rejection rides back as an annotated mcp
  tool-error (never swallowed).
- [`profiles.go`](examples/profiles.go) — one sealed branded **base** policy over
  a repo upstream, and two per-agent **ephemeral profiles** (`reader`, `writer`)
  minted from it with `lagom.Mint` — proving a policy is *data*: the same binary
  yields two different slim surfaces.
- [`branded_test.go`](examples/branded_test.go) — drives all of it through the
  mcp-go in-process client and asserts: the branded surface hides `lagom` and the
  upstream names; pins are injected and constraints enforced; the two profiles
  produce two different surfaces (multi-agent); `Refire` reproduces a profile's
  surface byte-for-byte; and two upstreams each wrapped compose for one agent
  (multi-mcp).

```sh
just examples  # go vet + in-process mcp-go tests (CGO_ENABLED=0), against the
               # committed lagom.wasm (refresh it first with `just wasm`)
```

## Test

```sh
just wasm      # rebuild the embedded engine from lagom-core (refresh lagom.wasm)
just go-test   # in-process wazero tests (CGO_ENABLED=0 — proves the no-cgo path)
```
