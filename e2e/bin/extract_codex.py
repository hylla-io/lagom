#!/usr/bin/env python3
"""Extract the verifiable facts from a codex `exec --json` transcript.

codex emits newline-delimited events; tool calls and messages arrive nested
under `item.completed` -> `.item`. We surface ONLY what proves lagom's concepts
from the agent's OWN actions (not its self-report): which branded servers/tools it
actually invoked, the exact arguments it sent, the result text it got back, and
the token usage. Output is one compact JSON object on stdout.

Usage: extract_codex.py <codex.events.jsonl>
"""
import json
import sys


def main() -> None:
    path = sys.argv[1]
    tool_calls = []
    servers = set()
    final_message = None
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
            et = ev.get("type")
            if et == "turn.completed":
                usage = ev.get("usage", {})
            item = ev.get("item") if et == "item.completed" else None
            if not item:
                continue
            it = item.get("type")
            if it == "mcp_tool_call":
                server = item.get("server")
                servers.add(server)
                result_text = None
                res = item.get("result") or {}
                for c in (res.get("content") or []):
                    if c.get("type") == "text":
                        result_text = c.get("text")
                        break
                tool_calls.append(
                    {
                        "server": server,
                        "tool": item.get("tool"),
                        "arguments": item.get("arguments"),
                        "result_text": result_text,
                        "error": item.get("error"),
                        "status": item.get("status"),
                    }
                )
            elif it == "agent_message":
                final_message = item.get("text")

    json.dump(
        {
            "tool_calls": tool_calls,
            "servers": sorted(s for s in servers if s),
            "final_message": final_message,
            "usage": usage,
        },
        sys.stdout,
        indent=2,
    )
    print()


if __name__ == "__main__":
    main()
