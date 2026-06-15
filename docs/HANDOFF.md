# lagom — project handoff / current state (durable)

Single source of "where we are" for a fresh session or the sand team. Pairs with
`SPEC.md` (design), `CONTEXT.md` (glossary), `FEATURES.md` (feature inventory),
`docs/adr/` (decisions), `docs/SAND_ORCH_PROMPT.md` (sand design brief),
`docs/SAND_GETTING_STARTED.md` (sand consume + e2e runbook),
`docs/SAND_LAGOM_HANDOFF.md` (caveman contract), `docs/POC_FINDINGS.md`
(real-agent results), `bench/` (real token numbers), `bin/lagom-codex-poc.sh`
(working codex confinement run), `go/examples/branded.go` (consumer pattern).

## ON RESUME (lossless pickup)

1. Read this file. 2. `git log --oneline -10` for the latest commits (HEAD below
may lag). 3. `just ci && just go-test && just node-test && just py-test &&
just parity && just examples` to confirm everything is still green. 4. Durable
behavioral rules are in `~/.claude` memory (`binding-parity-and-docs`,
`always-real-benchmarks`, `lagom-go-get-consumption`) + this repo's `CLAUDE.md`.
The reproducible techniques are encoded in `bin/*.sh` + `bench/*.py` — run them,
don't reconstruct them.

## STATUS (2026-06-15)

- lagom **v0.1.0, UNRELEASED**. Pushed to `main` at **`be8cbd3`** (or later — `git log`) (`github.com/hylla-io/lagom`, private).
- One Rust core (`lagom-core`) + faces: **CLI**, **Python** (PyO3), **Go** (wasm+wazero, `go get`), **TS/Node** (napi-rs).
- All 6 gates green (verified): `just ci`, `just go-test`, `just node-test`, `just py-test`, `just parity` (zero drift), `just examples`.
- Real token savings (Anthropic `count_tokens`, logged in `bench/`): **~52% avg** tool-surface reduction across real MCP servers (everything −64%, filesystem −67%, memory −50%, sequential-thinking −43% via caveman docs).

## HOW SAND CONSUMES lagom (no release needed)

```sh
GOPRIVATE='github.com/hylla-io/*' go get github.com/hylla-io/lagom/go@main   # or @04c7e5a
```
```go
import lagom "github.com/hylla-io/lagom/go"
g, _ := lagom.NewGuard(ctx, fullToolDefsJSON, policyJSON) // policy = sand's branded projection
slim := g.SlimDefs()              // -> register as your mcp-go server's tools/list
up, err := g.Gate(ctx, callJSON)  // -> rewritten upstream call, or err (rejected)
```
Pattern to copy: `go/examples/branded.go` (mcp-go server gated by lagom-go, proven e2e). lagom-go deps = ONLY wazero (no cgo). lagom stays invisible: sand picks its own server/tool names + descriptions in the policy.

## PROVEN (real, reproducible)

- lagom slims correctly under clean env (echo kept, secret/get-sum dropped) — direct `lagom serve` probe.
- **codex exec e2e GREEN** (`bin/lagom-codex-poc.sh`): real headless agent sees ONLY the slim tool, "secret tool not available", pinned `token=LOCKED` injected (agent never set it), clean teardown. codex connects MCP synchronously — the proven headless confined vehicle.
- `go get` consumption proven (lagom-demo pulled a real pushed commit; mcp-go e2e green).
- Token savings real + logged (`bench/results.jsonl|.csv|REPORT.md|raw/`, chart `bench/savings.svg`, re-run `python3 bench/bench.py`).

## VEHICLE GUIDANCE (for sand)

- **codex exec** = headless confined vehicle (sync MCP). Use hermetic CODEX_HOME + `approval_policy="never"` + per-tool `approval_mode="approve"` + `--ignore-user-config` + `project_doc_max_bytes=0` + `skills.bundled.enabled=false`; lagom-wrapped server under `mcp_servers.<name>`.
- **claude -p**: loads `--mcp-config`/discovered MCP ASYNC; a trivial one-shot turn fires before connect (race). Works with a big persona + project CLAUDE.md (delays the turn) OR the in-session built-in Agent tool. `--settings` deny is NOT enforced in `-p` (use a PreToolUse hook). Clean HOME needs an API key (OAuth isn't file-based). codex is simpler; prefer it for headless.
- ALWAYS: ephemeral per-agent profile + DENY every escape tool (Bash/Read/Write/Edit/Grep/Glob/Task/Agent/Workflow/ToolSearch/…) + `--strict-mcp-config` + **dispatcher kills the process tree on exit** (the harness leaks MCP children).

## WHAT'S LEFT (tracked)

1. ~~Go `PolicyBuilder`~~ **DONE** (`go/builder.go` + tests; parity with py/ts). lagom-go now has full API parity. NOTE for sand: lagom's `ToolDef` field is `input_schema` (snake_case); MCP servers emit `inputSchema` — map it before `Project`/`NewGuard` (the `go/examples/branded.go` helper shows the one-line mapping). Candidate DX improvement: alias `inputSchema` in core `ToolDef`.
2. **Per-binding real-MCP e2e + real numbers** for Python and TS (Go is proven). HOW: feed the real captured tool surfaces in `bench/raw/*.full.json` through `lagom-py.project` and `lagom-node.project`, assert byte-identical slim to the Rust/Go output (parity), and record token numbers per binding in `bench/`. (The `just parity` harness already proves byte-identical on synthetic fixtures; this extends it to real surfaces.)
3. **Joint sand e2e**: once sand `go get`s lagom and stands up `sand mcp --profile`, run the real codex/agent E2E *through sand*, re-verify parity + correctness across binary + Go/Py/TS + Rust-lib.
4. **Release**: tag `v0.1.0` ONLY on the user's express word, AFTER full e2e proof. Until then, sand pins a pseudo-version (`@main`/`@<commit>`).

## ENV / GATES (gotchas)

- Bare `go`, `python3 -c`, `sed`, `awk`, `tee`, `rm -rf`, `git` (for subagents) are blocked by the action gate — route through `just` recipes or the `!` shell prefix.
- Build gate = `just ci` (Rust workspace; lagom-py/lagom-node/lagom-wasm excluded, gated by `just py*`/`just node-*`/`just wasm`+`go-test`). CI mirrors all jobs.
- graphify is an MCP (`mcp__graphify__*`) + CLI over `graphify-out/graph.json` (gitignored); refresh with `graphify update .`.

## DURABLE RULES (in ~/.claude memory + CLAUDE.md)

- **NO DRIFT**: all bindings stay in capability parity (Go = canonical full-test); docs full/accurate/current on every surface.
- **REAL DATA ONLY**: perf/savings claims backed by measured data logged to `bench/`; graphs from data; never hand-wave.
- orch never mutates sibling git except on the user's explicit instruction; never release without express word.
