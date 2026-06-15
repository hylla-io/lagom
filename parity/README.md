# Cross-binding parity — the NO-DRIFT guard

`lagom-core` is the single source of behavior; every face (Rust core, Go-via-wasm,
Python, TypeScript) is a thin skin over it that crosses the boundary as
JSON-in/JSON-out. This guard proves they stay in **capability parity — zero
drift** (see `CLAUDE.md` "Binding parity & docs — NO DRIFT" and
`docs/SAND_LAGOM_HANDOFF.md` §9 "NO DRIFT").

## What it does

One shared fixture (`fixtures/cases.json`) of `project` / `rewrite` / `merge` /
`validate` / `mint` / `refire` cases — success **and** error variants — is run
through **every** face. The comparator asserts, per case:

1. identical ok/err **classification**, and
2. for ok cases, **byte-identical** result JSON.

A divergence on either is a **CRITICAL** drift bug and fails the gate
non-zero. (Error-message *framing* — `ValueError` vs JS `Error` vs
`lagom: <op>: <msg>` — is a documented, legitimate per-binding difference, so
err-case payload text is deliberately not byte-compared; only the classification
is.)

Why byte-identity holds: all faces serialize the same `lagom_core` types through
`serde_json` (the wasm, PyO3, and napi faces all marshal JSON strings; Go drives
the wasm engine's own bytes). `BTreeMap`-backed maps make key order stable, so
the guard also catches any serializer-level regression (e.g. the deliberate
field-order vs `Value`-alphabetical-order distinction between a mint record's
`resolved.policy` and its `dynamic_inputs`).

## Run it

```sh
just parity          # build + run all four faces, then compare (exits non-zero on drift)
just parity-compare  # re-run only the comparator over the last parity/out/*.json
```

Per-face runners (each writes `parity/out/<face>.json`):

```sh
just rust-parity     # canonical reference (lagom-core directly)
just go-parity       # lagom-go via the committed wasm blob (CGO_ENABLED=0)
just py-parity       # lagom-py wheel in a throwaway uv venv
just node-parity     # lagom-node napi addon
```

Note: `go-parity` embeds the **committed** `go/internal/wasmbin/lagom.wasm`. If
`lagom-core` changed, run `just wasm` first to refresh the blob, or the Go face
tests a stale engine (the same stale-blob caveat as `just go-test`, see
`FEATURES.md`).

## Layout

- `fixtures/cases.json` — the one shared fixture (the only place cases live).
- `runners/rust/` — reference runner (standalone crate; empty `[workspace]`).
- `go/parity/` — the Go runner (own module, relative-replace on `lagom-go`,
  mirrors `go/examples`; it must sit under `go/` for the relative replace).
- `runners/python/run.py`, `runners/node/run.mjs` — the binding runners.
- `runners/compare/` — the comparator (standalone crate); emits the verdict.

`parity/out/` is machine-generated (gitignored); everything else is the guard's
committed source.
