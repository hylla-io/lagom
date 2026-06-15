#!/usr/bin/env bash
# reproduce.sh — one command to regenerate the DETERMINISTIC lagom-via-sand
# evidence from a clean checkout: every concept's protocol-level proof (through
# `sand mcp`) plus the real token-savings numbers. No machine-specific paths.
#
# Needs: sand on PATH (cd ../sand/main && mage install), node, and — for the
# savings bench — ANTHROPIC_API_KEY (count_tokens is free). The agent-level runs
# (codex / claude -p) are non-deterministic + cost API tokens; their exact
# commands are printed at the end (see docs/LAGOM_VIA_SAND_E2E.md §Reproduce).
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO"
BIN=e2e/bin
command -v sand >/dev/null || { echo "FAIL: sand not on PATH (cd ../sand/main && mage install)"; exit 1; }

echo "== resolve profile templates (portable) =="
for p in fastmcp-drop-pin fastmcp-constrain fastmcp-sealed multi-alpha multi-beta fastmcp-passthrough; do
  bash "$BIN/resolve.sh" "e2e/profiles/$p.json" "e2e/runs/_resolved/$p.json" >/dev/null
done

echo "== concept wire proofs (through sand, deterministic) =="
PROFILE="$REPO/e2e/runs/_resolved/fastmcp-drop-pin.json" OUT=e2e/runs/c1-drop-pin/wire bash "$BIN/wire-probe.sh"
PROFILE="$REPO/e2e/runs/_resolved/fastmcp-constrain.json" OUT=e2e/runs/c4-constrain/wire bash "$BIN/wire-probe.sh"
PROFILE="$REPO/e2e/runs/_resolved/fastmcp-constrain.json" OUT=e2e/runs/c4-constrain/wire NAME=echo ARGS='{"message":"hi"}' LABEL=good bash "$BIN/wire-call.sh"
PROFILE="$REPO/e2e/runs/_resolved/fastmcp-constrain.json" OUT=e2e/runs/c4-constrain/wire NAME=echo ARGS='{"message":"NOPE"}' LABEL=bad bash "$BIN/wire-call.sh"
PROFILE="$REPO/e2e/runs/_resolved/fastmcp-sealed.json" OUT=e2e/runs/c5-sealed/wire bash "$BIN/wire-probe.sh"
PROFILE="$REPO/e2e/runs/_resolved/multi-alpha.json" OUT=e2e/runs/c6-multi/wire-alpha bash "$BIN/wire-probe.sh"
PROFILE="$REPO/e2e/runs/_resolved/multi-beta.json" OUT=e2e/runs/c6-multi/wire-beta bash "$BIN/wire-probe.sh"

echo "== savings bench (real count_tokens through sand) =="
if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
  PROFILE="$REPO/e2e/runs/_resolved/fastmcp-passthrough.json" OUT=e2e/runs/bench/fast-full bash "$BIN/wire-probe.sh" >/dev/null
  PROFILE="$REPO/e2e/runs/_resolved/fastmcp-sealed.json" OUT=e2e/runs/bench/fast-slim bash "$BIN/wire-probe.sh" >/dev/null
  PROFILE="$REPO/e2e/profiles/everything-passthrough.json" OUT=e2e/runs/bench/everything-full bash "$BIN/wire-probe.sh" >/dev/null
  PROFILE="$REPO/e2e/profiles/everything-slim.json" OUT=e2e/runs/bench/everything-slim bash "$BIN/wire-probe.sh" >/dev/null
  python3 "$BIN/bench_savings.py" \
    fast-mcp e2e/runs/bench/fast-full/wire.tools.jsonl e2e/runs/bench/fast-slim/wire.tools.jsonl \
    everything e2e/runs/bench/everything-full/wire.tools.jsonl e2e/runs/bench/everything-slim/wire.tools.jsonl
else
  echo "  (skipped: set ANTHROPIC_API_KEY to regenerate token numbers)"
fi

cat <<'NEXT'

== agent-level runs (non-deterministic, cost API tokens — run manually) ==
  codex (concept 1):  PROFILE=e2e/profiles/fastmcp-drop-pin.json SERVER=guarded KEPT=echo \
    OUT=e2e/runs/c1-drop-pin/codex PROMPT='...' bash e2e/bin/run-codex.sh
  codex (multi):      see docs/LAGOM_VIA_SAND_E2E.md §Reproduce
  claude -p:          bash e2e/bin/run-claude.sh (see docs)
NEXT
echo "DONE — deterministic evidence regenerated under e2e/runs/"
