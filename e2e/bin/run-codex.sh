#!/usr/bin/env bash
# run-codex.sh — drive a REAL headless codex agent confined to a `sand mcp`
# server (lagom inside, invisible) and capture the FULL transcript + tool calls.
#
# Unlike a self-reported assertion, this records codex's own `--json` event
# stream (every MCP tool-list, tool call, and result) so we verify what the agent
# ACTUALLY saw and did — not what it claims. Everything is logged under OUT/.
#
# Usage (env-driven):
#   PROFILE=<path to a sand profile.json template using __NODE__/__FAST__/__UP*__>
#   SERVER=<brand server name in the profile, e.g. guarded>
#   KEPT=<comma tool names to pre-approve, e.g. echo>
#   PROMPT=<the probe prompt>
#   OUT=<output dir under e2e/runs/...>
#   [UPSTREAM=<path to stdio mcp js>]   # default: lagom fast-mcp.js
#   bin/run-codex.sh
#
# Requires sand + codex + node on PATH. Reaps the process tree on exit.
set -uo pipefail

SAND="$(command -v sand)"; CODEX="$(command -v codex)"; NODE="$(command -v node)"
REPO="/Users/evanschultz/Documents/Code/hylla/lagom/main"
UPSTREAM="${UPSTREAM:-$REPO/bin/fast-mcp.js}"
: "${PROFILE:?set PROFILE}"; : "${SERVER:?set SERVER}"; : "${KEPT:?set KEPT}"
: "${PROMPT:?set PROMPT}"; : "${OUT:?set OUT}"

for b in "$SAND" "$CODEX" "$NODE"; do [[ -z "$b" ]] && { echo "FAIL: missing sand/codex/node"; exit 1; }; done
[[ -f "$UPSTREAM" ]] || { echo "FAIL: upstream not found: $UPSTREAM"; exit 1; }

mkdir -p "$OUT"
W="$(mktemp -d /tmp/lagom-e2e.XXXXXX)"
cleanup() { pkill -f "$W" 2>/dev/null; pkill -f "$UPSTREAM" 2>/dev/null; sleep 1; rm -rf "$W"; }
trap cleanup EXIT
mkdir -p "$W/proj"

# Materialize the profile: substitute node + upstream paths into the template.
sed -e "s#__NODE__#${NODE}#g" -e "s#__FAST__#${UPSTREAM}#g" \
    -e "s#__UP1__#${NODE}#g"  -e "s#__UP2__#${UPSTREAM}#g" "$PROFILE" > "$W/profile.json"
cp "$W/profile.json" "$OUT/profile.resolved.json"

# Build codex's per-tool approval map from KEPT (so calls don't block).
TOOLS=""; IFS=',' read -ra KT <<< "$KEPT"
for t in "${KT[@]}"; do TOOLS="${TOOLS}${t}={approval_mode=\"approve\"},"; done
MCP="mcp_servers.${SERVER}={command=\"${SAND}\",args=[\"mcp\",\"--profile\",\"${W}/profile.json\"],startup_timeout_sec=25,tools={${TOOLS%,}}}"

echo "--- procs before ---" | tee "$OUT/run.log"
pgrep -fl "sand mcp|$(basename "$UPSTREAM")" 2>/dev/null | tee -a "$OUT/run.log" || echo "(none)" | tee -a "$OUT/run.log"

# --json streams every event (incl. mcp tool calls) to stdout; -o writes the
# final agent message; stderr carries the mcp connect log.
"$CODEX" exec --json --ephemeral --ignore-user-config --skip-git-repo-check -C "$W/proj" \
  -c 'approval_policy="never"' \
  -c 'sandbox_mode="read-only"' \
  -c 'project_doc_max_bytes=0' \
  -c 'skills.bundled.enabled=false' \
  -c "$MCP" \
  -o "$OUT/codex.final.txt" \
  "$PROMPT" \
  > "$OUT/codex.events.jsonl" 2> "$OUT/codex.err.log"
echo "codex exit: $?" | tee -a "$OUT/run.log"

echo "--- procs AFTER ---" | tee -a "$OUT/run.log"
pkill -f "$W" 2>/dev/null
if pgrep -fl "sand mcp|$(basename "$UPSTREAM")" 2>/dev/null | tee -a "$OUT/run.log"; then
  echo "no-leak: FAIL" | tee -a "$OUT/run.log"
else
  echo "no-leak: PASS (torn down)" | tee -a "$OUT/run.log"
fi
echo "transcript: $OUT/codex.events.jsonl ($(wc -l < "$OUT/codex.events.jsonl") events)" | tee -a "$OUT/run.log"
