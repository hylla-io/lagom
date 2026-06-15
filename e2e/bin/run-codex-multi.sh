#!/usr/bin/env bash
# run-codex-multi.sh — prove MULTI-UPSTREAM composition at the agent level: two
# independent `sand mcp` servers (two profiles, two brands) wired into ONE codex
# agent, captured via --json. Proves an agent's toolset is the union of two
# lagom-slimmed upstreams, each with its own renames/pins. Reaps both trees.
#
# Usage: PA=<resolved profile A> SA=<brand A> KA=<kept tool A>
#        PB=<resolved profile B> SB=<brand B> KB=<kept tool B>
#        PROMPT=<probe> OUT=<dir> bin/run-codex-multi.sh
set -uo pipefail
SAND="$(command -v sand)"; CODEX="$(command -v codex)"
UP="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/bin/fast-mcp.js"  # repo-relative
: "${PA:?}"; : "${SA:?}"; : "${KA:?}"; : "${PB:?}"; : "${SB:?}"; : "${KB:?}"; : "${PROMPT:?}"; : "${OUT:?}"
mkdir -p "$OUT"
W="$(mktemp -d /tmp/lagom-e2e-multi.XXXXXX)"
cleanup() { pkill -f "$W" 2>/dev/null; pkill -f "$UP" 2>/dev/null; sleep 1; rm -rf "$W"; }
trap cleanup EXIT; mkdir -p "$W/proj"

MA="mcp_servers.${SA}={command=\"${SAND}\",args=[\"mcp\",\"--profile\",\"${PA}\"],startup_timeout_sec=25,tools={${KA}={approval_mode=\"approve\"}}}"
MB="mcp_servers.${SB}={command=\"${SAND}\",args=[\"mcp\",\"--profile\",\"${PB}\"],startup_timeout_sec=25,tools={${KB}={approval_mode=\"approve\"}}}"

"$CODEX" exec --json --ephemeral --ignore-user-config --skip-git-repo-check -C "$W/proj" \
  -c 'approval_policy="never"' -c 'sandbox_mode="read-only"' \
  -c 'project_doc_max_bytes=0' -c 'skills.bundled.enabled=false' \
  -c "$MA" -c "$MB" -o "$OUT/codex.final.txt" \
  "$PROMPT" > "$OUT/codex.events.jsonl" 2> "$OUT/codex.err.log"
echo "codex exit: $?"
pkill -f "$W" 2>/dev/null
pgrep -fl "sand mcp|$(basename "$UP")" 2>/dev/null && echo "no-leak: FAIL" || echo "no-leak: PASS"
echo "transcript: $OUT/codex.events.jsonl ($(wc -l < "$OUT/codex.events.jsonl") events)"
