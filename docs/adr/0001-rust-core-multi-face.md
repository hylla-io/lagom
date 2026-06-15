# Rust core with multiple delivery faces

Status: accepted

lagom is **one Rust core** — a transport-less, side-effect-free transform +
policy engine that turns a full upstream MCP tool surface into a narrowed
*projection* — delivered through multiple **faces**, none primary:

- a standalone **CLI** binary: an out-of-process stdio proxy, a harness-agnostic
  drop-in for `.mcp.json` / `settings.json` (wraps servers for harnesses we
  don't control, e.g. Claude Code);
- an embeddable **binding** per language (Python wheel, Node addon, Rust crate,
  Go module): the compiled core shipped *as a normal project dependency* — no
  separately-installed binary — exposing both `project()`/`rewrite()` (for
  in-process agents) and `mint_stdio_server()` (for spawned agent processes).

## Considered Options

- **Go binary (workspace fit).** Rejected: Go produces a fine standalone binary
  but cannot be embedded into Python/Node/Rust as a native dependency. Under the
  "lib as a project dep, no separate binary" requirement this is disqualifying.
- **In-process library, reimplemented per language.** Rejected: 4× the
  implementation and maintenance of the transform engine; guarantees drift
  between implementations.
- **Rust core via FFI to each language.** Accepted as the spine. Rust is
  best-in-class for this: PyO3/maturin (Python), napi-rs (Node), native (Rust).

## Consequences

- **Go is the problem child for the binding.** Native embedding needs cgo + a C
  toolchain + per-platform archives, which breaks `go get`. Mitigation: compile
  the core to **wasm** and run it via `wazero` (pure-Go host, no cgo). Wasm
  doubles as a universal embed target for the *transport-less* core.
- **Wasm is pure compute** — it cannot spawn the upstream child process or do
  stdio. So `mint_stdio_server()` / the CLI stay native; the wasm path serves
  only the `project()`/`rewrite()` (transport-less) face.
- The transport-less core is **foundational**; the stdio server and CLI are thin
  adapters over it. Building stdio-first would entangle I/O into the core and
  forfeit the wasm-embeddable lib — so the dependency direction is fixed:
  everything depends on the pure core, nothing pure depends on transport.
