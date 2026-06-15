// Command lagom-parity-go is the Go-via-wasm face runner of the cross-binding
// NO-DRIFT parity guard. It reads the shared fixture
// (parity/fixtures/cases.json), runs every case through the real lagom-go
// binding (the committed wasm engine via wazero), and prints a deterministic
// result map to stdout:
//
//	{"<case>": {"status": "ok"|"err", "payload": <json-string|null>}, ...}
//
// payload for an ok case is the exact JSON the engine emitted (the byte string
// every face must match); for an err case it is null. The comparator diffs this
// against the Rust reference. Run via `just parity` (which passes the absolute
// fixture path as argv[1]).
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"sort"

	lagom "github.com/hylla-io/lagom/go"
)

// fixture mirrors the committed parity fixture shape.
type fixture struct {
	Upstream json.RawMessage `json:"upstream"`
	Cases    []caseSpec      `json:"cases"`
}

// caseSpec is one parity case. Optional fields are nil when absent.
type caseSpec struct {
	Name            string          `json:"name"`
	Op              string          `json:"op"`
	Policy          json.RawMessage `json:"policy"`
	Call            json.RawMessage `json:"call"`
	Base            json.RawMessage `json:"base"`
	Overlay         json.RawMessage `json:"overlay"`
	RunID           string          `json:"run_id"`
	Dynamic         json.RawMessage `json:"dynamic"`
	UpstreamCommand json.RawMessage `json:"upstream_command"`
	MintOf          string          `json:"mint_of"`
}

// result is the per-case outcome: an ok payload (the engine's JSON) or an err.
type result struct {
	Status  string  `json:"status"`
	Payload *string `json:"payload"`
}

func ok(payload []byte) result {
	s := string(payload)
	return result{Status: "ok", Payload: &s}
}

func errResult() result {
	return result{Status: "err", Payload: nil}
}

// isNull reports whether a raw JSON value is absent or the literal null, so a
// `"dynamic": null` overlay is treated as no overlay (parity with the others).
func isNull(b json.RawMessage) bool {
	return len(b) == 0 || string(b) == "null"
}

func mintRecord(ctx context.Context, c caseSpec) ([]byte, error) {
	var dyn []byte
	if !isNull(c.Dynamic) {
		dyn = c.Dynamic
	}
	return lagom.Mint(ctx, c.RunID, c.Base, dyn, c.UpstreamCommand)
}

func runCase(ctx context.Context, c caseSpec, byName map[string]caseSpec, upstream []byte) result {
	switch c.Op {
	case "project":
		out, err := lagom.Project(ctx, upstream, c.Policy)
		if err != nil {
			return errResult()
		}
		return ok(out)
	case "rewrite":
		out, err := lagom.Rewrite(ctx, c.Call, c.Policy)
		if err != nil {
			return errResult()
		}
		return ok(out)
	case "merge":
		out, err := lagom.Merge(ctx, c.Base, c.Overlay)
		if err != nil {
			return errResult()
		}
		return ok(out)
	case "validate":
		if err := lagom.Validate(ctx, c.Policy, upstream); err != nil {
			return errResult()
		}
		// Match the canonical face: validate ok serializes as the JSON null.
		return ok([]byte("null"))
	case "mint":
		out, err := mintRecord(ctx, c)
		if err != nil {
			return errResult()
		}
		return ok(out)
	case "refire":
		src, found := byName[c.MintOf]
		if !found {
			panic(fmt.Sprintf("refire references unknown case %q", c.MintOf))
		}
		record, err := mintRecord(ctx, src)
		if err != nil {
			panic(fmt.Sprintf("referenced mint %q must succeed: %v", c.MintOf, err))
		}
		out, err := lagom.Refire(ctx, record)
		if err != nil {
			return errResult()
		}
		return ok(out)
	default:
		panic(fmt.Sprintf("unknown op %q", c.Op))
	}
}

func main() {
	path := "parity/fixtures/cases.json"
	if len(os.Args) > 1 {
		path = os.Args[1]
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		fmt.Fprintf(os.Stderr, "read fixture %s: %v\n", path, err)
		os.Exit(1)
	}
	var fx fixture
	if err := json.Unmarshal(raw, &fx); err != nil {
		fmt.Fprintf(os.Stderr, "parse fixture: %v\n", err)
		os.Exit(1)
	}

	ctx := context.Background()
	byName := make(map[string]caseSpec, len(fx.Cases))
	for _, c := range fx.Cases {
		byName[c.Name] = c
	}

	out := make(map[string]result, len(fx.Cases))
	for _, c := range fx.Cases {
		out[c.Name] = runCase(ctx, c, byName, fx.Upstream)
	}

	// Emit sorted, indented (matches the Rust reference's pretty layout).
	names := make([]string, 0, len(out))
	for n := range out {
		names = append(names, n)
	}
	sort.Strings(names)
	ordered := make(map[string]result, len(out))
	for _, n := range names {
		ordered[n] = out[n]
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(ordered); err != nil {
		fmt.Fprintf(os.Stderr, "encode results: %v\n", err)
		os.Exit(1)
	}
}
