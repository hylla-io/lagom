#!/usr/bin/env python3
"""bin/agent-audit-toon.py — translate bin/sh dispatcher capture into TOON audit summary.

Mirrors `post_tooluse_agent_audit.py` (hook, built-in Agent path) — same 16-bucket TOON
schema. This script handles the OTHER subagent dispatch paths: codex-exec stream + claude -p
JSON envelope (ollama / claude-native). Together with the hook, all 3 subagent dispatch
paths surface tool-call audit in the same shape.

USAGE:
  bin/agent-audit-toon.py --run-base <path-prefix>

Where <path-prefix> is the timestamp-role-pid prefix the dispatcher emits
(e.g. .claude/agent-runs/20260527-024832-ta-go-planning-38014). The script reads:

  <run-base>.meta.json — { run, role, backend, model, tier, served_by, cwd, gate, ts,
                           stdout, stderr } (per dispatcher line 638-641)
  <run-base>.tier<N>.<backend>.out — backend response
  <run-base>.tier<N>.<backend>.err — backend stderr / stream

Parses by backend:
  codex-exec → stream markers in .err file:
    `mcp: <server>/<tool> started|completed|failed` → MCP call
    `exec` + next line `/bin/zsh -lc <command>` → Bash call
    `codex` heading → model text (skip)
  ollama-local / ollama-cloud / claude-native → JSON envelope in .out file:
    `permission_denials[]` → forbidden_calls bucket (entries with tool_name + tool_input)
    Note: under --output-format json, successful tool_use stream is NOT in the envelope
    (gap; see AGENT_SANDBOX_SPEC.md F13). We capture denials + usage; successful
    tool_use stream loss is documented as dispatcher follow-up (switch to stream-json).

Emits TOON to stdout matching the 16-bucket schema from the hook.

SAND + TILLSYN GO-PORT REFERENCE:
  Sand's Go MCP returns `tool_calls` array in response envelope mirroring this TOON.
  Tillsyn's Go adapter persists tool_calls to action_item.metadata.tool_calls.
  The Python here is the proof-of-concept; sand/tillsyn re-implement the parser in Go.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


# --- Tool categorization (mirrors post_tooluse_agent_audit.py) ---

FILE_OPS = frozenset({"Read", "Edit", "Write", "MultiEdit", "Grep", "Glob", "NotebookEdit"})
BASH = frozenset({"Bash"})
WEB = frozenset({"WebFetch", "WebSearch"})
LSP = frozenset({"LSP"})
AGENT_RECURSIVE = frozenset({"Agent"})
SKILL = frozenset({"Skill"})
SCHEDULE = frozenset({"ScheduleWakeup", "CronCreate", "CronDelete", "CronList"})
WORKTREE = frozenset({"EnterWorktree", "ExitWorktree"})

# Claude code namespace (mcp__<server>__<tool>)
MCP_TA_PREFIX = "mcp__ta__"
MCP_TILLSYN_PREFIX = "mcp__tillsyn"
MCP_HYLLA_PREFIX = "mcp__hylla__"
PLUGIN_CONTEXT7_PREFIX = "mcp__plugin_context7_"
PLUGIN_PLAYWRIGHT_PREFIX = "mcp__plugin_playwright_"
PLUGIN_GOPLS_PREFIX = "mcp__plugin_gopls"

BUCKETS = [
    "file_ops",
    "bash_calls",
    "mcp_ta_calls",
    "mcp_tillsyn_calls",
    "mcp_hylla_calls",
    "plugin_context7_calls",
    "plugin_playwright_calls",
    "plugin_gopls_calls",
    "web_calls",
    "lsp_calls",
    "agent_calls",
    "skill_calls",
    "schedule_calls",
    "worktree_calls",
    "other_calls",
]


def categorize_claude(tool_name):
    if tool_name in FILE_OPS:
        return "file_ops"
    if tool_name in BASH:
        return "bash_calls"
    if tool_name in WEB:
        return "web_calls"
    if tool_name in LSP:
        return "lsp_calls"
    if tool_name in AGENT_RECURSIVE:
        return "agent_calls"
    if tool_name in SKILL:
        return "skill_calls"
    if tool_name in SCHEDULE:
        return "schedule_calls"
    if tool_name in WORKTREE:
        return "worktree_calls"
    if tool_name.startswith(MCP_TA_PREFIX):
        return "mcp_ta_calls"
    if tool_name.startswith(MCP_TILLSYN_PREFIX):
        return "mcp_tillsyn_calls"
    if tool_name.startswith(MCP_HYLLA_PREFIX):
        return "mcp_hylla_calls"
    if tool_name.startswith(PLUGIN_CONTEXT7_PREFIX):
        return "plugin_context7_calls"
    if tool_name.startswith(PLUGIN_PLAYWRIGHT_PREFIX):
        return "plugin_playwright_calls"
    if tool_name.startswith(PLUGIN_GOPLS_PREFIX):
        return "plugin_gopls_calls"
    return "other_calls"


def categorize_codex_mcp(server):
    """Codex stream uses server names without mcp__ prefix (e.g. `mcp: hylla/hylla.search`).
    Map server → bucket.
    """
    if server == "ta":
        return "mcp_ta_calls"
    if server.startswith("tillsyn"):
        return "mcp_tillsyn_calls"
    if server == "hylla":
        return "mcp_hylla_calls"
    if server == "context7":
        return "plugin_context7_calls"
    if server == "playwright":
        return "plugin_playwright_calls"
    if server == "gopls":
        return "plugin_gopls_calls"
    return "other_calls"


# --- Codex stream parser ---

# Codex emits status with PARENS for `(completed)` / `(failed)` but NO parens for `started`.
# Match both forms.
MCP_LINE_RE = re.compile(r'^mcp: ([^/\s]+)/(\S+)\s+\(?(started|completed|failed)\)?\s*$')
EXEC_LINE_RE = re.compile(r'^exec\s*$')
ZSH_LINE_RE = re.compile(r'^/bin/zsh -lc\s+(.+)$')
# Codex execpolicy rejection (CreateProcess-level deny by rules/default.rules `prefix_rule(forbidden)`):
# ERROR codex_core::tools::router: error=exec_command failed for `/bin/zsh -lc 'CMD'`: CreateProcess { message: "Rejected(\"... policy forbids commands starting with `VERB`\")" }
# Rust Debug-printed strings escape inner quotes as `\"` literally in the output stream.
# Match liberally: just look for the policy-forbids phrase + extract command + forbidden verb.
REJECT_LINE_RE = re.compile(
    r'error=exec_command failed for `(?P<cmd>[^`]*)`.*?policy forbids commands starting with `(?P<verb>[^`]+)`'
)


def parse_codex_stream(err_path):
    """Parse codex-exec .err for mcp: + exec/zsh markers + execpolicy rejections.
    Returns (calls, denials) — calls = successful tool invocations, denials = execpolicy rejections.
    """
    calls = []
    denials = []
    try:
        text = err_path.read_text(encoding="utf-8")
    except Exception:
        return calls, denials
    lines = text.splitlines()
    i = 0
    n = len(lines)
    while i < n:
        line = lines[i]
        # codex execpolicy rejection (CreateProcess deny) — appears as a single ERROR line
        rj = REJECT_LINE_RE.search(line)
        if rj:
            cmd = rj.group("cmd").strip()
            verb = rj.group("verb").strip()
            # Strip outer `/bin/zsh -lc '...'` wrapper to expose the actual command
            m_zsh = re.match(r"^/bin/zsh -lc\s+'([^']*)'$", cmd)
            inner_cmd = m_zsh.group(1) if m_zsh else cmd
            brief = f"REJECTED-by-execpolicy cmd={inner_cmd[:80]} forbidden_verb={verb}"
            denials.append({
                "raw_name": "Bash",
                "category": "forbidden_calls",
                "input_brief": brief,
            })
            i += 1
            continue
        # MCP marker
        m = MCP_LINE_RE.match(line)
        if m:
            server, tool, status = m.group(1), m.group(2), m.group(3)
            # Emit one entry per call lifecycle — use the FIRST sighting ("started").
            # Completed / failed adjust the status. Dedup by (server, tool, line-index).
            if status == "started":
                calls.append({
                    "raw_name": f"mcp: {server}/{tool}",
                    "category": categorize_codex_mcp(server),
                    "input_brief": f"server={server} tool={tool} status=pending",
                })
            else:
                # Update the most recent matching started entry, if any
                for c in reversed(calls):
                    if c["raw_name"] == f"mcp: {server}/{tool}" and "pending" in c["input_brief"]:
                        c["input_brief"] = f"server={server} tool={tool} status={status}"
                        break
                else:
                    # No prior started; record as standalone
                    calls.append({
                        "raw_name": f"mcp: {server}/{tool}",
                        "category": categorize_codex_mcp(server),
                        "input_brief": f"server={server} tool={tool} status={status}",
                    })
            i += 1
            continue
        # Bash exec: `exec` on its own line, then `/bin/zsh -lc <command>`
        if EXEC_LINE_RE.match(line):
            i += 1
            if i < n:
                z = ZSH_LINE_RE.match(lines[i].strip())
                if z:
                    cmd = z.group(1).strip()
                    # Strip a trailing "in <dir>" suffix that codex adds
                    cmd = re.sub(r'\s+in\s+/\S+$', '', cmd)
                    # Strip outer wrapping quotes
                    if (cmd.startswith("'") and cmd.endswith("'")) or (cmd.startswith('"') and cmd.endswith('"')):
                        cmd = cmd[1:-1]
                    brief = cmd if len(cmd) <= 80 else cmd[:77] + "..."
                    calls.append({
                        "raw_name": "Bash",
                        "category": "bash_calls",
                        "input_brief": brief,
                    })
            i += 1
            continue
        i += 1
    return calls, denials


# --- Claude -p / ollama JSON envelope parser ---

def parse_claude_envelope(out_path):
    """Parse claude -p / ollama --output-format json envelope.
    Returns (calls, denials, usage_summary).

    Note: under --output-format json, successful tool_use stream is NOT captured. Only
    final result + permission_denials + usage are available. To capture full tool_use
    stream, dispatcher needs --output-format stream-json (follow-up).
    """
    calls = []
    denials = []
    usage_summary = {}
    try:
        text = out_path.read_text(encoding="utf-8")
        envelope = json.loads(text)
    except Exception:
        return calls, denials, usage_summary
    for d in envelope.get("permission_denials", []) or []:
        tool_name = d.get("tool_name", "?")
        tool_input = d.get("tool_input", {}) or {}
        cmd = tool_input.get("command", "") or ""
        if not cmd:
            cmd = json.dumps(tool_input)[:80] if tool_input else ""
        brief = f"DENIED tool_name={tool_name} input={cmd[:80]}"
        denials.append({
            "raw_name": tool_name,
            "category": "forbidden_calls",
            "input_brief": brief,
        })
    usage = envelope.get("usage", {}) or {}
    usage_summary = {
        "input_tokens": usage.get("input_tokens", "?"),
        "output_tokens": usage.get("output_tokens", "?"),
        "duration_ms": envelope.get("duration_ms", "?"),
        "num_turns": envelope.get("num_turns", "?"),
        "terminal_reason": envelope.get("terminal_reason", "?"),
        "is_error": envelope.get("is_error", "?"),
    }
    return calls, denials, usage_summary


# --- TOON rendering (mirrors hook's render_toon shape) ---

def render_toon(meta, calls, denials, usage_summary, source_type):
    lines = []
    lines.append("agent_audit:")
    lines.append(f"  source: {source_type}")
    lines.append(f"  run: {meta.get('run', '?')}")
    lines.append(f"  role: {meta.get('role', '?')}")
    lines.append(f"  backend: {meta.get('backend', '?')}")
    lines.append(f"  model: {meta.get('model', '?')}")
    lines.append(f"  served_by: {meta.get('served_by', '?')}")
    lines.append(f"  ts: {meta.get('ts', '?')}")
    if usage_summary:
        lines.append(f"  input_tokens: {usage_summary.get('input_tokens', '?')}")
        lines.append(f"  output_tokens: {usage_summary.get('output_tokens', '?')}")
        lines.append(f"  duration_ms: {usage_summary.get('duration_ms', '?')}")
        lines.append(f"  num_turns: {usage_summary.get('num_turns', '?')}")
        lines.append(f"  terminal_reason: {usage_summary.get('terminal_reason', '?')}")
    lines.append(f"  tool_calls_total: {len(calls)}")
    lines.append(f"  forbidden_denials: {len(denials)}")
    lines.append("")

    by_bucket = {b: [] for b in BUCKETS}
    for idx, c in enumerate(calls, start=1):
        bucket = c["category"]
        if bucket not in by_bucket:
            bucket = "other_calls"
        by_bucket[bucket].append({"idx": idx, "name": c["raw_name"], "input_brief": c["input_brief"]})

    for bucket in BUCKETS:
        items = by_bucket[bucket]
        n = len(items)
        if n == 0:
            lines.append(f"{bucket}[0]:")
        else:
            lines.append(f"{bucket}[{n}]{{idx,name,input_brief}}:")
            for it in items:
                ib = it["input_brief"].replace(",", ";").replace("\n", " ")
                if len(ib) > 120:
                    ib = ib[:117] + "..."
                lines.append(f"  {it['idx']},{it['name']},{ib}")
        lines.append("")

    if not denials:
        lines.append("forbidden_calls[0]:")
        lines.append("  (no permission_denials in this run; populated from claude -p envelope OR codex execpolicy rejections)")
    else:
        lines.append(f"forbidden_calls[{len(denials)}]{{idx,name,input_brief}}:")
        for idx, d in enumerate(denials, start=1):
            ib = d["input_brief"].replace(",", ";").replace("\n", " ")
            if len(ib) > 120:
                ib = ib[:117] + "..."
            lines.append(f"  {idx},{d['raw_name']},{ib}")
    lines.append("")

    lines.append("out_of_scope[0]:")
    lines.append("  (bin/sh path enforces persona allowlist at invoke time via --allowedTools / -c mcp_servers.*; not retro-audited here)")
    lines.append("")

    lines.append("totals:")
    for bucket in BUCKETS:
        short = bucket[:-len("_calls")] if bucket.endswith("_calls") else bucket
        lines.append(f"  {short}: {len(by_bucket[bucket])}")
    lines.append(f"  forbidden: {len(denials)}")
    lines.append("  out_of_scope: 0")
    return "\n".join(lines)


def main(argv):
    if len(argv) < 3 or argv[1] != "--run-base":
        print("Usage: agent-audit-toon.py --run-base <path-prefix>", file=sys.stderr)
        return 2
    base = Path(argv[2])
    meta_path = Path(str(base) + ".meta.json")
    try:
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
    except Exception as e:
        print(f"# audit error: cannot read meta {meta_path}: {e}", file=sys.stderr)
        return 1

    backend = meta.get("backend", "")
    tier = meta.get("tier", 1)
    # Reconstruct stdout/stderr paths from meta or fall back to convention
    audit_dir = base.parent
    out_name = meta.get("stdout", f"{base.name}.tier{tier}.{backend}.out")
    err_name = meta.get("stderr", f"{base.name}.tier{tier}.{backend}.err")
    out_path = audit_dir / out_name
    err_path = audit_dir / err_name

    calls = []
    denials = []
    usage_summary = {}

    if backend == "codex-exec":
        calls, denials = parse_codex_stream(err_path)
        source_type = f"codex-exec stream ({err_path.name})"
    elif backend in ("ollama-local", "ollama-cloud", "claude-native"):
        calls, denials, usage_summary = parse_claude_envelope(out_path)
        source_type = f"claude -p JSON envelope ({backend}; out={out_path.name})"
    else:
        print(f"# audit error: unknown backend {backend}", file=sys.stderr)
        return 1

    toon = render_toon(meta, calls, denials, usage_summary, source_type)
    print(toon)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
