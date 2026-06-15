# PROMPT FOR SAND ORCH — CAVEMAN

give this whole file to sand orch. caveman. short. no grammar. only what matter.
intent + reqs for sand to build its side right, so sand + lagom fit first time.

---

## 0. WHO YOU ARE, WHAT YOU DO

you = sand. you launch agents (claude -p, codex exec). you box them. you gate them.
you will be FIRST app that eat lagom as a dep. you use lagom to make each agent's MCP
small + safe. agent never see lagom. lagom hide inside you.

two jobs:
1. BOX the agent (clean room, no escape, per-agent config) — your job, mostly missing for claude path now.
2. SLIM the agent's MCP (only tools/args it need) — use lagom for this.

## 1. WHAT LAGOM IS (your new dep)

- lagom = take big MCP, give agent only small piece. drop tools, hide args, limit args, short docs.
- lagom = rust core, many faces. you use the GO face: `go get github.com/hylla-io/lagom/go` (needs `GOPRIVATE=github.com/hylla-io/*`). no cgo. wasm inside, run by wazero. one go get, no extra binary.
- lagom go api = pure functions:
  - `Project(ctx, upstreamDefsJSON, policyJSON) -> slimDefsJSON` — small tool list for tools/list
  - `Rewrite(ctx, callJSON, policyJSON) -> upstreamCallJSON | error` — inject hidden args, reject bad call
  - `Merge(ctx, baseJSON, overlayJSON) -> policyJSON` — narrow only, never widen
  - `Validate(ctx, policyJSON, upstreamDefsJSON) -> error` — drift? loud error
- lagom read NOTHING by self. YOU feed it defs + policy. YOU own files + brand.

## 2. POLICY = DATA (the slim recipe)

- policy = json (or toml for global CLI, but you use json/code). policy say per tool:
  - drop tool. rename tool. new short doc (description override). 
  - per arg: pin (fix value, gone from schema, injected on call). constrain (enum / range / regex). default. passthrough.
- pin example: `{"tools":{"query":{"args":{"scope":{"pin":"public"}}}}}` -> agent never see scope, lagom always send scope="public".
- drop example: `{"tools":{"admin_reset":{"presence":"drop"}}}` -> tool gone.
- enum example: `{"tools":{"delete_file":{"args":{"path":{"constrain":{"enum":["a.txt","b.txt"]}}}}}}` -> only those paths, else reject.
- default policy = keep all (passthrough). sealed policy = drop all unless kept (use for tight sandbox).
- SAME you-binary every agent. each agent get DIFFERENT policy json. no rebuild. policy = data.

## 3. BRAND (lagom invisible)

- you pick: your server name, your tool names (rename), your short docs (override), your config shape.
- lagom name only in your go.mod. your users never see lagom. good.
- do NOT use lagom.toml (that = global CLI only). you build policy json in code / your own config.

## 4. HOW YOU FIT (the shape we want)

- you BECOME the branded slim MCP server. add command: `sand mcp --profile <path>`.
  - `sand mcp` use lagom-go INSIDE.
  - on start: load your full tool defs -> `lagom.Project(defs, policy)` -> register ONLY slim tools (use mcp-go `NewToolWithRawSchema` with the projected schema).
  - on each call: `lagom.Rewrite(call, policy)` first -> inject pins / reject bad -> then run real handler.
  - (pattern proven: lagom repo `go/examples/branded.go` — `NewBrandedServer` turns a lagom `Guard` into a real mcp-go server. copy that shape.)
- flow per agent:
  1. orch -> you dispatch role X
  2. you pick/compute policy for role X (json)
  3. you write ephemeral profile /tmp/agent-ID/profile.json
  4. you write agent .mcp.json -> point at `sand mcp --profile /tmp/agent-ID/profile.json`
  5. you write per-agent settings.json (deny bash/write, allow only mcp__<srv>__*, context limit)
  6. you spawn `claude -p --strict-mcp-config --mcp-config <that> --settings <that>` in CLEAN tmp HOME
  7. agent see ONLY slim tools. agent can NOT escape. lagom invisible.
  8. agent die -> `sand mcp` child die -> temp gone. ephemeral by process-tree.

