# Rust workspace with a `just` gate; Go bootstrap superseded

Status: accepted

ADR-0001 made lagom's core Rust. This repo was bootstrapped with Go scaffolding
(`go.mod`, `magefile.go`'s canonical 12-target gate, `cmd/lagom/main.go`, the
`ta-go-*` dispatched agents, Hylla-as-evidence discipline) — all of which assume
a Go module. That scaffolding was always "replace once the domain is decided"
(CLAUDE.md); the domain is now decided, so it is superseded:

- **Cargo workspace** (`Cargo.toml` + `crates/*`) replaces the Go module.
- **`just` replaces `mage` as the gate runner.** `mage` is a *Go* build tool:
  magefiles are Go programs compiled against a Go module and the `mage`
  dependency — there is no Go module here for them to live in. `just` is a
  language-agnostic command runner that needs no Go toolchain and wraps the
  cargo gate (`fmt-check` + `clippy -D warnings` + `test` + `build`, behind
  `just ci`). The canonical 12-target *names* in CLAUDE.md are a Go/mage
  convention; the Rust equivalent is cargo verbs behind thin `just` recipes.
- **`ta` MCP is retained** — work tracking is language-agnostic.
- **Hylla is dropped** as an evidence source for lagom's own code (it is
  Go-only). Use Read/Grep, `cargo doc`, and rust-analyzer instead.
- **The `ta-go-*` agents do not apply** to a Rust crate (their gates run the Go
  toolchain). Build work uses general-purpose agents (or future Rust-flavored
  agent defs) against `just ci`.

## Consequences

- CLAUDE.md is now substantially inaccurate for this project (it mandates mage,
  the Go toolchain ban, Hylla-first evidence, and the `ta-go-*` matrix). It needs
  a Rust-oriented rewrite; until then, this ADR + `SPEC.md` are authoritative for
  tooling.
- CI (`.github/workflows/ci.yml`) runs a Rust toolchain + `just ci`, not Go +
  mage.
