# sand ⊃ lagom — findings, proof, and numbers (drop_016)

Durable, shareable record of sand consuming lagom as a Go dependency to slim each
dispatched agent's MCP surface. Everything below is reproducible from this repo
(commands in each section). Intended to hand to the lagom team so lagom can use
sand to prove its own concepts end-to-end.

- sand commit range: `2d2f40c … HEAD` (this branch, `github.com/evanmschultz/sand@main`).
- lagom consumed: `github.com/hylla-io/lagom/go` pinned `a7a748e` (a direct require; pure Go, only transitive dep wazero).
- Host: macOS (Darwin 25), codex-cli 0.139.0, node v26.3.0, Anthropic `count_tokens` (free) model `claude-haiku-4-5-20251001`.

---

## 1. What sand built on top of lagom

- `internal/slimmcp/projector.go` — `MapUpstreamDefs` maps MCP `inputSchema` (camel) → lagom `input_schema` (snake), the #1 consumer gotcha. Plus the `Upstream`/`slimDef`/`upstreamCall` data seam.
- `internal/slimmcp/server.go` — `NewBrandedServer` wraps a `lagom.NewGuard` into a real mcp-go server: `SlimDefs()` → tools/list, every call gated by `Gate()` first. lagom is invisible; all branding lives in the policy. Copied from lagom `go/examples/branded.go`.
- `internal/slimmcp/profile.go` — the ephemeral per-agent profile (`server_name`, `upstream`{command,args,env}, `policy`) + `LoadProfile`.
- `internal/slimmcp/upstream.go` (+ `reaper_unix.go`/`reaper_other.go`) — `DialUpstream` spawns the REAL upstream MCP over stdio (mcp-go stdio client, `Setpgid` process group), probes its tools/list, and returns a closer that Closes the client AND force-kills the process group (POC finding: harnesses leak MCP children).
- `cmd/sand/main.go` — `sand mcp --profile <json>` subcommand: load profile → dial+wrap upstream → serve the slim branded server over stdio → reap on stdin-EOF or SIGINT/SIGTERM.

Gates: `mage ci` green throughout (race/vet/gofumpt/tidy; 85.4% total, slimmcp 82.2%).

---

## 2. PROOF — real headless codex agent confined to `sand mcp` (GREEN ×2)

Harness: `bin/sand-codex-e2e.sh` (wraps node `fast-mcp.js`: tools `echo`+`secret`; policy keeps all, **drops `secret`**, **pins `echo.token=LOCKED`**). Reproduce:

```sh
mage install                      # ensure `sand mcp` is current
bash bin/sand-codex-e2e.sh        # real codex exec, ~50k tokens
```

Captured agent output (verbatim, exit 0):

```json
{"mcp_tools":["echo"],"tried_secret":"ERROR: secret tool is not available","called_echo":"echo:{\"message\":\"hi\",\"token\":\"LOCKED\"}"}
```

All four proofs GREEN:
1. **Slim surface** — the agent sees ONLY `echo`; `secret` is absent from its tool list.
2. **Dropped → unavailable** — calling `secret` returns "secret tool is not available".
3. **Pin injected** — the agent sent only `message:hi`; the upstream received `token:LOCKED` it never set.
4. **No leak** — after exit, no `sand mcp`/`fast-mcp` processes remain (process group reaped).

lagom is invisible end-to-end: the agent's server is named "guarded"; the word "lagom" never appears.

---

## 3. NUMBERS — tool-surface token savings

### 3.1 Demo servers (real npm MCP servers, via `sand mcp`)
`bench/sand_bench.py` → `bench/REPORT.md`:

| upstream | tools full→slim | full tok | slim tok | saved |
|---|---|---|---|---|
| everything | 13→2 | 1812 | 689 | 62.0% |
| filesystem | 14→2 | 2314 | 747 | 67.7% |
| memory | 9→2 | 1534 | 792 | 48.4% |

**Total: 54.8% saved** across 4 upstreams (matches lagom's measured ~52%; confirms the savings hold through sand's shipped binary).

