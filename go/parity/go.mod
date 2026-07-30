// Module for the lagom Go-binding parity runner (the NO-DRIFT cross-binding
// guard). Separate from the parent lagom-go module on purpose, exactly like
// go/examples: it is a consumer of the binding, so keeping it out of the binding
// module preserves wazero as lagom-go's ONLY non-stdlib dependency. It consumes
// the in-repo lagom-go via a relative replace, so it exercises the real binding
// + the committed wasm engine.
module github.com/hylla-io/lagom/go/parity

go 1.25.0

require github.com/hylla-io/lagom/go v0.0.0

require (
	github.com/tetratelabs/wazero v1.12.0 // indirect
	golang.org/x/sys v0.44.0 // indirect
)

replace github.com/hylla-io/lagom/go => ../
