# SAND + LAGOM — CAVEMAN HANDOFF

caveman docs. short. no grammar. only what matter.
sand = first app that eat lagom as dep. build both right, first time.
this file = shared truth. copy to sand repo too.

---

## 1. WHAT EACH THING IS

- lagom = make big MCP small for one agent. agent see less. agent do less. safe + small context.
- sand = launch agents (claude -p, codex exec). sand = dispatcher + gate + env box.
- sand use lagom inside. sand = first "user" of lagom-as-lib.
- agent never know lagom there. lagom invisible if app want.

## 2. HOW LAGOM WORK (consume it)

- lagom = one rust brain. faces: CLI binary, python, go (go get), TS (npm, shipped now).
- lib = pure functions. read NOTHING by self. you feed it.
  - project(tools, policy) -> small tools
  - rewrite(call, policy) -> safe call OR no (reject)
  - merge(base, overlay) -> smaller only, never bigger
  - validate(policy, real_tools) -> drift? loud no
- policy = DATA. toml or json or code-builder. policy say:
  - drop tool. hide arg (pin = fix value, gone from schema). limit arg (enum/range/regex). rename tool. new short doc.
- SAME binary every agent. each agent get DIFFERENT policy. no rebuild per agent. policy = data not code. fast.

## 3. BRAND (lagom invisible)

- app pick: own server name, own tool names (rename), own short docs (override), own config shape.
- lagom name only in go.mod / pip / npm. app user never see lagom. yes possible.
- lagom.toml = ONLY for people using global lagom CLI. app NOT use lagom.toml. app build policy in code or own branded config. (yes, you understood right.)
- ONE-CALL helper = `Guard` (SHIPPED). app dev write tiny code: `NewGuard(ctx, upstreamDefs, policy)` -> `SlimDefs()` (tools/list) + `Gate(call)` (rewrite/reject). "give me slim server for profile X" -> done.

## 4. MANY MCP, MANY AGENT

- many MCP for one agent: run many lagom-wrap servers, one per upstream, all in agent .mcp.json. each own slim. works now by compose. NO single config for many-upstream yet. maybe later.
- many agent: each agent own policy -> own slim. orch mint per agent. works.
- app consumer: slim different per agent, per project. app give each agent own policy. easy IF app give good helper.
- goal = STUPID EASY for app dev AND app user. need: one-call helper + docs + examples. not all there. build it.

## 5. SECURITY — CAN AGENT ESCAPE? IS RUST WRONG?

- lagom NOT bypass at MCP door. agent only path to upstream = THROUGH lagom. agent never know upstream cmd/keys. lagom own upstream child. no go-around.
- context small: lagom cut tools/list BEFORE agent see. full list never enter agent brain.
- BUT: if agent have escape tool (bash, exec, raw network) -> agent run upstream self, skip lagom. this break ANY mcp sandbox. NOT lagom fault. NOT rust fault.
- FIX = harness take away escape tools + clean room. that = SAND job. (gate, deny bash, strict-mcp-config, clean HOME.)
- rust NOT wrong. single binary RIGHT. per-agent = data not compile. no "build on demand" need. go would be same. keep rust.
- SAFE = lagom (only door + small context) + sand (no escape tool + clean room). need BOTH.

## 6. SAND GAPS (must fix to be good sandbox + good lagom user)

codex path = GOOD already: hermetic CODEX_HOME, execpolicy bash-deny, role-MCP inject, clean config. ~85% boxed.

claude -p path = WEAK. must grow:
- 6.1 NO clean HOME. agent see orch ~/.claude, global CLAUDE.md, plugins, hooks. MUST FIX. give tmp HOME / CLAUDE_CONFIG_DIR per agent.
- 6.2 NO per-agent settings.json. no --settings passed. MUST ADD. dev set per-agent permission + tool allow + context limit.
- 6.3 NO per-agent CLAUDE.md / --add-dir. ADD if want per-agent context files.
- 6.4 NO per-agent hooks. only ONE global hook + env-var gate. ADD per-role hooks.
- 6.5 NO per-agent context budget.
- why weak: sand claude path assume short agent that inherit orch session. for lagom sandbox we NEED full isolation. so claude path must grow up.

## 7. HOW SAND + LAGOM FIT (the plan)

best shape: sand IS the branded lagom-consuming MCP server (like `go/examples/branded.go`) + the dispatcher.
- sand use lagom-go inside. sand expose own server: `sand mcp --profile <ephemeral.json>` (name = sand, not lagom).
- flow per agent:
  1. orch -> sand dispatch role X
  2. sand pick/compute policy for role X (lagom Policy, data)
  3. sand write ephemeral profile /tmp/agent-ID/profile.json
  4. sand write agent .mcp.json -> point at `sand mcp --profile /tmp/agent-ID/profile.json`
  5. sand write per-agent settings.json (deny bash, allow only mcp__X__*, context limit)
  6. sand spawn `claude -p --strict-mcp-config --mcp-config ... --settings ...` in CLEAN tmp HOME
  7. agent see ONLY slim tools. agent can NOT escape (no bash). lagom invisible.
  8. agent die -> sand mcp child die -> upstream die -> temp gone. ephemeral.
- lagom slim the tools inside `sand mcp`. sand box the agent around it. together = sealed.

## 8. CONTRACT (agree NOW so no rework)

seam between sand and lagom:
- sand gives lagom: full upstream tool defs (json) + policy (json/builder). per role/agent.
- lagom gives sand: projected slim defs (for tools/list) + rewrite(call) (inject pin, reject bad). + validate (drift loud).
- ephemeral: sand mint profile (data) -> lagom resolve + serve via sand's own server. lagom mint_record/refire let re-run same box.
- branding: sand set server name + tool names + docs. lagom never reads sand's config; sand parses, builds Policy, calls lagom.
- decide: sand uses lagom-GO in-proc (sand is go, sand already mcp-go server) -> YES. sand not spawn separate lagom binary. lagom-go = the dep.

## 9. DO IT RIGHT FIRST TIME — ASKS

LAGOM side (ALL DONE — kept for the record):
- [DONE] TS binding (napi-rs) + smoke test like python (`@hylla-io/lagom`, `just node-test`).
- [DONE] docs ALL bindings (rustdoc, go doc, python docstrings, TS jsdoc, README per-binding + branding).
- [DONE] one-call brandable helper = `Guard` (NewGuard -> SlimDefs + Gate). app dev write tiny code. lagom invisible.
- [DONE] mint/refire wired + simple in every binding (Go `Mint`/`Refire`, py/ts `mint`/`refire`, CLI `serve --audit`/`refire`).
- [DONE] NO DRIFT: go = canonical full test. parity guard `just parity` proves rust/go/py/ts byte-identical (16 cases, zero drift).

SAND side:
- claude path: add clean tmp HOME, per-agent settings.json, per-agent hooks, optional per-agent CLAUDE.md, context budget.
- consume lagom-go for per-role slim MCP (sand mcp --profile).
- prove: real claude -p in clean room see ONLY slim tools, no escape, server die on exit.

BOTH:
- agree §8 contract before build. one Policy shape. one ephemeral profile shape. one helper API.
- build lagom helper first, then sand consume it, then prove together with real claude -p. test FULL again.
