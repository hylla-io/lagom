# lagom build gate — Rust workspace.
# Canonical gate is `just ci`. Never invoke the raw cargo toolchain in CI flows;
# route through these targets so the gate name is stable across the workspace.

set quiet

# List available targets.
default:
    @just --list

# Verify formatting without writing.
fmt-check:
    cargo fmt --all --check

# Apply formatting.
fmt:
    cargo fmt --all

# Lint; warnings are errors.
clippy:
    cargo clippy --all-targets --all-features -- -D warnings

# Run all tests.
test:
    cargo test --all

# Build all crates.
build:
    cargo build --all

# The gate: format check + lint + test + build.
ci: fmt-check clippy test build

# Build the Python binding wheel and run its smoke test in an isolated uv env.
# lagom-py is excluded from the default workspace build because the pyo3
# extension-module cdylib needs a Python interpreter to link; maturin supplies
# that. Requires `maturin` (`uv tool install maturin`) and `uv`.
#
# Steps: build an abi3 wheel -> create a throwaway venv -> install the wheel +
# pytest into it -> run the smoke test (which imports `lagom` and asserts
# project() drops a tool + pins an arg).
py:
    rm -rf crates/lagom-py/dist crates/lagom-py/.venv
    maturin build --release --manifest-path crates/lagom-py/Cargo.toml --out crates/lagom-py/dist
    uv venv crates/lagom-py/.venv
    # Install the built wheel by path (NOT by name): the PyPI name `lagom` is
    # already taken by an unrelated DI library, so resolving by name would fetch
    # the wrong package. pytest comes from the index.
    VIRTUAL_ENV=crates/lagom-py/.venv uv pip install crates/lagom-py/dist/*.whl pytest
    VIRTUAL_ENV=crates/lagom-py/.venv uv run --no-project pytest crates/lagom-py/tests -q

# Format + lint the excluded lagom-py crate (not covered by the workspace gate).
py-check:
    cargo fmt --manifest-path crates/lagom-py/Cargo.toml --check
    # NOT --all-features: enabling `extension-module` makes the clippy test
    # binary fail to link libpython. Default features keep it linkable.
    cargo clippy --manifest-path crates/lagom-py/Cargo.toml --all-targets -- -D warnings

# Rust unit tests for the binding (runs against the rlib, no interpreter needed).
py-test:
    cargo test --manifest-path crates/lagom-py/Cargo.toml

# Build the wasm32 face of lagom-core (the Go binding's embedded engine) and
# refresh the copy the Go module `go:embed`s.
#
# lagom-wasm is EXCLUDED from the default workspace (see root Cargo.toml): its
# JSON memory ABI is only meaningful on wasm32, so building it into
# `cargo build --all` on the host triple is pointless and would drag wasm-shaped
# alloc/panic surface into the native gate. It builds separately here.
#
# Requires the wasm target: `rustup target add wasm32-unknown-unknown`. The
# repo's rustc is Homebrew while the wasm std lives under rustup, so we drive the
# rustup `stable` toolchain explicitly (its cargo + rustc) to avoid the Homebrew
# rustc shadowing the cross-target std.
wasm:
    RUSTC="$(rustup which --toolchain stable rustc)" rustup run stable cargo build --release --target wasm32-unknown-unknown -p lagom-wasm --manifest-path crates/lagom-wasm/Cargo.toml
    cp crates/lagom-wasm/target/wasm32-unknown-unknown/release/lagom_wasm.wasm go/internal/wasmbin/lagom.wasm

# Test the Go binding in-process via wazero (pure Go, no cgo). Proves project()
# drops a tool + pins/hides an arg, and that the module is `go get`-clean: the
# CGO_ENABLED=0 run fails if any cgo crept in. Embeds the committed
# go/internal/wasmbin/lagom.wasm (refresh it with `just wasm`).
go-test:
    cd go && go mod tidy
    cd go && CGO_ENABLED=0 go vet ./...
    cd go && CGO_ENABLED=0 go test -count=1 ./...

# Build the Node/TypeScript binding (napi-rs `.node` addon + generated
# index.js/index.d.ts) into the crate folder.
#
# lagom-node is EXCLUDED from the default workspace (see root Cargo.toml): it is a
# napi-rs cdylib built by the napi CLI, not by `cargo build --all`, so keeping it
# out keeps `just ci` green on machines without the napi toolchain (mirrors
# lagom-py / lagom-wasm). The napi CLI is fetched on demand via `npx` — no global
# install required (Node + npm must be present). `--platform` emits the
# platform-tagged `.node` plus the `index.js`/`index.d.ts` the package ships.
node-build:
    cd crates/lagom-node && npx -y -p @napi-rs/cli@3 napi build --platform --release --manifest-path Cargo.toml

# Build the Node binding then run its smoke test (mirrors `just py`): import the
# addon from Node and assert project() drops a tool + pins/hides an arg and
# rewrite() rejects an out-of-enum call. Uses Node's built-in test runner, so no
# extra npm test deps are installed.
node-test: node-build
    cd crates/lagom-node && node --test '__test__/**/*.test.mjs'

