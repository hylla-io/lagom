#!/usr/bin/env python3
"""lagom token-savings benchmark.

For each real MCP server: drive `lagom serve` to capture the FULL projected
tools/list (passthrough policy) and SLIM variants, then measure the REAL Claude
token cost of each tool surface with Anthropic's `count_tokens` API. Logs every
data point (raw tools, policy, token counts) to bench/results.jsonl + a CSV +
REPORT.md so the numbers are reproducible and auditable.

Run:  python3 bench/bench.py
Env:  ANTHROPIC_API_KEY (real Anthropic key; count_tokens is free).
"""
import json, os, select, subprocess, sys, time, urllib.request, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
LAGOM = str(ROOT / "target" / "debug" / "lagom")
OUT = ROOT / "bench"
RAW = OUT / "raw"
RAW.mkdir(parents=True, exist_ok=True)
MODEL = "claude-haiku-4-5-20251001"
BASE = os.environ.get("ANTHROPIC_BASE_URL", "https://api.anthropic.com").rstrip("/")
KEY = os.environ["ANTHROPIC_API_KEY"]

# A broad set of real, popular MCP servers (npx + uvx). Servers that need creds
# or can't start headless are skipped AND logged (status:"skipped") — never
# silently dropped, so the corpus is honest about coverage.
SERVERS = [
    ("everything", ["npx", "-y", "@modelcontextprotocol/server-everything"]),
    ("filesystem", ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/tmp"]),
    ("memory", ["npx", "-y", "@modelcontextprotocol/server-memory"]),
    ("sequential-thinking", ["npx", "-y", "@modelcontextprotocol/server-sequential-thinking"]),
    ("git", ["uvx", "mcp-server-git"]),
    ("fetch", ["uvx", "mcp-server-fetch"]),
    ("time", ["uvx", "mcp-server-time"]),
    ("sqlite", ["uvx", "mcp-server-sqlite", "--db-path", "/tmp/lagom-bench.db"]),
    ("github", ["npx", "-y", "@modelcontextprotocol/server-github"]),
    ("brave-search", ["npx", "-y", "@modelcontextprotocol/server-brave-search"]),
    ("puppeteer", ["npx", "-y", "@modelcontextprotocol/server-puppeteer"]),
    ("slack", ["npx", "-y", "@modelcontextprotocol/server-slack"]),
]
KEEP_K = 2  # the slim "this agent needs only K tools" allowlist size


def capture_tools(upstream, policy_path, timeout=60):
    """Spawn lagom serve over `upstream` with `policy_path`; return projected tools."""
    proc = subprocess.Popen(
        [LAGOM, "serve", "--config", policy_path, "--", *upstream],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    )
    try:
        # lagom already initialized the upstream during its own startup probe, so
        # forward tools/list directly (no downstream re-init).
        proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}) + "\n")
        proc.stdin.flush()
        deadline = time.time() + timeout
        while time.time() < deadline:
            if not select.select([proc.stdout], [], [], deadline - time.time())[0]:
                break
            line = proc.stdout.readline()
            if not line:
                break
            try:
                m = json.loads(line)
            except Exception:
                continue
            if m.get("id") == 2 and "result" in m:
                return m["result"].get("tools", [])
        return None
    finally:
        proc.kill()


def to_anthropic_tools(mcp_tools):
    out = []
    for t in mcp_tools:
        out.append({
            "name": t.get("name", "x"),
            "description": t.get("description", "") or "",
            "input_schema": t.get("inputSchema") or {"type": "object", "properties": {}},
        })
    return out


