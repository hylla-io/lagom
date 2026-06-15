#!/usr/bin/env python3
"""Measure REAL tool-surface token savings of a lagom slim projection vs full,
counting through the actual Anthropic tokenizer (`count_tokens`, free).

Inputs are pairs of MCP `tools/list` captures (the id:2 result.tools array) taken
straight off `sand mcp` via wire-probe.sh — so the numbers are what an agent
actually pays for the surface sand serves. Method mirrors sand's bench
(bench/sand_bench.py): tool-token cost = count(messages+tools) - count(messages).
Dotted tool names are sanitized to ^[A-Za-z0-9_-]{1,64}$ for the count only.

Writes e2e/bench/{results.jsonl,results.csv,REPORT.md}. Needs ANTHROPIC_API_KEY.

Usage: bench_savings.py <label> <full.tools.jsonl> <slim.tools.jsonl> [<label> <full> <slim> ...]
"""
import csv
import json
import os
import re
import sys
import urllib.request

API = "https://api.anthropic.com/v1/messages/count_tokens"
MODEL = "claude-haiku-4-5-20251001"  # same model sand's bench used -> comparable
KEY = os.environ["ANTHROPIC_API_KEY"]
OUT = os.path.join(os.path.dirname(__file__), "..", "bench")


def tools_from(path):
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("id") == 2 and ev.get("result", {}).get("tools") is not None:
                return ev["result"]["tools"]
    return []


def to_anthropic(mcp_tools):
    out = []
    for t in mcp_tools:
        name = re.sub(r"[^A-Za-z0-9_-]", "_", t["name"])[:64]
        out.append(
            {
                "name": name,
                "description": t.get("description") or "",
                "input_schema": t.get("inputSchema") or {"type": "object"},
            }
        )
    return out


def count(tools):
    body = json.dumps(
        {"model": MODEL, "messages": [{"role": "user", "content": "."}], "tools": tools}
    ).encode()
    req = urllib.request.Request(
        API,
        data=body,
        headers={
            "x-api-key": KEY,
            "anthropic-version": "2023-06-01",
            "content-type": "application/json",
        },
    )
    with urllib.request.urlopen(req) as r:
        return json.load(r)["input_tokens"]


def main():
    args = sys.argv[1:]
    triples = [args[i : i + 3] for i in range(0, len(args), 3)]
    os.makedirs(OUT, exist_ok=True)
    base = count([])  # message-only baseline
    rows = []
    for label, full_f, slim_f in triples:
        full_t = to_anthropic(tools_from(full_f))
        slim_t = to_anthropic(tools_from(slim_f))
        full_tok = count(full_t) - base
        slim_tok = count(slim_t) - base
        saved = round(100 * (full_tok - slim_tok) / full_tok, 1) if full_tok else 0.0
        row = {
            "upstream": label,
            "full_tools": len(full_t),
            "slim_tools": len(slim_t),
            "full_tok": full_tok,
            "slim_tok": slim_tok,
            "saved_pct": saved,
        }
        rows.append(row)
        print(f"  {label}: {len(full_t)}->{len(slim_t)} tools, {full_tok}->{slim_tok} tok, {saved}% saved")

    with open(os.path.join(OUT, "results.jsonl"), "w", encoding="utf-8") as fh:
        fh.write(json.dumps({"_method": f"Anthropic count_tokens {MODEL}; tool cost = count(msg+tools)-count(msg); through `sand mcp` wire", "baseline_tok": base}) + "\n")
        for r in rows:
            fh.write(json.dumps(r) + "\n")
    with open(os.path.join(OUT, "results.csv"), "w", newline="", encoding="utf-8") as fh:
        w = csv.DictWriter(fh, fieldnames=list(rows[0].keys()))
        w.writeheader()
        w.writerows(rows)
    total_full = sum(r["full_tok"] for r in rows)
    total_slim = sum(r["slim_tok"] for r in rows)
    total_saved = round(100 * (total_full - total_slim) / total_full, 1) if total_full else 0.0
    with open(os.path.join(OUT, "REPORT.md"), "w", encoding="utf-8") as fh:
        fh.write("# lagom-via-sand — tool-surface token savings (REAL)\n\n")
        fh.write(f"Anthropic `count_tokens` ({MODEL}); tool cost = count(msg+tools) - count(msg, baseline={base}). Surfaces captured off `sand mcp` (wire-probe.sh).\n\n")
        fh.write("| upstream | tools full→slim | full tok | slim tok | saved |\n|---|---|---|---|---|\n")
        for r in rows:
            fh.write(f"| {r['upstream']} | {r['full_tools']}→{r['slim_tools']} | {r['full_tok']} | {r['slim_tok']} | {r['saved_pct']}% |\n")
        fh.write(f"\n**Total: {total_saved}% saved** ({total_full}→{total_slim} tool tokens).\n")
    print(f"TOTAL: {total_saved}% saved ({total_full}->{total_slim}) -> e2e/bench/REPORT.md")


if __name__ == "__main__":
    main()
