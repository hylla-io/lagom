// Package wasmbin embeds the compiled lagom-core wasm module.
//
// The bytes are the release build of the lagom-wasm crate
// (cargo build --release --target wasm32-unknown-unknown -p lagom-wasm),
// refreshed by the repo's `just wasm` recipe. Embedding the .wasm via go:embed
// is what makes the Go binding `go get`-clean: there is no cgo, no separately
// installed binary, and no per-platform native archive — the pure-Go wazero
// runtime (see the parent package) interprets these bytes in-process.
package wasmbin

import _ "embed"

// WASM is the embedded lagom-core wasm module (the transport-less
// project/rewrite/merge/validate engine over the JSON memory ABI).
//
//go:embed lagom.wasm
var WASM []byte
