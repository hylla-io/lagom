# lagom token-savings benchmark

Measured 2026-06-15T03:57:52Z with Anthropic `count_tokens` (model `claude-haiku-4-5-20251001`); tool-def tokens = count(message+tools) − count(message-only baseline=8). Slim = sealed allowlist keeping 2 tools; caveman = + terse description override. Raw tool surfaces in `bench/raw/`.

| server | tools full→slim | full tok | slim(allowlist) | slim(caveman) | saved (allow / caveman) |
|---|---|---|---|---|---|
| everything | 13→2 | 2040 | 729 | 719 | 64.3% / 64.8% |
| filesystem | 14→2 | 2590 | 861 | 752 | 66.8% / 71.0% |
| memory | 9→2 | 1710 | 853 | 836 | 50.1% / 51.1% |
| sequential-thinking | 1→1 | 1539 | 1539 | 874 | 0.0% / 43.2% |
| everything-2 | 13→2 | 2040 | 729 | 719 | 64.3% / 64.8% |

**Totals (allowlist):** 9919 → 4711 tool tokens — **52.5% saved** across 5 servers.
