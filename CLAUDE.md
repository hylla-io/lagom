# CLAUDE.md — project guidance for lagom

Project-local guidance for working inside the `lagom` tree. Global rules (Tillsyn
coordination, Section 0 reasoning, evidence sources, worktree hygiene, output
style) live at `~/.claude/CLAUDE.md` and are NOT duplicated here.

lagom is a **Rust** project: a transport-less core (`lagom-core`) behind multiple
faces (CLI, Python binding, and a wasm/Go binding via wazero). See
[`SPEC.md`](SPEC.md) for the design,
[`CONTEXT.md`](CONTEXT.md) for the canonical glossary, and
[`docs/adr/`](docs/adr/) for architecture rationale. ADR-0002 superseded the
original Go/mage bootstrap — this file reflects the Rust reality.

## Build gate — `just ci`

The canonical gate is **`just ci`**: `cargo fmt --all --check` + `cargo clippy
--all-targets --all-features -D warnings` + `cargo test --all` + `cargo build
--all`. Run it before any build action item is marked complete; work is not done
until it is green.

- Prefer the `just` recipes over raw cargo so the gate name is stable:
  `just fmt`, `just fmt-check`, `just clippy`, `just test`, `just build`,
  `just ci`. `just --list` for the full set.
- You MAY run cargo/just freely (read or mutate the working tree); they are not
  git mutations.
- The **Python binding** (`lagom-py`) is a pyo3 `extension-module` cdylib and is
  **excluded** from the workspace build (`Cargo.toml`), so `just ci` does not
  cover it. Build/lint/test it separately: `just py` (maturin wheel + smoke
  test), `just py-check` (fmt + clippy), `just py-test` (rlib unit tests). These
  need `maturin` and `uv`. lagom-py runs in a dedicated `python` CI job.
- The **wasm/Go binding** (`lagom-wasm` crate + `go/` module) is likewise
  **excluded** from the workspace (its memory ABI is only meaningful on wasm32),
  so `just ci` does not cover it. Build/test it separately: `just wasm` (build the
  wasm32 face via the rustup `stable` toolchain and refresh the committed
  `go/internal/wasmbin/lagom.wasm`), `just go-test` (the Go binding's in-process
  wazero tests under `CGO_ENABLED=0`). `just wasm` needs
  `rustup target add wasm32-unknown-unknown`. NOTE: `lagom-wasm`/`lagom-go` are
  not yet in a CI job — see the KNOWN GAPS section of `FEATURES.md`.

## Rust development rules

- **Hexagonal / dependency-inverted**: everything depends on the pure
  `lagom-core`; nothing pure depends on transport (ADR-0001). The stdio proxy
  and CLI are thin adapters. Do not reach transport types into the core.
- **`lagom-core` is DONE and green** — `Policy`, `project`, `rewrite`, `merge`,
  `validate`. Depend on it; do not reimplement or edit it except to add it as a
  dependency.
- **Smallest concrete design.** No abstraction for hypothetical future variation.
- **Idiomatic Rust** — naming, module structure, import grouping (std /
  third-party / local), errors wrapped with `thiserror` and bubbled at clean
  boundaries; never swallow (`SPEC.md` §9.1).
- **Doc comments** (`///`) on every public item; module docs (`//!`) cite the
  relevant `SPEC.md` section.
- **Tests**: co-located `#[cfg(test)]` modules, table-driven and
  behavior-oriented. TDD where practical; ship small tested increments.

## Binding parity & docs — NO DRIFT

`lagom-core` is the single source of behavior; every face (CLI, Python, Go/wasm,
TS) is a thin skin over it. **Go is the canonical full-test surface** — it gets
the deepest e2e/battle tests. After ANY behavior/capability change or finding on
the Go side, **propagate it to the Python and TS bindings and the core** so all
bindings stay in **capability parity — zero drift**. A capability/behavior present
in one binding but missing in another is a **bug**, not a backlog item.

A **TS binding** (napi-rs) is required and smoke-tested the same way as Python
(build the artifact, import it, assert a `project()` drop + pin).

Docs must be **full, accurate, and current across every surface at all times**:
rustdoc (`///`), `go doc`, Python docstrings, TS types/JSDoc, and the README
(CLI + each binding's `go get`/`pip`/`npm` usage + the branding pattern). No
binding is "done" until it is parity-tested AND doc-complete.

## Benchmarks & claims — REAL data only

Any performance / token-savings / comparison claim MUST be backed by **real
measurement with real instruments** and **logged to a reproducible store** — never
asserted. Measure with the actual tokenizer (Anthropic `count_tokens`), real MCP
servers, and real agents. The pattern is `bench/`: a re-runnable harness
(`bench.py`) that writes `results.jsonl` (every data point + exact inputs/policy/
method), `results.csv` (graph-ready), `REPORT.md` (table + totals + methodology),
`raw/` (captured surfaces), and a chart generated FROM the logged data. Graphs are
generated from the data file, not by hand. No claim ships without a re-run path.

## Evidence sources (Rust)

Hylla and mage do not apply here (Hylla is Go-only; ADR-0002 dropped both).
Evidence order for Rust work:

1. **graphify** for codebase understanding/review — run `graphify update .` to
   refresh the graph, then the graphify MCP tools (`mcp__graphify__query_graph`,
   `get_node`, `get_neighbors`, `shortest_path`, `god_nodes`) or the
   `graphify query/explain/affected` CLI.
2. **rust-analyzer** via the `LSP` tool for symbol-level semantics.
3. `git diff` for uncommitted deltas; Read/Grep/Glob for non-Rust and post-edit
   pre-commit Rust.
4. **Context7** + `cargo doc` for external crate semantics.

## Work tracking — `ta`

Cascade / work tracking uses the **`ta` MCP** (`mcp__ta__*` on `.ta/`-managed
records) — language-agnostic, retained across the Rust migration. `ta` records
are the durable source of truth; built-in `TaskCreate`/`TaskUpdate` are fine for
granular sub-steps. NEVER the `tillsyn` MCP (only the tillsyn repo has it wired).

- All `ta <read-command>` invocations from dispatched roles MUST pass `--json`
  (`ta get`, `ta list-sections`, `ta schema`, `ta search`).
- The `ta` MCP server pins one project per process: launch Claude Code from the
  active checkout, or pass `--project <abs-path>` in the MCP server invocation.

There is no `ta-go-*` agent matrix here — those run the Go toolchain. Rust build
work uses general-purpose (or future Rust-flavored) agents against `just ci`.

## Cascade methodology — plan down, build up

Canonical contract: [`CASCADE_METHODOLOGY.md`](CASCADE_METHODOLOGY.md) (consumed
from tillsyn, the methodology source). Key invariants: plan top-down / build
bottom-up; recurse on atomicity (1-2 small code blocks, ≤80 LOC incl. tests, ≤3
files per build droplet); per-branch parallelism (only `blocked_by` serializes);
a plan-QA descent gate (proof + falsification) before a node spawns children;
**droplet-level QA = the `just ci` gate** (no per-droplet LLM proof); orch
auto-advances.

## Git discipline

Per `feedback_no_sibling_git_mutations`, orch never runs git here — the dev owns
git history. Do not commit/push/add/tag/publish from dispatched roles.
