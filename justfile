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
