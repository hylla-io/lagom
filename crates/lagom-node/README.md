# lagom — Node.js / TypeScript binding

The [lagom](https://github.com/hylla-io/lagom) engine as a native Node addon
(via [napi-rs](https://napi.rs/)). lagom wraps an upstream MCP server and serves
a **narrowed projection** of it — drop tools, pin/hide args, constrain domains,
slim docs — so an agent sees and can do **just enough** (`SPEC.md` §1).

This binding is a thin, in-process skin over the single Rust core
(`lagom-core`); it is in **capability parity** with the Python and Go bindings —
the same JSON-in / JSON-out contract, byte-for-byte (NO DRIFT).

## Install

```sh
npm install @hylla-io/lagom
```

(or build from source: `just node-build`, which compiles the `.node` addon and
emits the typed `index.js` / `index.d.ts`.)

## The four pure operations

All take and return **JSON strings** (the upstream tool defs, the policy, and
tool calls), matching every other lagom binding. Engine rejections, merge
widening, and drift all throw a JavaScript `Error` carrying the engine's own
annotated message — never swallowed (`SPEC.md` §9.1).

```ts
import { project, rewrite, merge, validate, mint, refire, Guard, PolicyBuilder } from "@hylla-io/lagom";

// Author a sealed, allowlist projection with the typed builder.
const b = PolicyBuilder.sealed();   // drop every tool unless kept
b.keep("search");                   // allowlist `search`
b.pin("search", "artifact", JSON.stringify("hylla")); // fix + hide an arg
b.constrainEnum("search", "query", JSON.stringify(["a", "b"]));
const policy = b.build();            // -> Policy as a JSON string

// Forward transform: narrow the upstream `tools/list` surface.
const slim = JSON.parse(project(upstreamDefsJson, policy));
//   -> `search` only; its schema no longer mentions the pinned `artifact`.

// Inverse transform: re-inject the pin the agent never saw; reject bad calls.
const call = JSON.parse(
  rewrite(JSON.stringify({ name: "search", arguments: { query: "a" } }), policy),
);
//   -> call.arguments.artifact === "hylla"
//   query "z" -> throws: "argument `query` violates its constraint: ..."

// Narrow-only merge (the sandbox enforcement): widening throws.
const composed = merge(integratorBaseJson, endUserOverlayJson);

// Drift check against the live upstream surface: throws on a stale reference.
validate(policy, upstreamDefsJson);
```

### `PolicyBuilder`

| method | effect |
| --- | --- |
| `new PolicyBuilder()` | passthrough (keep everything unless narrowed) |
| `PolicyBuilder.sealed()` | drop everything unless explicitly `keep`-ed |
| `keep(tool)` / `dropTool(tool)` | presence |
| `rename(tool, name)` | expose `tool` under a different downstream name |
| `describe(tool, text)` | Tier-1 slim description override |
| `pin(tool, arg, valueJson)` | fix an arg + remove it from the schema |
| `default(tool, arg, valueJson)` | supply when omitted (stays visible) |
| `constrainEnum(tool, arg, valuesJson)` | restrict to an enum subset |
| `constrainRange(tool, arg, min?, max?)` | numeric range |
| `constrainPattern(tool, arg, pattern)` | regex pattern |
| `build()` | emit the `Policy` as a JSON string |

Value arguments are passed as JSON strings (`JSON.stringify(...)`), exactly like
the Python binding, so marshalling is identical across languages.

## The brandable one-call helper — `Guard`

When an app embeds lagom (rather than running the CLI), it wires a slim,
**branded** MCP in two calls with `Guard` — lagom stays invisible. All branding
(renamed tool names, override descriptions, dropped tools) lives in the `Policy`
the app supplies; the helper reads nothing itself.

```ts
// `policyJson` is the app's branded projection, built from the app's own config.
// Here: keep upstream `search` under the app's own name `find`, pin (hide)
// `artifact`, drop everything else.
const g = new Guard(upstreamDefsJson, policyJson); // throws on drift / malformed input

// Register g.slimDefs() as your server's tools/list — already shows `find`, not
// `search`; no `artifact`; no `lagom` anywhere.
const toolsList = JSON.parse(g.slimDefs());

// Gate each incoming tools/call: maps `find` -> `search`, injects the pin, or
// throws an annotated Error to relay to the agent.
const upstreamCall = JSON.parse(g.gate(incomingCallJson));
```

`new Guard(upstreamJson, policyJson)` mirrors `lagom_core::Guard` and the
Python/Go `Guard` exactly (NO DRIFT).

## Ephemeral mint & refire (`SPEC.md` §8.2)

Mint a per-agent projection from a sealed **base** narrowed by a per-agent
**dynamic** overlay, persist the record anywhere, and refire the exact
projection later — even after the source config has changed. Pure: identical
inputs mint a byte-identical record.

```ts
const record = mint("agent-7", baseJson, dynamicJson, "npx", ["-y", "@some/mcp-server"]);
// ... persist `record` (plain JSON string) wherever you like ...
const resolved = refire(record); // {"policy","upstream"} JSON, ready to serve; throws on a malformed record
```

The overlay may only **narrow** the base; any widening throws (the sandbox
enforcement, `SPEC.md` §5.2). The Node `mint`/`refire` are pure (the wasm-clean
core), in capability parity with the Go and Python bindings.

## Spawned-process face

`mintStdioServer(policyJson, command, args?, env?, audit?, runId?)` spawns the
upstream as a child, drift-validates, then bridges JSON-RPC over this process's
stdio, applying the projection — for agents that drive a child upstream over
stdio (`SPEC.md` §7.2, §8, §10). With `audit` set it writes an append-only JSONL
trace (`SPEC.md` §8.2, §9.3). It **blocks** until the session ends.

## Shipped skills

`shippedSkills()` returns the bundled lagom skill markdown as `[filename, body]`
tuples; `emitSkills(dir?)` writes them to disk — identical bytes to the CLI's
`lagom emit-skills` (`SPEC.md` §12). lagom never runs a model itself; the skills
are inert until a host loads them.

## Branding (lagom invisible)

An app embedding this binding picks its **own** server name, tool names
(`rename`), and slim docs (`describe`) — lagom never appears to the end-user. The
only place the name `lagom` shows is the `npm install @hylla-io/lagom` line; the
projected surface is fully the integrator's own (`docs/SAND_LAGOM_HANDOFF.md`
§3).
