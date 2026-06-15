#!/usr/bin/env bash
# run-claude.sh — drive `claude -p` (headless) confined to a `sand mcp` server and
# capture the FULL stream-json transcript (system init tool list + tool_use calls
# + result). Tests claude -p as a confinement vehicle and records whatever
# actually happens — including the known async-MCP-connect race (a one-shot turn
# can fire before --mcp-config servers finish connecting). Findings, not faith.
#
# Usage: PROFILE=<resolved profile.json> SERVER=<brand> ALLOW=<mcp__brand__tool,...>
#        PROMPT=<probe> OUT=<dir> [UPSTREAM=<js>] bin/run-claude.sh
set -uo pipefail
SAND="$(command -v sand)"; CLAUDE="$(command -v claude)"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"  # repo root (e2e/bin -> ..)
UPSTREAM="${UPSTREAM:-$REPO/bin/fast-mcp.js}"
: "${PROFILE:?set PROFILE}"; : "${SERVER:?set SERVER}"; : "${ALLOW:?set ALLOW}"
: "${PROMPT:?set PROMPT}"; : "${OUT:?set OUT}"
for b in "$SAND" "$CLAUDE"; do [[ -z "$b" ]] && { echo "FAIL: missing sand/claude"; exit 1; }; done
mkdir -p "$OUT"
W="$(mktemp -d /tmp/lagom-e2e-claude.XXXXXX)"
cleanup() { pkill -f "$W" 2>/dev/null; pkill -f "$UPSTREAM" 2>/dev/null; sleep 1; rm -rf "$W"; }
trap cleanup EXIT

# claude's --mcp-config shape (distinct from codex's -c TOML).
cat > "$W/mcp.json" <<EOF
{"mcpServers":{"${SERVER}":{"command":"${SAND}","args":["mcp","--profile","${PROFILE}"]}}}
EOF
cp "$W/mcp.json" "$OUT/claude.mcp.json"

# Deny every escape tool; allow only the kept mcp tool(s). --strict-mcp-config so
# ONLY this server is loaded (no user/global MCP). stream-json + verbose => events.
"$CLAUDE" -p "$PROMPT" \
  --output-format stream-json --verbose \
  --mcp-config "$W/mcp.json" --strict-mcp-config \
  --allowedTools "$ALLOW" \
  --disallowedTools "Bash" "Read" "Write" "Edit" "Glob" "Grep" "Task" "WebFetch" "WebSearch" \
  > "$OUT/claude.events.jsonl" 2> "$OUT/claude.err.log"
echo "claude exit: $?"
pkill -f "$W" 2>/dev/null
if pgrep -fl "sand mcp|$(basename "$UPSTREAM")" 2>/dev/null; then echo "no-leak: FAIL"; else echo "no-leak: PASS"; fi
echo "transcript: $OUT/claude.events.jsonl ($(wc -l < "$OUT/claude.events.jsonl") events)"
