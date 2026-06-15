# PROMPT FOR SAND ORCH — consume + e2e battle-test lagom (caveman runbook)

you = sand. you eat lagom (go) as a dep to slim each agent's MCP. lagom invisible.
this = exact steps. real commands. do them.

## 1. GO GET (private repo, no release needed)

```sh
# one-time git auth for the private repo:
gh auth setup-git
# then:
export GOPRIVATE='github.com/hylla-io/*'
go get github.com/hylla-io/lagom/go@main      # or pin a commit: @0490cb3
```
notes:
- private repo -> needs GOPRIVATE (skips public proxy + sumdb) + git auth (gh setup-git, or a GIT_CONFIG insteadOf token).
- NO tag yet (v0.1.0 unreleased on purpose). pin `@main` or `@<commit>` for battle-test.
- pure go: NO cgo. `CGO_ENABLED=0 go build/test` works. lagom-go's only dep = wazero.

## 2. GO DOC (how you read the API)

pkg.go.dev will NOT show it (private). use LOCAL go doc after go get:
```sh
go doc github.com/hylla-io/lagom/go            # package overview + symbol list
go doc github.com/hylla-io/lagom/go Guard      # the helper type + methods
go doc github.com/hylla-io/lagom/go PolicyBuilder
go doc github.com/hylla-io/lagom/go Project
go doc -all github.com/hylla-io/lagom/go       # everything
```
every exported symbol has a doc comment. that IS the reference.

## 3. THE API (what you call)

```go
import lagom "github.com/hylla-io/lagom/go"

// pure ops (JSON in / JSON out, []byte):
slim, err := lagom.Project(ctx, upstreamDefsJSON, policyJSON)  // narrow tools/list
up,   err := lagom.Rewrite(ctx, callJSON, policyJSON)          // inject pins / reject
pol,  err := lagom.Merge(ctx, baseJSON, overlayJSON)           // narrow-only compose
err  = lagom.Validate(ctx, policyJSON, upstreamDefsJSON)       // drift -> error

// the helper you actually use in `sand mcp` (two calls, lagom invisible):
g, err := lagom.NewGuard(ctx, upstreamDefsJSON, policyJSON)
toolsList := g.SlimDefs()              // register as your mcp-go tools/list
upCall, err := g.Gate(ctx, callJSON)   // gate each tools/call (err = rejected)

// build policies typed (or just write the JSON):
pol, _ := lagom.SealedPolicyBuilder().Keep("search").
	Rename("search","find").Pin("search","artifact","hylla").
	ConstrainEnum("search","query",[]any{"a","b"}).Build()

// ephemeral per-agent mint + refire:
rec, _ := lagom.Mint(ctx, "agent-7", baseJSON, dynamicJSON, upstreamJSON)
resolved, _ := lagom.Refire(ctx, rec)
```

### CRITICAL FIELD GOTCHA
lagom `ToolDef` JSON uses **`input_schema`** (snake_case). MCP servers emit
**`inputSchema`** (camel). MAP IT before passing to Project/NewGuard:
each tool -> `{"name":..., "description":..., "input_schema": <the inputSchema>}`.
copy the one-line mapping from `go/examples/branded.go` (`lagomDef`). this is the
#1 thing that bites consumers.

## 4. THE PATTERN TO COPY

`go/examples/branded.go` + `branded_test.go` in the lagom repo: a real mcp-go
server whose tools/list is `g.SlimDefs()` and whose handlers run `g.Gate(...)`
first. proven e2e via mcp-go in-process client. your `sand mcp --profile <json>`
IS this, with your branding + ephemeral profile.

## 5. E2E BATTLE TEST (what you must prove)

vehicle = **codex exec** (PROVEN: codex connects MCP synchronously, so the agent
sees the slim tools. claude -p loads MCP async and a one-shot turn races past it —
use in-session/built-in Agent for claude, codex for headless). working reference:
lagom repo `bin/lagom-codex-poc.sh`.

steps:
1. mint an ephemeral profile (lagom Policy json) per agent. write to /tmp/agent-ID/.
2. spawn `codex exec --ephemeral --ignore-user-config -C <cwd> -c approval_policy="never" -c skills.bundled.enabled=false -c project_doc_max_bytes=0 -c 'mcp_servers.guarded={command="sand",args=["mcp","--profile","/tmp/agent-ID/profile.json"],startup_timeout_sec=25,tools={<kept-tool>={approval_mode="approve"}}}'` with a probe prompt.
3. DENY all escape tools; clean env; dispatcher KILLS the process tree on exit (the harness leaks MCP children — verified — so `pkill -f <workdir>` yourself).

assertions (all must pass):
- agent's tool list = ONLY the slim tools (dropped tools ABSENT).
- calling a dropped tool -> "not available".
- a pinned arg is injected (downstream upstream call shows the pinned value though the agent never set it).
- after the agent exits, NO `sand mcp`/upstream procs remain (clean teardown).

## 6. REAL NUMBERS (required, no hand-waving)

measure token savings with the REAL tokenizer + log it (lagom rule: REAL data only).
copy lagom's `bench/` pattern: Anthropic `count_tokens` on (full tools/list) vs
(slim tools/list), per real MCP server, logged to jsonl+csv+report. lagom's first
result: ~52% avg tool-surface token cut. produce your own numbers for sand's real
upstreams and track them.

## 7. REFERENCES IN THE LAGOM REPO

- `docs/HANDOFF.md` — full project state (read first).
- `docs/SAND_ORCH_PROMPT.md` — the design brief (what to build).
- `docs/POC_FINDINGS.md` — the real-agent findings (codex green, claude race).
- `go/examples/branded.go` — the consumer pattern to copy.
- `bin/lagom-codex-poc.sh` — the working codex confinement run.
- `bench/` — the real token-savings harness + numbers.