def count_tokens(tools):
    body = json.dumps({
        "model": MODEL,
        "messages": [{"role": "user", "content": "."}],
        "tools": tools,
    }).encode()
    req = urllib.request.Request(
        f"{BASE}/v1/messages/count_tokens", data=body,
        headers={"x-api-key": KEY, "anthropic-version": "2023-06-01", "content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)["input_tokens"]


def write_policy(path, text):
    pathlib.Path(path).write_text(text)


def main():
    base_tokens = count_tokens([])  # message-only baseline (subtract to isolate tools)
    results = []
    tmp = RAW / "policies"
    tmp.mkdir(exist_ok=True)
    passthrough = str(tmp / "passthrough.toml")
    write_policy(passthrough, 'default-presence = "keep"\n')

    for name, cmd in SERVERS:
        print(f"[{name}] capturing full surface…", flush=True)
        full = capture_tools(cmd, passthrough)
        if not full:
            print(f"[{name}] SKIP (no tools / unreachable)", flush=True)
            results.append({"server": name, "status": "skipped"})
            continue
        names = [t["name"] for t in full]
        keep = names[:KEEP_K]

        # slim-allowlist: sealed + keep K tools (drop the rest).
        allow = 'default-presence = "drop"\n' + "".join(f'[tools."{k}"]\npresence = "keep"\n' for k in keep)
        allow_path = str(tmp / f"{name}.allow.toml")
        write_policy(allow_path, allow)
        slim = capture_tools(cmd, allow_path)

        # slim-allowlist + caveman descriptions on the kept tools.
        cave = 'default-presence = "drop"\n' + "".join(
            f'[tools."{k}"]\npresence = "keep"\ndescription = "{k} tool"\n' for k in keep)
        cave_path = str(tmp / f"{name}.caveman.toml")
        write_policy(cave_path, cave)
        slimc = capture_tools(cmd, cave_path)

        (RAW / f"{name}.full.json").write_text(json.dumps(full, indent=2))
        if slim: (RAW / f"{name}.slim.json").write_text(json.dumps(slim, indent=2))

        tf = count_tokens(to_anthropic_tools(full)) - base_tokens
        ts = (count_tokens(to_anthropic_tools(slim)) - base_tokens) if slim else None
        tc = (count_tokens(to_anthropic_tools(slimc)) - base_tokens) if slimc else None
        row = {
            "server": name, "status": "ok",
            "tools_full": len(full), "tools_slim": len(slim) if slim else None,
            "kept": keep,
            "tool_tokens_full": tf,
            "tool_tokens_slim_allowlist": ts,
            "tool_tokens_slim_caveman": tc,
            "saved_pct_allowlist": round(100 * (tf - ts) / tf, 1) if ts is not None and tf else None,
            "saved_pct_caveman": round(100 * (tf - tc) / tf, 1) if tc is not None and tf else None,
            "model": MODEL, "method": "anthropic count_tokens (tool defs - empty baseline)",
            "base_tokens": base_tokens,
        }
        results.append(row)
        print(f"[{name}] full={len(full)}t/{tf}tok  slim={row['tools_slim']}t/{ts}tok "
              f"({row['saved_pct_allowlist']}% saved)  caveman={tc}tok ({row['saved_pct_caveman']}%)", flush=True)

    ts_stamp = subprocess.run(["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"], capture_output=True, text=True).stdout.strip()
    (OUT / "results.jsonl").write_text("".join(json.dumps({**r, "ts": ts_stamp}) + "\n" for r in results))
    ok = [r for r in results if r.get("status") == "ok"]
    # CSV
    cols = ["server", "tools_full", "tools_slim", "tool_tokens_full", "tool_tokens_slim_allowlist",
            "tool_tokens_slim_caveman", "saved_pct_allowlist", "saved_pct_caveman"]
    csv = ",".join(cols) + "\n" + "\n".join(",".join(str(r.get(c, "")) for c in cols) for r in ok)
    (OUT / "results.csv").write_text(csv + "\n")
    # REPORT.md
    tot_full = sum(r["tool_tokens_full"] for r in ok)
    tot_slim = sum(r["tool_tokens_slim_allowlist"] for r in ok if r["tool_tokens_slim_allowlist"] is not None)
    rep = [f"# lagom token-savings benchmark\n",
           f"Measured {ts_stamp} with Anthropic `count_tokens` (model `{MODEL}`); tool-def tokens = "
           f"count(message+tools) − count(message-only baseline={base_tokens}). Slim = sealed allowlist "
           f"keeping {KEEP_K} tools; caveman = + terse description override. Raw tool surfaces in `bench/raw/`.\n",
           "| server | tools full→slim | full tok | slim(allowlist) | slim(caveman) | saved (allow / caveman) |",
           "|---|---|---|---|---|---|"]
    for r in ok:
        rep.append(f"| {r['server']} | {r['tools_full']}→{r['tools_slim']} | {r['tool_tokens_full']} | "
                   f"{r['tool_tokens_slim_allowlist']} | {r['tool_tokens_slim_caveman']} | "
                   f"{r['saved_pct_allowlist']}% / {r['saved_pct_caveman']}% |")
    if tot_full:
        rep.append(f"\n**Totals (allowlist):** {tot_full} → {tot_slim} tool tokens — "
                   f"**{round(100*(tot_full-tot_slim)/tot_full,1)}% saved** across {len(ok)} servers.")
    (OUT / "REPORT.md").write_text("\n".join(rep) + "\n")
    print("\n".join(rep))


if __name__ == "__main__":
    main()