# Drive the lagom-go consumption examples (a branded mcp-go server + multi-agent
# ephemeral profiles + multi-upstream composition) through the mcp-go in-process
# client. Lives in its own module (go/examples) so the binding module keeps
# wazero as its ONLY non-stdlib dep; this module pulls in mcp-go. Embeds the
# committed go/internal/wasmbin/lagom.wasm (refresh it with `just wasm`). Runs
# under CGO_ENABLED=0 to keep the go-get-clean (no-cgo) guarantee enforced.
examples:
    cd go/examples && go mod tidy
    cd go/examples && test -z "$(gofmt -l .)" || { echo "gofmt: files need formatting:"; gofmt -l .; exit 1; }
    cd go/examples && CGO_ENABLED=0 go vet ./...
    cd go/examples && CGO_ENABLED=0 go test -count=1 ./...

# Format + lint the excluded lagom-node crate (not covered by the workspace gate).
# Clippy is scoped to `--lib`: a napi cdylib's N-API symbols (`napi_*`) are
# resolved by the Node runtime at addon-load time and are NOT present when cargo
# links a standalone test/bench binary, so `--all-targets` would fail to link.
# Linting the library surface catches the binding code without that link step.
node-check:
    cargo fmt --manifest-path crates/lagom-node/Cargo.toml --check
    cargo clippy --manifest-path crates/lagom-node/Cargo.toml --lib -- -D warnings

# Host-side Rust unit tests for the Node binding (the `#[cfg(test)]` mirror of the
# Node smoke test). A napi cdylib's N-API symbols are provided by Node at runtime,
# so the test binary must be linked with undefined-symbol lookup deferred to load
# time (`-undefined dynamic_lookup` on macOS clang). The headline parity gate is
# `just node-test` (real Node import); this recipe is the faster inner-loop mirror.
node-rust-test:
    RUSTFLAGS="-C link-arg=-undefined -C link-arg=dynamic_lookup" cargo test --manifest-path crates/lagom-node/Cargo.toml

# ---------------------------------------------------------------------------
# Cross-binding parity — the NO-DRIFT guard.
#
# One shared fixture (parity/fixtures/cases.json) is run through every face
# (Rust core, Go-via-wasm, Python, TS) and the comparator asserts byte-identical
# ok-JSON outputs + identical ok/err classification. ANY behavioral difference
# across bindings is a CRITICAL drift. `just parity` runs the full matrix; each
# `*-parity` recipe runs one face's runner and writes its result map to
# parity/out/<face>.json.
# ---------------------------------------------------------------------------

# Absolute fixture path so every runner (each `cd`-ing into its own toolchain
# dir) reads the same committed fixture.
fixture := justfile_directory() / "parity/fixtures/cases.json"
parity_out := justfile_directory() / "parity/out"

# Rust core face (the canonical reference). Standalone crate -> parity/out/rust.json.
rust-parity:
    mkdir -p {{parity_out}}
    cargo run --quiet --manifest-path parity/runners/rust/Cargo.toml -- {{fixture}} > {{parity_out}}/rust.json

# Go-via-wasm face. Own module (consumes lagom-go) -> parity/out/go.json.
# CGO_ENABLED=0 keeps the go-get-clean (no-cgo) guarantee enforced, like go-test.
go-parity:
    mkdir -p {{parity_out}}
    cd go/parity && go mod tidy
    cd go/parity && CGO_ENABLED=0 go run . {{fixture}} > {{parity_out}}/go.json

# Python (PyO3) face. Builds the wheel into a throwaway venv (like `just py`),
# then runs the fixture through the imported `lagom` -> parity/out/python.json.
py-parity:
    mkdir -p {{parity_out}}
    rm -rf crates/lagom-py/dist crates/lagom-py/.venv
    maturin build --release --manifest-path crates/lagom-py/Cargo.toml --out crates/lagom-py/dist
    uv venv crates/lagom-py/.venv
    VIRTUAL_ENV=crates/lagom-py/.venv uv pip install crates/lagom-py/dist/*.whl
    VIRTUAL_ENV=crates/lagom-py/.venv uv run --no-project python parity/runners/python/run.py {{fixture}} > {{parity_out}}/python.json

# Node/TS (napi-rs) face. Builds the addon (like `just node-build`), then runs
# the fixture through the imported addon -> parity/out/node.json.
node-parity: node-build
    mkdir -p {{parity_out}}
    node parity/runners/node/run.mjs {{fixture}} crates/lagom-node > {{parity_out}}/node.json

# The full NO-DRIFT matrix: run all four faces, then compare. Exits non-zero on
# any drift. Needs the wasm blob fresh for Go — run `just wasm` first if lagom-core
# changed (the committed go/internal/wasmbin/lagom.wasm is what go-parity embeds).
parity: rust-parity go-parity py-parity node-parity
    cargo run --quiet --manifest-path parity/runners/compare/Cargo.toml -- \
        rust={{parity_out}}/rust.json \
        go={{parity_out}}/go.json \
        python={{parity_out}}/python.json \
        node={{parity_out}}/node.json

# Comparator only (assumes parity/out/*.json already produced) — fast re-verdict.
parity-compare:
    cargo run --quiet --manifest-path parity/runners/compare/Cargo.toml -- \
        rust={{parity_out}}/rust.json \
        go={{parity_out}}/go.json \
        python={{parity_out}}/python.json \
        node={{parity_out}}/node.json
