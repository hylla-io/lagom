#!/usr/bin/env python3
"""Extract the verifiable facts from a `claude -p --output-format stream-json` log.

Surfaces what proves (or disproves) confinement from the agent's OWN transcript:
the MCP servers/tools offered at init (exposing the async-connect race: a server
may still be `pending` with zero mcp tools when the turn starts), every tool_use
the agent issued, the tool_result text it got back, and the final result + usage.

Usage: extract_claude.py <claude.events.jsonl>
"""
import json
import sys


def main() -> None:
    path = sys.argv[1]
    init = {}
    tool_calls = []
    tool_results = []
    final_result = None
    usage = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            t = ev.get("type")
            if t == "system" and ev.get("subtype") == "init":
                tools = ev.get("tools") or []
                init = {
                    "mcp_servers": ev.get("mcp_servers"),
                    "mcp_tools_at_init": [x for x in tools if str(x).startswith("mcp__")],
                }
            elif t == "assistant":
                for c in (ev.get("message", {}).get("content") or []):
                    if c.get("type") == "tool_use":
                        tool_calls.append({"name": c.get("name"), "input": c.get("input")})
            elif t == "user":
                for c in (ev.get("message", {}).get("content") or []):
                    if c.get("type") == "tool_result":
                        tool_results.append(str(c.get("content"))[:300])
            elif t == "result":
                final_result = ev.get("result")
                usage = ev.get("usage", {})

    json.dump(
        {
            "init": init,
            "tool_calls": tool_calls,
            "tool_results": tool_results,
            "final_result": final_result,
            "usage": usage,
        },
        sys.stdout,
        indent=2,
    )
    print()


if __name__ == "__main__":
    main()
