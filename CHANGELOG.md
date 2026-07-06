# Changelog

All notable changes to lagom. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions follow SemVer
(pre-1.0: breaking changes may land in minor versions).

## [Unreleased]

## [0.1.0] — unreleased

First release. One transport-less Rust core (`lagom-core`) behind four faces at
byte-identical capability parity (guarded by `parity/` in CI):

- **Engine**: `Policy` model; `project` / `rewrite` / `merge` (narrow-only —
  the sandbox) / `validate` (drift = refuse to serve); deterministic `mint` /
  `refire` with full provenance (`MintRecord`); the brandable one-call `Guard`.
- **CLI** (`lagom`): `serve` (stdio proxy over a spawned upstream, `--audit`
  JSONL trace), `validate`, `refire`, `emit`, `emit-skills`.
- **Bindings**: Python (`hylla-lagom` on PyPI, `import lagom`), Node/TS
  (`@hylla-io/lagom`, napi-rs, 5 platforms), Go
  (`github.com/hylla-io/lagom/go`, wasm+wazero, no cgo), Rust (the crates
  themselves).
- **Security**: `tools/call` rewritten or rejected before the upstream;
  monotonic narrowing enforced at merge; JSON-RPC batches rejected;
  unparseable downstream lines rejected (never forwarded unparsed); README
  documents the full security model and non-goals; `SECURITY.md` +
  `cargo deny` + `govulncheck` + Dependabot 72h-cooldown supply-chain guards.
- **Tests**: unit + property-style tables across the core; process-level CLI
  integration tests; real-foreign-upstream e2e (`just e2e`); cross-binding
  parity suite; Go mcp-go consumption examples; real-measurement bench
  (`bench/`).
