# lagom proven end-to-end THROUGH sand (a real consumer + real agents)

Durable, reproducible record of lagom's limitations proven **only through sand**
(the first real `lagom-go` consumer) — never by calling lagom directly. The
agent-facing server is always `sand mcp --profile` (lagom *inside* — invisible in
names/schemas; a description + error brand-leak was found and is fixed/routed, see
Findings);
the evidence is each agent's own transcript + a protocol-level wire probe, not the
agent's self-report. Pairs with `SAND_LAGOM_FINDINGS.md` (sand's side) — same
tool/field names + the same `count_tokens` method so numbers are comparable.

- lagom: this repo; the `lagom-go` binding sand consumes (wasm+wazero, no cgo).
- sand: `/Users/evanschultz/Documents/Code/hylla/sand/main` (`sand mcp --profile`),
  pinned `lagom-go@a7a748e`. The slimming concepts below are version-independent
  (the post-`a7a748e` `inputSchema` alias changes input *format*, not slimming), so
  these proofs hold for both pins.
- Host: macOS (Darwin 25), codex-cli 0.139.0, claude 2.1.177, node v26, Anthropic
  `count_tokens` model `claude-haiku-4-5-20251001`.
- Vehicles: **codex exec** (sync MCP — the clean headless vehicle) and **claude -p**
  (async MCP connect; see the finding). Everything spawned via the `sand`/`codex`/
  `claude` CLIs through Bash — no MCP wired into the orchestrator.

## Harness (`e2e/bin/`)

- `run-codex.sh` / `run-codex-multi.sh` — confine a real codex agent to one/two
  `sand mcp` servers; capture the full `--json` event stream (every mcp tool call
  + result) and the final message; reap the process tree.
- `run-claude.sh` — same for `claude -p` (stream-json + verbose, `--strict-mcp-config`).
- `wire-probe.sh` — raw MCP `initialize`→`tools/list` straight to `sand mcp`:
  protocol-level proof of the exact slim surface, agent-independent.
- `wire-call.sh` — raw MCP `tools/call` to `sand mcp`: proves pin injection +
  constraint rejection with no agent involved.
- `extract_codex.py` / `extract_claude.py` — distill a transcript to verifiable
  facts (servers, tool calls + args, result text, usage).
- `resolve.sh` — substitute node/upstream paths into a profile template.

## Concepts proven (all GREEN, through sand only)

| # | concept | evidence (vehicle) | result | artifacts |
|---|---|---|---|---|
| 1 | DROP | wire + codex + claude | `secret` absent from surface; agent gets "not available" | `e2e/runs/c1-drop-pin/` |
| 2 | RENAME / brand | codex (c1 `guarded`; c6 `ping`/`reveal`) | agent sees brand names, never upstream names (brand leaked in descriptions/errors — fixed/routed, see Findings) | `e2e/runs/c1-*`, `c6-multi/` |
| 3 | PIN | wire + codex | agent sends none; upstream receives `token=LOCKED` / `A1` / `key=B2` | `e2e/runs/c1-*`, `c4-*`, `c6-*` |
| 4 | CONSTRAIN | wire-call | `message:hi`→ok; `message:NOPE`→**rejected** (`isError`), never forwarded | `e2e/runs/c4-constrain/` |
| 5 | SEALED | wire | `default_presence=drop` → only the allowlist offered | `e2e/runs/c5-sealed/` |
| 6 | MULTI-UPSTREAM | codex | two `sand` servers (`alpha`+`beta`) compose into one toolset | `e2e/runs/c6-multi/` |
| 7 | REFIRE determinism | wire ×2 | same profile re-served → byte-identical slim surface | `e2e/runs/c7-refire/` |

Representative transcript facts (extracted, not self-reported):

- **c1 codex** — `mcp_tool_call{server:"guarded", tool:"echo", arguments:{message:"hi"}}`
  → `result:"echo:{\"message\":\"hi\",\"token\":\"LOCKED\"}"`. Agent never sent
  `token`; lagom injected it. `secret` never callable. usage input 58,708.
- **c6 codex** — calls `alpha/ping{message:"hi"}`→`token:"A1"` and
  `beta/reveal{}`→`key:"B2"` (both renamed upstream tools, both pins injected).
- **c4 wire** — `echo{message:"NOPE"}` →
  `"lagom: rewrite: argument \`message\` violates its constraint: \`message\` ∈ {hi, bye}"`, `isError:true`.

## claude -p finding (we checked it, not just codex)

claude -p **does** confine to the lagom-slimmed surface in 2.1.177 — but via a
different path than codex:

- At turn start the transcript shows `mcp_servers:[{guarded, status:"pending"}]`
  and **zero** mcp tools — the documented async-connect race is real.
- This claude version exposes MCP tools as *deferred/searchable*. The agent ran
  `ToolSearch` (`+guarded`, `select:mcp__guarded__secret`); by then MCP had
  connected, so it found **only** `mcp__guarded__echo`, "No matching deferred
  tools found" for `secret`, called echo, and got `token=LOCKED` back.
- Net: confinement held (echo only, secret undiscoverable, pin injected), but it
  depends on the agent searching after connect, and claude's built-in tools need
  explicit denial. **codex remains the simpler headless vehicle** (tools present
  synchronously). Transcript: `e2e/runs/c1-drop-pin/claude/`.

