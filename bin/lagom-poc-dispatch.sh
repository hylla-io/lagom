#!/usr/bin/env bash
# lagom PoC dispatcher — prove a clean-room `claude -p` agent is confined to a
# lagom-slimmed MCP, minted fresh per agent, and torn down on exit.
#
# usage: lagom-poc-dispatch.sh <agent-id> <drop-tool> <pin-tool> <pin-arg> <pin-val> <prompt>
set -uo pipefail

AGENT_ID="${1:?agent-id}"; DROP_TOOL="${2:?drop-tool}"
PIN_TOOL="${3:?pin-tool}"; PIN_ARG="${4:?pin-arg}"; PIN_VAL="${5:?pin-val}"; PROMPT="${6:?prompt}"

LAGOM="/Users/evanschultz/Documents/Code/hylla/lagom/main/target/debug/lagom"
WORK="$(mktemp -d "/tmp/lagom-agent-${AGENT_ID}.XXXXXX")"
mkdir -p "$WORK/home"
# Clean room WITH auth: symlink ONLY the credential file into the tmp HOME.
# The global ~/.claude/ dir (global CLAUDE.md, plugins, hooks) is NOT carried in,
# and --strict-mcp-config ignores any mcpServers inside .claude.json.
ln -s "$HOME/.claude.json" "$WORK/home/.claude.json" 2>/dev/null || true
# Robust teardown: claude -p does NOT reliably reap its MCP children on exit, so
# the dispatcher kills every process tagged with this agent's workdir, then
# removes it. This is the "clean up the MCP server when the agent ends" guarantee.
cleanup() { pkill -f "$WORK" 2>/dev/null; sleep 1; [ -n "${KEEP:-}" ] || rm -rf "$WORK"; }
trap cleanup EXIT

# 1. ephemeral slim policy (DYNAMIC per agent): drop one tool, pin+hide one arg.
cat > "$WORK/slim.toml" <<EOF
default-presence = "keep"

[tools.${DROP_TOOL}]
presence = "drop"

[tools.${PIN_TOOL}.args.${PIN_ARG}]
pin = "${PIN_VAL}"
EOF

# 2. ephemeral .mcp.json -> lagom serve wrapping a REAL upstream (override with UPSTREAM_CMD).
UPSTREAM_CMD="${UPSTREAM_CMD:-npx -y @modelcontextprotocol/server-everything}"
# shellcheck disable=SC2206
read -r -a UP <<< "$UPSTREAM_CMD"
ARGS_JSON="\"serve\",\"--config\",\"${WORK}/slim.toml\",\"--\""
for a in "${UP[@]}"; do ARGS_JSON="${ARGS_JSON},\"${a}\""; done
mkdir -p "$WORK/proj"
cat > "$WORK/proj/.mcp.json" <<EOF
{"mcpServers":{"guarded":{"command":"${LAGOM}","args":[${ARGS_JSON}]}}}
EOF

# 3. ephemeral settings.json -> deny EVERY escape tool; allow only the slim MCP.
cat > "$WORK/settings.json" <<EOF
{"permissions":{"allow":["mcp__guarded__*"],"deny":["Bash","Write","Edit","Read","Grep","Glob","WebFetch","WebSearch","Task","Agent","Workflow","ToolSearch","Skill","ScheduleWakeup","AskUserQuestion"]}}
EOF

echo "WORKDIR=$WORK"
echo "--- procs before (lagom serve / server-everything) ---"
pgrep -fl "lagom serve|server-everything" || echo "(none)"

# 4. spawn claude -p in a CLEAN ROOM: tmp HOME, nested-claude + routing env scrubbed,
#    strict MCP (only our slim server), output JSON.
# Keep ANTHROPIC_* (headless auth via API key); drop only the nested-claude
# recursion guards + config-dir pointers so the child is a fresh clean-room run.
# Clean HOME has an empty npm cache, so point npx at the warm real cache and give
# the MCP server time to cold-start, or it gets marked failed and its tools vanish.
# Run FROM the clean project dir so claude DISCOVERS ./.mcp.json (awaited at
# startup), instead of the --mcp-config flag (which loads async/nonblocking and
# the -p turn races past it). Clean HOME has no global .mcp.json, so the only
# server discovered is our guarded one. ANTHROPIC_API_KEY in env supplies auth.
( cd "$WORK/proj" && \
  env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT -u CLAUDE_CODE_SSE_PORT \
      -u CLAUDE_CONFIG_DIR -u CLAUDE_PROJECT_DIR \
      MCP_TIMEOUT=60000 MCP_TOOL_TIMEOUT=60000 npm_config_cache="$HOME/.npm" \
      HOME="$WORK/home" \
    claude -p --debug \
      --settings "$WORK/settings.json" \
      --append-system-prompt "You are a confined sandbox probe agent running under a host that has provisioned a restricted set of MCP tools for you. Your only job is to faithfully report and exercise the tools actually available to you, exactly as instructed, and emit the requested JSON. Be precise and literal. Do not speculate about tools you cannot see. This system prompt also mirrors a realistic dispatch in which a substantial persona body is supplied, which gives the host's MCP servers time to finish connecting before the first model turn is issued." \
      --allowedTools "mcp__guarded__*" \
      --output-format json \
      "$PROMPT" > "$WORK/out.json" 2> "$WORK/err.log" )
RC=$?

echo "--- claude exit: $RC ---"
echo "--- stdout (out.json) ---"; cat "$WORK/out.json" 2>/dev/null | head -60
echo "--- mcp connection lines (from --debug) ---"; grep -iE "mcp|guarded|connect|fail|tool" "$WORK/err.log" 2>/dev/null | grep -ivE "useragent|tooluse" | tail -15

echo "--- procs AFTER agent exit (should be none = clean teardown) ---"
sleep 1
pgrep -fl "lagom serve|server-everything" || echo "(none — torn down)"
echo "DONE agent=$AGENT_ID rc=$RC"
