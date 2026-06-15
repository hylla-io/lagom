# lagom: give an agent *just enough* MCP — measured, not asserted

**Thesis.** Most MCP servers hand an agent a big tray of tools it doesn't need.
That tray costs tokens on *every* turn (tool defs are re-sent each request) and
widens what a possibly-adversarial agent can do. **lagom** is a transport-less
engine that projects an existing MCP server into a *narrowed* one — drop tools,
pin/hide args, constrain domains, slim descriptions — so the agent sees and can
do just enough. This post is the receipts: real token measurements across 10
real MCP servers, a real agent provably confined to a slimmed surface, and a
one-command path to reproduce every number. No blind stats.

Everything here is regenerable from a clean checkout (commands in §5). Measured
2026-06-15; Anthropic `count_tokens` model `claude-haiku-4-5-20251001`;
codex-cli 0.139.0.

## 1. What lagom is (and is not)

- 1.1 lagom is a pure transform: `project(full_defs, policy) → slim_defs` and
  `rewrite(call, policy) → upstream_call | reject`. One Rust core behind faces
  (CLI, Python, Go-via-wasm, TS). It is transport-less — it **never talks to a
  model and never sees a token bill.** What it measurably controls is the *tool
  surface*: the tool definitions sent to the model, and which calls are allowed.
- 1.2 So the honest claim is bounded: lagom reduces the **tool-surface tokens**
  re-sent every turn, and **structurally confines** what an agent can call. It
  does not, by itself, measure end-to-end billed cost (that belongs to the agent
  harness). We measure exactly what lagom owns.

## 2. Method (so you can attack it)

- 2.1 For each real MCP server we drive `lagom serve` to capture the **full**
  projected `tools/list` (passthrough policy) and a **slim** one (sealed
  allowlist keeping 2 tools; "caveman" = + terse description override).
- 2.2 We count the real token cost of each surface with Anthropic's
  `count_tokens` API: `tool_tokens = count(message + tools) − count(message-only
  baseline)`. Baseline = 8 tokens. Free API, real tokenizer.
- 2.3 Every raw surface, policy, and count is logged to `bench/raw/`,
  `bench/results.jsonl`, `bench/results.csv`, `bench/REPORT.md`. The chart
  (`bench/savings.svg`) is generated *from* `results.jsonl`, not by hand.
- 2.4 Servers that need credentials or can't start headless are **skipped and
  logged** (`status:"skipped"`), never silently dropped.

## 3. Results — tool-surface token reduction (10 real servers)

| server | tools full→slim | full tok | slim (allowlist) | slim (caveman) | saved (allow / caveman) |
|---|---|---|---|---|---|
| everything | 13→2 | 2040 | 729 | 719 | 64.3% / 64.8% |
| filesystem | 14→2 | 2590 | 861 | 752 | 66.8% / 71.0% |
| memory | 9→2 | 1710 | 853 | 836 | 50.1% / 51.1% |
| sequential-thinking | 1→1 | 1539 | 1539 | 874 | 0.0% / 43.2% |
| git | 12→2 | 2089 | 678 | 673 | 67.5% / 67.8% |
| fetch | 1→1 | 813 | 813 | 746 | 0.0% / 8.2% |
| time | 2→2 | 811 | 811 | 808 | 0.0% / 0.4% |
| sqlite | 6→2 | 879 | 634 | 619 | 27.9% / 29.6% |
| github | 26→2 | 5385 | 884 | 882 | 83.6% / 83.6% |
| puppeteer | 7→2 | 1213 | 836 | 833 | 31.1% / 31.3% |

**Total: 19,069 → 8,638 tool tokens — 54.7% saved across 10 servers** (allowlist).
Skipped (cred-gated, logged): brave-search, slack.

- 3.1 The win scales with surface size: github (26 tools) → 83.6%. A focused
  agent that needs 2 of 26 tools pays ~1/6 the tool-token tax every turn.
- 3.2 **Honest rows.** Single-tool servers (sequential-thinking, fetch, time)
  show **0% under allowlisting** — you can't drop the only tool. There, only
  description slimming ("caveman") helps (sequential-thinking 43.2%). lagom is a
  surface tool; it can't reduce a surface that's already minimal.

## 4. Confinement — a real agent, provably limited

A real headless `codex exec` agent was confined to a lagom-slimmed MCP (`lagom
serve`, policy = keep all + drop `secret` + pin `echo.token`). Verified from the
agent's own transcript and a protocol-level wire probe, not its self-report:

- 4.1 **Slim surface** — the agent's tool list is `echo` only; `secret` is absent.
- 4.2 **Dropped → unavailable** — calling `secret` returns "tool not available".
- 4.3 **Pin injected** — the agent sent `{message:"hi"}` (no token); the upstream
  received `{"message":"hi","token":"LOCKED"}`. It cannot *not* send the pin, and
  cannot *see* it.
- 4.4 **Constraint enforced** — an out-of-enum arg is rejected with an annotation,
  never forwarded (protocol wire test).
- 4.5 **Brand-free** — the projected surface names lagom nowhere (server name,
  tool names, schemas, and — after a bug we found and fixed, see §6 — the
  description addenda: `"Restricted: \`token\` is fixed."`, not "by lagom").
- 4.6 Clean teardown: no leaked server/upstream processes after exit.

Artifacts: `e2e/runs/lagom-direct/` (wire surface + codex result), `e2e/runs/c*`
(broader concept runs through the `sand` consumer).

## 5. Reproduce everything

```sh
# prereqs: rust toolchain, node, uv (uvx), ANTHROPIC_API_KEY (count_tokens is free)
just build                       # build the lagom binary
python3 bench/bench.py           # regenerate the 10-server token corpus + REPORT
python3 bench/chart.py           # regenerate savings.svg FROM results.jsonl
bash bin/lagom-codex-poc.sh      # real codex agent confined to lagom serve (needs codex)
just reproduce                   # deterministic concept proofs through the sand consumer
```

Raw surfaces, policies, transcripts, jsonl/csv, and the chart are all committed.
Failures and skipped servers are kept in, not scrubbed.

## 6. Honest limitations

- 6.1 **Surface tokens ≠ billed cost.** We measure tool-def tokens re-sent per
  turn — real and lagom-attributable — not end-to-end billed tokens of a full
  agent run (that depends on turns, caching, output, and the harness, not lagom).
- 6.2 **Single-tool servers gain little** from allowlisting (§3.2).
- 6.3 **Coverage is what installs headless** — 10 of 12 attempted; 2 cred-gated
  servers skipped (logged).
- 6.4 **We found our own bug.** Constrained tools under passthrough descriptions
  leaked `"Restricted by lagom:"` to the agent — invisibility held for names but
  not descriptions. Caught by grepping our own captures; fixed in core
  (brand-free addendum + regression test); the pre-fix captures are kept for the
  record and marked.
- 6.5 **Unreleased.** lagom is v0.1.0, not yet tagged; numbers are from the
  current `main`.
