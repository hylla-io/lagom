# lagom

**lagom** takes an existing MCP server and serves a *narrowed* view of it —
fewer tools, pinned or hidden arguments, constrained domains, slimmer tool docs —
so an agent sees and can do **just enough** (Swedish *lagom*) for its job:
nothing more to abuse, nothing extra to bloat its context.

It is a thin, transport-less Rust core behind multiple **faces**:

- a standalone **CLI** that runs an out-of-process stdio proxy — a drop-in for
  any harness that reads `.mcp.json` / `settings.json` (Claude Code, etc.);
- a **Python binding** (PyO3/maturin) shipping the compiled core as a normal
  project dependency, for in-process and spawned-process agents.

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
import lagom  # exposes project, rewrite, merge, validate, mint_stdio_server,
               # PolicyBuilder, shipped_skills, emit_skills
```

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

## Build & test

The canonical gate is [`just`](https://just.systems):

```sh
just ci          # fmt-check + clippy -D warnings + test + build (the gate)
just py          # build + smoke-test the Python wheel (excluded from `just ci`)
just --list      # all recipes
```

## License

[MIT](LICENSE).