## Numbers — real tool-surface token savings (through sand)

`count_tokens` on the surfaces `sand mcp` actually serves (`e2e/bench/`):

| upstream | tools full→slim | full tok | slim tok | saved |
|---|---|---|---|---|
| fast-mcp | 2→1 | 608 | 561 | 7.7% |
| everything | 13→2 | 1812 | 652 | **64.0%** |

The `everything` result (64.0%) matches sand's logged 62.0% and lagom's own bench
(−64%) — cross-validated across three independent measurements. fast-mcp is small
by construction (2 tools, one kept). Realistic multi-upstream magnitudes are sand's
logged **54.8%** across everything+filesystem+memory (`SAND_LAGOM_FINDINGS.md` §3,
reproducible via sand's `bench/sand_bench.py`).

## Findings / issues surfaced

- **Invisibility was NOT complete — names held, descriptions + errors leaked the
  brand.** Honest correction: lagom is invisible in server names, tool names, and
  schemas, but two paths leaked the word "lagom" to the agent:
  1. **Description addendum (FIXED in core).** A constrained/pinned tool under a
     *passthrough* description got `"Restricted by lagom: \`token\` is fixed."`
     appended — so the agent literally saw "lagom" in the `echo` description in
     these very runs. Fixed: the addendum is now brand-free (`"Restricted: ..."`,
     `crates/lagom-core/src/project.rs`; regression test `addendum_is_brand_free`;
     verified across all 4 bindings in `parity/out/*`). **The committed wire/
     transcript captures here predate the fix** (taken through `sand@a7a748e`) and
     still show the old text; a clean through-sand re-capture is pending sand's
     pin bump (the fix is verified at the lagom level today, not yet through sand).
  2. **Constraint-rejection error prefix (consumer-boundary).** A reject returns
     `"lagom: rewrite: argument ..."` — the `lagom:` comes from the lagom-go
     binding (`go/lagom.go:157`), which prefixes *all* errors (intentional + useful
     for Go devs). It only leaks because the consumer relays the raw error string
     to the agent. *Fix:* sand should present the clean reject text (strip the
     `lagom: rewrite:` prefix) when surfacing a gate rejection to an agent; lagom
     keeps the prefix for developer-facing errors.
- **claude -p async race** (above) — real, recoverable via ToolSearch; codex preferred.
- **No process leak** in any run — sand reaps on stdin-EOF/signal AND the harness
  `pkill`s the workdir; every run logged `no-leak: PASS`.

## Reproduce everything

```sh
# prereqs: sand on PATH (cd ../../sand/main && mage install), codex, claude, node,
#          ANTHROPIC_API_KEY (count_tokens is free).
cd /Users/evanschultz/Documents/Code/hylla/lagom/main

# concept 1 — DROP + RENAME + PIN + sealed-surface (codex + claude + wire)
PROFILE=e2e/profiles/fastmcp-drop-pin.json SERVER=guarded KEPT=echo \
  OUT=e2e/runs/c1-drop-pin/codex \
  PROMPT='Output ONLY one compact JSON object: {"mcp_tools":[...],"tried_secret":"...","called_echo":"..."}' \
  bash e2e/bin/run-codex.sh
python3 e2e/bin/extract_codex.py e2e/runs/c1-drop-pin/codex/codex.events.jsonl

# resolve templates, then wire/agent proofs for concepts 4–7
for p in fastmcp-constrain fastmcp-sealed multi-alpha multi-beta fastmcp-passthrough; do
  bash e2e/bin/resolve.sh e2e/profiles/$p.json e2e/runs/_resolved/$p.json; done
PROFILE="$PWD/e2e/runs/_resolved/fastmcp-constrain.json" OUT=e2e/runs/c4-constrain/wire bash e2e/bin/wire-probe.sh
PROFILE="$PWD/e2e/runs/_resolved/fastmcp-constrain.json" OUT=e2e/runs/c4-constrain/wire NAME=echo ARGS='{"message":"NOPE"}' LABEL=bad bash e2e/bin/wire-call.sh
PA="$PWD/e2e/runs/_resolved/multi-alpha.json" SA=alpha KA=ping \
PB="$PWD/e2e/runs/_resolved/multi-beta.json"  SB=beta  KB=reveal \
  OUT=e2e/runs/c6-multi/codex PROMPT='Output ONLY JSON: {"tools":[...],"ping_result":"...","reveal_result":"..."}' \
  bash e2e/bin/run-codex-multi.sh

# savings (real count_tokens through sand)
PROFILE="$PWD/e2e/profiles/everything-passthrough.json" OUT=e2e/runs/bench/everything-full bash e2e/bin/wire-probe.sh
PROFILE="$PWD/e2e/profiles/everything-slim.json"        OUT=e2e/runs/bench/everything-slim bash e2e/bin/wire-probe.sh
python3 e2e/bin/bench_savings.py \
  everything e2e/runs/bench/everything-full/wire.tools.jsonl e2e/runs/bench/everything-slim/wire.tools.jsonl
```
Raw transcripts, resolved profiles, wire captures, extracted facts, and the bench
jsonl/csv/REPORT are all committed under `e2e/`. Failures and messy iterations are
kept in (only secrets scrubbed).
