#!/usr/bin/env bash
# wire-call.sh — protocol-level proof of rewrite/constrain through sand.
#
# Speaks raw MCP (initialize -> initialized -> tools/call) to `sand mcp --profile`
# and captures the call response. Proves, with no agent involved, that lagom (via
# sand) injects pins and REJECTS constraint-violating args (annotated, never
# forwarded). The response for id:2 is either a result (allowed) or an error
# (rejected) — both are recorded.
#
# Usage: PROFILE=<resolved> OUT=<dir> NAME=<tool> ARGS=<json args object>
#        LABEL=<tag> bin/wire-call.sh
set -uo pipefail
SAND="$(command -v sand)"
: "${PROFILE:?}"; : "${OUT:?}"; : "${NAME:?}"; : "${ARGS:?}"; : "${LABEL:?}"
mkdir -p "$OUT"
REQ='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wire-call","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"'"$NAME"'","arguments":'"$ARGS"'}}'
printf '%s\n' "$REQ" | "$SAND" mcp --profile "$PROFILE" > "$OUT/call.$LABEL.jsonl" 2> "$OUT/call.$LABEL.err"
if command -v jq >/dev/null; then
  echo "[$LABEL] $NAME $ARGS ->"
  jq -c 'select(.id==2) | {result, error}' "$OUT/call.$LABEL.jsonl" 2>/dev/null
fi