## 5. SECURITY — VERY IMPORTANT

- lagom is only door to upstream. agent can NOT go around lagom IF agent have no escape tool.
- ESCAPE TOOL = bash, exec, raw network. if agent has one, agent run upstream itself, skip lagom. breaks sandbox.
- SO YOUR JOB: take away escape tools. allow ONLY `mcp__<srv>__*`. deny Bash/Write/exec. `--strict-mcp-config` so agent ignore all other MCP.
- context limit = real because lagom cut tools/list before agent see. but only works if agent only has the slim server (strict-mcp-config) + clean HOME (no other mcp from ~/.claude).

## 6. WHAT YOU MUST BUILD/FIX (your gaps today)

codex path = already good (hermetic CODEX_HOME, execpolicy). keep.
claude -p path = WEAK. fix:
- 6.1 CLEAN ROOM: spawn claude -p with tmp HOME / tmp CLAUDE_CONFIG_DIR. agent must NOT inherit orch ~/.claude, global CLAUDE.md, plugins, hooks. (today: inherits. MUST FIX.)
- 6.2 PER-AGENT settings.json: pass `--settings <path>` per agent. dev set permission + tool allow + context limit. (today: none. ADD.)
- 6.3 PER-AGENT CLAUDE.md / context files: optional `--add-dir` or system prompt per agent. (ADD if want.)
- 6.4 PER-AGENT hooks: let dev define hooks per role/agent (PreToolUse etc). (today: one global hook + env gate. ADD per-role.)
- 6.5 the `sand mcp --profile` branded server using lagom-go (section 4).

## 7. TIMING (do not block on lagom)

- NOW: you build your side ALONE — clean room, per-agent settings/hooks/CLAUDE.md, the `sand mcp` server skeleton. do not wait.
- lagom is being finished in parallel (TS binding, docs, brandable helper, mint/refire, full battle/e2e, binding parity).
- WHEN lagom says "fully tested + battle/e2e done": you `go get` lagom, wire it into `sand mcp`, and TEST your consumption.
- THEN: joint test — lagom + sand together prove a real claude -p agent sees ONLY slim tools, cannot escape, server dies on exit.

## 8. CONTRACT (agree, do not drift)

- you give lagom: full upstream defs (json) + policy (json). per role/agent.
- lagom give you: slim defs (json), Rewrite(call), Validate(drift).
- you use lagom-GO in-proc (you are go + mcp-go server already). NOT a separate lagom binary.
- you own brand. lagom never read your config. you parse, you build policy json, you call lagom.
- ephemeral: you mint profile (data). lagom mint_record/refire let you re-run same box if needed.

## 9. DONE = (your checklist)

- [ ] `sand mcp --profile <json>` serves a slim MCP (lagom-go inside), branded, lagom invisible
- [ ] claude -p spawned in CLEAN tmp HOME, no inherited ~/.claude
- [ ] per-agent settings.json passed (deny escape tools, allow only slim mcp, context limit)
- [ ] per-agent hooks + optional per-agent CLAUDE.md supported
- [ ] PROOF: real claude -p agent lists ONLY slim tools, cannot call dropped/constrained, cannot escape (no bash), server process gone after agent exit
- [ ] consume lagom via `go get` (GOPRIVATE set), pinned to a tested lagom version
- [ ] joint e2e test with lagom passes

## 10. REFERENCE

- lagom repo: github.com/hylla-io/lagom. docs: SPEC.md, CONTEXT.md (glossary), FEATURES.md, docs/SAND_LAGOM_HANDOFF.md.
- working consumer example to copy: `go/examples/branded.go` + `branded_test.go` (mcp-go server gated by lagom-go `Guard`, proven e2e; runs via `just examples`).
- go get: `GOPRIVATE='github.com/hylla-io/*' go get github.com/hylla-io/lagom/go@<tested-version>`.
