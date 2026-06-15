#!/usr/bin/env bash
# lagom PoC — codex exec confined to a lagom-slimmed MCP. codex connects MCP
# synchronously (startup_timeout_sec), so unlike claude -p it sees the tools.
set -uo pipefail
LAGOM="/Users/evanschultz/Documents/Code/hylla/lagom/main/target/debug/lagom"
FAST="/Users/evanschultz/Documents/Code/hylla/lagom/main/bin/fast-mcp.js"
W="$(mktemp -d /tmp/lagom-codex.XXXXXX)"
cleanup() { pkill -f "$W" 2>/dev/null; sleep 1; rm -rf "$W"; }
trap cleanup EXIT

cat > "$W/slim.toml" <<EOF
default-presence = "keep"

[tools.secret]
presence = "drop"

[tools.echo.args.token]
pin = "LOCKED"
EOF
mkdir -p "$W/proj"

MCP="mcp_servers.guarded={command=\"${LAGOM}\",args=[\"serve\",\"--config\",\"${W}/slim.toml\",\"--\",\"node\",\"${FAST}\"],startup_timeout_sec=25,tools={echo={approval_mode=\"approve\"}}}"

echo "--- procs before ---"; pgrep -fl "lagom serve|fast-mcp" || echo "(none)"
echo "=== codex exec (confined to guarded MCP) ==="
codex exec --ephemeral --ignore-user-config --skip-git-repo-check -C "$W/proj" \
  -c 'approval_policy="never"' \
  -c 'sandbox_mode="read-only"' \
  -c 'project_doc_max_bytes=0' \
  -c 'skills.bundled.enabled=false' \
  -c "$MCP" \
  'Output ONLY one compact JSON object, nothing else: {"mcp_tools":[the tool names available to you],"tried_secret":"call the secret tool with key=x; put result text or the EXACT error","called_echo":"call the echo tool with message=hi; put the EXACT result text returned"}' \
  2>"$W/err.log"
echo "--- codex exit: $? ---"
echo "--- err tail ---"; tail -6 "$W/err.log"
echo "--- procs AFTER (clean teardown?) ---"; sleep 1; pkill -f "$W" 2>/dev/null; pgrep -fl "lagom serve|fast-mcp" || echo "(none — torn down)"
