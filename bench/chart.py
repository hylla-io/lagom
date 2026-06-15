#!/usr/bin/env python3
"""Render bench/results.jsonl as a dependency-free SVG bar chart (full vs slim
tool tokens per server). Run: python3 bench/chart.py -> bench/savings.svg"""
import json, pathlib

OUT = pathlib.Path(__file__).resolve().parent
rows = [json.loads(l) for l in (OUT / "results.jsonl").read_text().splitlines() if l.strip()]
rows = [r for r in rows if r.get("status") == "ok"]

W, H, pad, top = 820, 420, 60, 40
maxtok = max(r["tool_tokens_full"] for r in rows) or 1
bw = (W - 2 * pad) / (len(rows) * 3)
svg = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="monospace" font-size="11">']
svg.append(f'<text x="{W/2}" y="22" text-anchor="middle" font-size="15">lagom: MCP tool-surface tokens — full vs slim (Anthropic count_tokens)</text>')
base = H - pad
for i, r in enumerate(rows):
    x0 = pad + i * 3 * bw + bw * 0.3
    full_h = (r["tool_tokens_full"] / maxtok) * (H - pad - top)
    slim = r.get("tool_tokens_slim_allowlist") or r["tool_tokens_full"]
    slim_h = (slim / maxtok) * (H - pad - top)
    svg.append(f'<rect x="{x0:.0f}" y="{base-full_h:.0f}" width="{bw:.0f}" height="{full_h:.0f}" fill="#c0392b"/>')
    svg.append(f'<rect x="{x0+bw:.0f}" y="{base-slim_h:.0f}" width="{bw:.0f}" height="{slim_h:.0f}" fill="#27ae60"/>')
    svg.append(f'<text x="{x0+bw:.0f}" y="{base-full_h-6:.0f}" text-anchor="middle" fill="#c0392b">{r["tool_tokens_full"]}</text>')
    svg.append(f'<text x="{x0+bw:.0f}" y="{base-slim_h-6:.0f}" text-anchor="middle" fill="#27ae60">{slim}</text>')
    pct = r.get("saved_pct_allowlist")
    svg.append(f'<text x="{x0+bw:.0f}" y="{base+14:.0f}" text-anchor="middle">{r["server"][:14]}</text>')
    svg.append(f'<text x="{x0+bw:.0f}" y="{base+28:.0f}" text-anchor="middle" fill="#27ae60">-{pct}%</text>')
svg.append(f'<rect x="{pad}" y="{top-14}" width="11" height="11" fill="#c0392b"/><text x="{pad+16}" y="{top-4}">full surface</text>')
svg.append(f'<rect x="{pad+110}" y="{top-14}" width="11" height="11" fill="#27ae60"/><text x="{pad+126}" y="{top-4}">lagom slim (allowlist)</text>')
svg.append("</svg>")
(OUT / "savings.svg").write_text("\n".join(svg))
print("wrote", OUT / "savings.svg")
