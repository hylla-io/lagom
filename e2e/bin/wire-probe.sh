#!/usr/bin/env bash
# wire-probe.sh — deterministic protocol-level proof of the slim surface.
#
# Speaks raw MCP JSON-RPC (initialize -> initialized -> tools/list) directly to
# `sand mcp --profile <p>` and captures the tools/list response. This proves
# EXACTLY which tools the server offers an agent — independent of any agent's
# self-report. Pair it with a codex transcript (run-codex.sh) for the full proof:
# wire = "what is offered", transcript = "what the agent did within it".
#
# Usage: PROFILE=<resolved profile.json> OUT=<dir> bin/wire-probe.sh
set -uo pipefail
SAND="$(command -v sand)"
: "${PROFILE:?set PROFILE}"; : "${OUT:?set OUT}"
mkdir -p "$OUT"

REQ='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wire-probe","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}'

# Feed the three messages, then EOF so sand exits and reaps its upstream child.
printf '%s\n' "$REQ" | "$SAND" mcp --profile "$PROFILE" > "$OUT/wire.tools.jsonl" 2> "$OUT/wire.err.log"

# Pull the tool names from the tools/list result (id:2).
if command -v jq >/dev/null; then
  jq -rc 'select(.id==2) | .result.tools[].name' "$OUT/wire.tools.jsonl" 2>/dev/null \
    | sort > "$OUT/wire.toolnames.txt"
fi
echo "wire tools offered: $(paste -sd, "$OUT/wire.toolnames.txt" 2>/dev/null)"