### 3.2 Per-role on the REAL upstreams sand pays for (ta + hylla)
`bench/sand_role_bench.py` → `bench/role_REPORT.md`. Full surface an agent loads = ta (9 tools, 4790 tok) + hylla (16 tools, 4387 tok) = **9177 tool tokens per turn**.

| role | full tok (ta+hylla) | slim tok | saved/turn |
|---|---|---|---|
| planner | 9177 | 6625 | 27.8% |
| builder | 9177 | 3112 | 66.1% |
| qa | 9177 | 3222 | 64.9% |
| closeout | 9177 | 4390 | 52.2% |

**Cost framing (tool defs are re-sent on EVERY turn of EVERY agent):** avg ~4839 tok saved/turn. An illustrative cascade (12 agents × 15 turns) → ~871K input tokens saved/cascade (≈ $2.61 at $3/Mtok input, before output/caching). Reproduce:

```sh
python3 bench/sand_bench.py          # demo servers
python3 bench/sand_role_bench.py     # ta+hylla per-role (needs ANTHROPIC_API_KEY)
```

Raw captured surfaces + the exact profiles are in `bench/raw/` (shareable + diffable).

**The undeniable version (TODO):** A/B one real cascade — full MCP vs sand-slimmed MCP — comparing billed input tokens from the dispatch trace. Planned next.

---

## 4. Findings / issues surfaced (for sand + lagom)

- **lagom DX [RESOLVED in lagom core, post-`a7a748e`]:** `ToolDef` used to require snake `input_schema` while MCP emits camel `inputSchema` — every consumer mapped per tool. lagom core now accepts **both** casings (serde `alias`) and normalizes missing/`null` schema to `{}` (`crates/lagom-core/src/tooldef.rs`, tests in same file). Consumers can pass the raw upstream `tools/list` straight to `Project`/`NewGuard`; output stays canonical snake. **Action for sand:** bump the lagom pin to the commit carrying this change, then delete `MapUpstreamDefs` (the shim is now dead code).
- **Anthropic tools API rejects dotted tool names** (`hylla.artifact.list`) — `^[a-zA-Z0-9_-]{1,64}$`. Relevant to anyone token-measuring real MCP surfaces; the bench sanitizes names for counting only.
- **sand-side latent bug caught pre-consumer:** marshalling a `[]byte` schema inside a `map[string]any` base64-encodes it (the common path, since mcp-go's client populates `InputSchema` not `RawInputSchema`). Fixed; hermetic test now guards it.
- **Dogfood — sand's OWN dispatch (DONE):** a `sand(codex-exec gpt-5.5)` qa-falsification ran over `internal/slimmcp` via `mcp__sand__dispatch`; the trace returned fully populated (36 tool calls, all read-only, zero git/edit mutations — gate held). It independently confirmed three defects, now FIXED (commit `d302f44`): (1) `DialUpstream` startup timeout (30s bounded context on Initialize+ListTools); (2) `MapUpstreamDefs` now preserves the schema verbatim and normalizes absent/JSON-null/empty `inputSchema` to `{}`; (3) `LoadProfile` rejects non-object/array policies. `mage ci` green (85.6%).
- **Open sand items (tracked):** upstream env isolation (decision); the per-builtin-agent bash gate did not confine raw `go test` (read-only here — gate-hardening TODO); A/B real-cascade billed-token proof; cross-project `sand mcp` adoption; docs (SAND-SPEC/README) catch-up.

---

## 5. Reproduce everything

```sh
GOPRIVATE='github.com/hylla-io/*' go get github.com/hylla-io/lagom/go@a7a748e
mage install
mage ci                              # gates green
mage testPkg ./internal/slimmcp      # in-process + hermetic stdio e2e
bash bin/sand-codex-e2e.sh           # real codex confinement proof
python3 bench/sand_bench.py          # demo token numbers
python3 bench/sand_role_bench.py     # real ta+hylla per-role numbers
```
