# hylla-lagom

Python binding for [lagom](https://github.com/hylla-io/lagom) — serve a
**narrowed** projection of an upstream MCP server: fewer tools, pinned or hidden
arguments, constrained domains, slimmer docs, so an agent sees and can do just
enough for its job.

The distribution is `hylla-lagom` (the PyPI name `lagom` belongs to an unrelated
DI library); the module imports as `lagom`:

```sh
pip install hylla-lagom     # or: uv add hylla-lagom
```

```python
import lagom  # project, rewrite, merge, validate, mint, refire,
              # mint_stdio_server, PolicyBuilder, Guard, shipped_skills,
              # emit_skills
```

Tool defs are read leniently: pass the raw upstream `tools/list` straight in
(camelCase `inputSchema` or snake_case `input_schema`; a missing/`null` schema
normalizes to `{}`). Projected output is always canonical `input_schema`.

Highlights:

- **`Guard`** — the one-call helper: `Guard(upstream_json, policy_json)`, then
  `slim_defs()` for the projected `tools/list` and `gate(call)` to rewrite each
  incoming call back to upstream (raises `ValueError` on reject).
- **`mint` / `refire`** — deterministic per-agent ephemeral projections with
  recorded provenance; identical inputs mint a byte-identical record.
- **`mint_stdio_server`** — spawn the upstream and serve the projected surface
  on this process's stdio, with optional append-only audit logging.

Full documentation, the policy model, and the security model live in the
[repository README](https://github.com/hylla-io/lagom) and `SPEC.md`. MIT
licensed.
