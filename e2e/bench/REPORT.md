# lagom-via-sand — tool-surface token savings (REAL)

Anthropic `count_tokens` (claude-haiku-4-5-20251001); tool cost = count(msg+tools) - count(msg, baseline=8). Surfaces captured off `sand mcp` (wire-probe.sh).

| upstream | tools full→slim | full tok | slim tok | saved |
|---|---|---|---|---|
| fast-mcp | 2→1 | 608 | 561 | 7.7% |
| everything | 13→2 | 1812 | 652 | 64.0% |

**Total: 49.9% saved** (2420→1213 tool tokens).
