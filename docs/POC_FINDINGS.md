# POC FINDINGS — REAL agent battle test (caveman)

what i tested: REAL `claude -p` agents (7 live Opus runs), clean tmp HOME, a
lagom-wrapped real MCP (server-everything, and a <1s node server), ephemeral
per-agent config minted at spawn. dispatcher: `bin/lagom-poc-dispatch.sh`.

## PROVEN

1. **lagom slims right under a clean env.** direct `lagom serve` probe (clean
   HOME, scrubbed env) returns the projected tools/list: `echo` kept, `get-sum`
   /`secret` **DROPPED** (absent), pinned arg gone from schema. lagom side = solid.

2. **clean-room claude -p auth.** clean tmp HOME alone = "Not logged in" (OAuth
   lives in macOS Keychain/session, NOT a file; symlinking `.claude.json` did not
   help). KEEPING `ANTHROPIC_API_KEY` + tmp HOME = authenticates. FINDING:
   headless agents need an API key (or a live session); you cannot clean-room
   OAuth from a file.

3. **teardown: claude -p LEAKS its MCP children.** after `claude -p` exits, the
   spawned `lagom serve` + upstream stayed ALIVE. the dispatcher must kill the
   process tree itself (`pkill -f "$WORKDIR"`); after that, none left. FINDING:
   do NOT trust the harness to reap MCP servers — the dispatcher owns cleanup.

4. **built-in tools persist in agent context.** the agent always has Read/Grep/
   Glob/Agent/Workflow/ToolSearch (+ deferred Cron/Task/...). `settings.deny`
   blocks EXECUTION, not PRESENCE. ToolSearch/Agent/Workflow are escape vectors →
   MUST deny ALL of them. FINDING: claude can't fully hide built-ins from
   context; confine by denying execution of every built-in (Bash/Read/Write/
   Edit/Grep/Glob/Task/Agent/Workflow/ToolSearch/...).

5. **claude -p loads MCP async — it's a RACE (refined).** debug log: `[MCP]
   --mcp-config servers running fully async (nonblocking)`; the guarded server
   **connects fine** ("Successfully connected (stdio) in 79ms, hasTools:true")
   but a trivial single-turn `-p` prompt fires the model request first (~112-254ms)
   so the tools miss that one turn. discovered `.mcp.json` behaves the same.
   REAL bin-sh dispatches win this race because a big persona body + project
   CLAUDE.md delay the turn past the ~250ms connect; trivial single-turn probes
   lose. `--settings deny` is ALSO not enforced in `-p` (bin-sh relies on a
   PreToolUse hook). So `claude -p` works for real multi-turn dispatches with big
   context; it is NOT reliable for a one-shot. **codex exec is the reliable
   headless vehicle (see GREEN below).**

## WHAT THIS MEANS FOR SAND (most important)

- headless one-shot `claude -p` is the WRONG vehicle for confined MCP — it races
  past the MCP connect. confined-MCP **claude** agents must run where the MCP is
  ALREADY connected: the **built-in Agent tool** (inside the orchestrator
  session, MCP already up) or a **warmed/persistent** claude session.
- this VALIDATES sand's existing design: claude-native via the built-in Agent
  tool; **codex** for headless (codex injects MCP via inline `-c` and is expected
  to connect synchronously before the turn — the likely viable headless confined
  path).
- always, regardless of vehicle: clean tmp HOME + API-key/session auth + DENY ALL
  built-ins + `--strict-mcp-config` + dispatcher reaps the process tree on exit.

## GREEN — proven end-to-end via codex exec (`bin/lagom-codex-poc.sh`)

a REAL headless `codex exec` agent, confined to a lagom-slimmed MCP (`secret`
dropped, `echo.token` pinned), wrapping the fast node upstream. codex connects MCP
SYNCHRONOUSLY (`startup_timeout_sec`), so unlike claude -p the tools are present.
agent output (exit 0):

```
{"mcp_tools":["mcp__guarded.echo", ...codex's own apps...],
 "tried_secret":"ERROR: secret tool is not available",
 "called_echo":"echo:{\"message\":\"hi\",\"token\":\"LOCKED\"}"}
```

every required proof, GREEN:
1. **context limited by the slim defs** — agent's guarded surface = ONLY `echo`.
2. **limited on what it can call** — `secret` (dropped) is "not available".
3. **unaware of the full server** — `secret` never appears; only the projection.
4. **dynamically created per agent** — ephemeral `slim.toml` minted at spawn.
5. **pin enforced** — agent sent only `message:hi`; upstream received
   `token:LOCKED` (lagom injected the hidden pin).
6. **clean teardown** — after exit, no `lagom serve`/`fast-mcp` procs (dispatcher
   reaps the tree).

## VEHICLE GUIDANCE FOR SAND

- **codex exec = the proven headless confined vehicle.** sync MCP + per-tool
  `approval_mode="approve"` + `approval_policy="never"` + hermetic CODEX_HOME +
  `--ignore-user-config` + `project_doc_max_bytes=0` + `skills.bundled.enabled=false`.
  lagom is the slimmer inside `mcp_servers.guarded`.
- **claude -p**: usable only with a big persona/CLAUDE.md (to win the async MCP
  race) AND a PreToolUse hook for deny-enforcement; not reliable one-shot. Prefer
  the built-in Agent tool (in-session) for OAuth/claude — matches sand's design.
- **always**: ephemeral per-agent profile + DENY all escape tools + dispatcher
  reaps the process tree on exit + (clean HOME needs API key, not OAuth-from-file).

## NOT yet tested (open)

- codex with OAuth auth specifically (used real ~/.codex auth here; confirm OAuth
  tier works the same).
- claude -p ollama tier (local, free) end-to-end with the slim MCP.
- folding the working codex vehicle into sand's actual dispatch.
