// Module for the lagom Go-binding examples. Separate from the parent lagom-go
// module on purpose: it pulls in mcp-go (the first real consumer's framework)
// so the binding module itself keeps wazero as its ONLY non-stdlib dependency
// (see go/README.md and CONTEXT.md "Go binding"). It consumes the in-repo
// lagom-go via a relative replace, so these examples exercise the real binding.
module github.com/hylla-io/lagom/go/examples

go 1.25.5

require (
	github.com/hylla-io/lagom/go v0.0.0
	github.com/mark3labs/mcp-go v0.55.1
)

require (
	github.com/google/jsonschema-go v0.4.2 // indirect
	github.com/google/uuid v1.6.0 // indirect
	github.com/santhosh-tekuri/jsonschema/v6 v6.0.2 // indirect
	github.com/spf13/cast v1.7.1 // indirect
	github.com/tetratelabs/wazero v1.9.0 // indirect
	github.com/yosida95/uritemplate/v3 v3.0.2 // indirect
	golang.org/x/text v0.14.0 // indirect
)

replace github.com/hylla-io/lagom/go => ../
