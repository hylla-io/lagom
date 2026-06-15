// Package lagom is the Go binding face of lagom-core (SPEC.md §2, §7.3,
// ADR-0001 "Go is the problem child").
//
// It exposes the transport-less projection engine — Project, Rewrite, Merge,
// Validate — as an in-process Go dependency consumed via `go get`, with NO cgo
// and NO separately installed binary. Native embedding of the Rust core into Go
// would need cgo + a C toolchain + per-platform archives; instead lagom-core is
// compiled to wasm32 and run via wazero (github.com/tetratelabs/wazero, a
// pure-Go wasm runtime). The .wasm is go:embed-ed, so consumers get the whole
// engine from a single `go get github.com/hylla-io/lagom/go`.
//
// wasm is pure compute: it cannot spawn the upstream child or do stdio, so the
// stdio proxy / minting faces stay native (the CLI and the Python
// mint_stdio_server). This binding is the project/rewrite/merge/validate
// (transport-less) face only (SPEC.md §7.3).
//
// # Marshalling
//
// Every operation crosses the wasm boundary as a UTF-8 JSON buffer in linear
// memory, mirroring the Python binding's JSON-in/JSON-out contract. Inputs and
// outputs are JSON because that keeps the binding decoupled from lagom-core's
// Rust types and identical across binding languages. An engine reject, a
// widening merge, or a drift failure surfaces as a Go error carrying the
// engine's own message (SPEC.md §9.1 — never swallow), never a silent empty
// result.
//
// # Concurrency
//
// A single wasm module instance is not safe for concurrent calls (it shares one
// linear memory), so each Engine serializes its operations under a mutex. The
// package-level Project/Rewrite/Merge/Validate use a lazily-initialized shared
// Engine; construct dedicated Engines with New for parallelism.
package lagom

import (
	"context"
	"encoding/json"
	"fmt"
	"sync"

	"github.com/hylla-io/lagom/go/internal/wasmbin"
	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
)

// Result-buffer status tags, matching lagom-wasm's memory ABI: the first byte
// of every result buffer is the tag; the rest is the payload.
const (
	statusOK  = 0 // remaining bytes are the result JSON
	statusErr = 1 // remaining bytes are a UTF-8 error message
)

// Engine is an instantiated lagom-core wasm module plus the wazero runtime
// hosting it. It owns one linear memory, so its operations are serialized; an
// Engine is safe for concurrent use by multiple goroutines (calls block on an
// internal mutex). Construct one with New and release it with Close.
type Engine struct {
	mu      sync.Mutex
	runtime wazero.Runtime
	mod     api.Module
	alloc   api.Function
	dealloc api.Function
	project api.Function
	rewrite api.Function
	merge   api.Function
	valid   api.Function
	mint    api.Function
	refire  api.Function
}

// New instantiates the embedded lagom-core wasm module in a fresh wazero
// runtime. The runtime is pure Go (no cgo); nothing is read from disk and no
// upstream process is spawned. Call Close when finished to free the runtime.
func New(ctx context.Context) (*Engine, error) {
	runtime := wazero.NewRuntime(ctx)
	mod, err := runtime.Instantiate(ctx, wasmbin.WASM)
	if err != nil {
		_ = runtime.Close(ctx)
		return nil, fmt.Errorf("lagom: instantiate wasm: %w", err)
	}

	e := &Engine{
		runtime: runtime,
		mod:     mod,
		alloc:   mod.ExportedFunction("alloc"),
		dealloc: mod.ExportedFunction("dealloc"),
		project: mod.ExportedFunction("project"),
		rewrite: mod.ExportedFunction("rewrite"),
		merge:   mod.ExportedFunction("merge"),
		valid:   mod.ExportedFunction("validate"),
		mint:    mod.ExportedFunction("mint"),
		refire:  mod.ExportedFunction("refire"),
	}
	for name, fn := range map[string]api.Function{
		"alloc": e.alloc, "dealloc": e.dealloc, "project": e.project,
		"rewrite": e.rewrite, "merge": e.merge, "validate": e.valid,
		"mint": e.mint, "refire": e.refire,
	} {
		if fn == nil {
			_ = runtime.Close(ctx)
			return nil, fmt.Errorf("lagom: wasm module is missing export %q", name)
		}
	}
	return e, nil
}

// Close releases the wazero runtime and the wasm instance. After Close the
// Engine must not be used.
func (e *Engine) Close(ctx context.Context) error {
	return e.runtime.Close(ctx)
}

// call drives one operation through the memory ABI: alloc an input buffer, write
// the JSON, invoke op (which returns the packed (ptr<<32)|len result), read the
// tagged result buffer, free both buffers, and return the payload — or an error
// carrying the engine's own message on a status-err result.
func (e *Engine) call(ctx context.Context, op api.Function, what string, input []byte) ([]byte, error) {
	e.mu.Lock()
	defer e.mu.Unlock()

	inLen := uint64(len(input))
	allocRes, err := e.alloc.Call(ctx, inLen)
	if err != nil {
		return nil, fmt.Errorf("lagom: %s: alloc input: %w", what, err)
	}
	inPtr := allocRes[0]
	// Free the input buffer once we're done regardless of outcome.
	defer func() { _, _ = e.dealloc.Call(ctx, inPtr, inLen) }()

	if !e.mod.Memory().Write(uint32(inPtr), input) {
		return nil, fmt.Errorf("lagom: %s: write input out of range", what)
	}

	res, err := op.Call(ctx, inPtr, inLen)
	if err != nil {
		return nil, fmt.Errorf("lagom: %s: call wasm: %w", what, err)
	}
	packed := res[0]
	outPtr := uint32(packed >> 32)
	outLen := uint32(packed & 0xffffffff)
	// Free the result buffer once we've copied the bytes out.
	defer func() { _, _ = e.dealloc.Call(ctx, uint64(outPtr), uint64(outLen)) }()

	raw, ok := e.mod.Memory().Read(outPtr, outLen)
	if !ok || outLen == 0 {
		return nil, fmt.Errorf("lagom: %s: read result out of range", what)
	}
	// Copy out of linear memory before the deferred dealloc reclaims it.
	out := make([]byte, outLen-1)
	tag := raw[0]
	copy(out, raw[1:])

	switch tag {
	case statusOK:
		return out, nil
	case statusErr:
		return nil, fmt.Errorf("lagom: %s: %s", what, string(out))
	default:
		return nil, fmt.Errorf("lagom: %s: unknown status tag %d", what, tag)
	}
}

// Project projects an upstream tool surface through a policy (SPEC.md §3, §4).
//
// upstreamJSON is the upstream tools/list array of tool defs as JSON;
// policyJSON is a lagom-core Policy as JSON. The returned JSON is the projected
// tool-def array — tools dropped, pinned args pruned from schemas, constraints
// applied. Malformed input or a serialization failure returns an error.
func (e *Engine) Project(ctx context.Context, upstreamJSON, policyJSON []byte) ([]byte, error) {
	in, err := envelope(map[string]json.RawMessage{
		"upstream": rawOrNull(upstreamJSON),
		"policy":   rawOrNull(policyJSON),
	})
	if err != nil {
		return nil, fmt.Errorf("lagom: project: %w", err)
	}
	return e.call(ctx, e.project, "project", in)
}

// Rewrite rewrites a projected call back into the upstream call, injecting
// pinned/default args, or returns an error on reject (SPEC.md §4, §9).
//
// callJSON is a lagom-core ToolCall as JSON; policyJSON is the policy. The
// returned JSON is the upstream ToolCall; a rejected call (unknown tool,
// violated constraint) returns an error annotated with the violated bound.
func (e *Engine) Rewrite(ctx context.Context, callJSON, policyJSON []byte) ([]byte, error) {
	in, err := envelope(map[string]json.RawMessage{
		"call":   rawOrNull(callJSON),
		"policy": rawOrNull(policyJSON),
	})
	if err != nil {
		return nil, fmt.Errorf("lagom: rewrite: %w", err)
	}
	return e.call(ctx, e.rewrite, "rewrite", in)
}

// Merge performs the narrow-only merge of an end-user overlay onto an
// integrator base (SPEC.md §5.2). Both are policies as JSON; the returned JSON
// is the composed policy. Any attempt to widen the sealed bounds (re-add a
// dropped tool, loosen a constraint, unpin) returns an error — this merge is
// the sandbox enforcement.
func (e *Engine) Merge(ctx context.Context, baseJSON, overlayJSON []byte) ([]byte, error) {
	in, err := envelope(map[string]json.RawMessage{
		"base":    rawOrNull(baseJSON),
		"overlay": rawOrNull(overlayJSON),
	})
	if err != nil {
		return nil, fmt.Errorf("lagom: merge: %w", err)
	}
	return e.call(ctx, e.merge, "merge", in)
}

// Validate validates a policy against the live upstream tool surface
// (SPEC.md §5.3). policyJSON is the policy; upstreamJSON is the upstream tool
// defs as JSON. It returns nil on success; on drift (a policy reference no
// longer matches the upstream) it returns an error carrying every per-reference
// drift message — serving must be refused.
func (e *Engine) Validate(ctx context.Context, policyJSON, upstreamJSON []byte) error {
	in, err := envelope(map[string]json.RawMessage{
		"policy":   rawOrNull(policyJSON),
		"upstream": rawOrNull(upstreamJSON),
	})
	if err != nil {
		return fmt.Errorf("lagom: validate: %w", err)
	}
	_, err = e.call(ctx, e.valid, "validate", in)
	return err
}

// --- Ephemeral mint / refire (SPEC.md §8.2) ---

// Mint mints an ephemeral per-agent projection in code and returns the recorded
// MintRecord as JSON (SPEC.md §8.1, §8.2).
//
// runID names the run; baseJSON is the integrator's sealed-ceiling Policy;
// dynamicJSON is an optional per-agent narrowing overlay (pass nil/empty for
// none — e.g. a `path` constraint scoping a subagent to the exact files it may
// touch); upstreamJSON is the UpstreamCommand (`{"command","args","env"}`) that
// launches the upstream this projection wraps. The returned JSON is a MintRecord
// (resolved policy + provenance) the app persists wherever it likes and later
// hands to Refire to reproduce the same server. The overlay may only NARROW the
// base; any widening (re-add a dropped tool, loosen a constraint, unpin) returns
// an error — this merge is the sandbox enforcement (SPEC.md §5.2). Pure: no LLM,
// no clock, no disk, so identical inputs mint a byte-identical record.
func (e *Engine) Mint(ctx context.Context, runID string, baseJSON, dynamicJSON, upstreamJSON []byte) ([]byte, error) {
	runIDJSON, err := json.Marshal(runID)
	if err != nil {
		return nil, fmt.Errorf("lagom: mint: %w", err)
	}
	in, err := envelope(map[string]json.RawMessage{
		"run_id":   runIDJSON,
		"base":     rawOrNull(baseJSON),
		"dynamic":  rawOrNull(dynamicJSON),
		"upstream": rawOrNull(upstreamJSON),
	})
	if err != nil {
		return nil, fmt.Errorf("lagom: mint: %w", err)
	}
	return e.call(ctx, e.mint, "mint", in)
}

// Refire re-mints the recorded resolved policy from a persisted MintRecord
// (SPEC.md §8.2).
//
// recordJSON is a MintRecord (as returned by Mint). The returned JSON is the
// ResolvedPolicy (`{"policy","upstream"}`) ready to serve — the exact projection
// the original run had, reproduced without re-resolution even if the source
// config has since changed. A malformed record returns an error.
func (e *Engine) Refire(ctx context.Context, recordJSON []byte) ([]byte, error) {
	in, err := envelope(map[string]json.RawMessage{
		"record": rawOrNull(recordJSON),
	})
	if err != nil {
		return nil, fmt.Errorf("lagom: refire: %w", err)
	}
	return e.call(ctx, e.refire, "refire", in)
}

// --- Guard: the brandable one-call helper (SPEC.md §2, §7.2) ---

// Guard is the ergonomic one-call helper an app wires a slim, branded MCP
// through. Construct one with NewGuard from the app's full tool defs plus a
// policy; it projects the slim downstream surface once (SlimDefs) and gates
// every incoming tools/call (Gate) — so the app registers slim tools and gates
// calls without ever touching Project/Rewrite plumbing or threading the policy
// at each call site.
//
// lagom stays invisible: all branding (downstream tool names via rename, slim
// docs via description override, which tools exist via drop/default_presence)
// lives in the policy the app supplies. The app reads the policy from wherever
// it likes; Guard reads nothing itself. This mirrors lagom_core::Guard and the
// Python/Node Guard exactly (NO DRIFT).
//
// A Guard holds a reference to an Engine and is safe for concurrent use to the
// same extent the Engine is (each Gate serializes on the Engine's mutex).
type Guard struct {
	eng      *Engine
	policy   []byte
	slimDefs []byte
}

// NewGuard builds a guard over the shared Engine from the upstream tool defs and
// the policy that narrows them. upstreamJSON is the app's full tools/list array
// as JSON; policyJSON is a lagom-core Policy as JSON (from the app's own config,
// a builder, or a literal). The slim surface is projected once here, so a
// malformed input or a projection failure surfaces immediately as an error
// rather than at first use. Use NewGuardWith to bind a dedicated Engine.
func NewGuard(ctx context.Context, upstreamJSON, policyJSON []byte) (*Guard, error) {
	e, err := shared(ctx)
	if err != nil {
		return nil, err
	}
	return NewGuardWith(ctx, e, upstreamJSON, policyJSON)
}

// NewGuardWith builds a guard bound to a specific Engine (for parallelism: each
// Engine owns its own linear memory). See NewGuard.
func NewGuardWith(ctx context.Context, e *Engine, upstreamJSON, policyJSON []byte) (*Guard, error) {
	policy := append([]byte(nil), rawOrNull(policyJSON)...)
	slim, err := e.Project(ctx, upstreamJSON, policy)
	if err != nil {
		return nil, fmt.Errorf("lagom: guard: %w", err)
	}
	return &Guard{eng: e, policy: policy, slimDefs: slim}, nil
}

// SlimDefs returns the projected, branded downstream tool defs as JSON — the
// array to advertise as tools/list. Tools dropped, pinned args pruned, names
// renamed and descriptions overridden per the policy. The returned slice is the
// Guard's own buffer; treat it as read-only.
func (g *Guard) SlimDefs() []byte {
	return g.slimDefs
}

// Gate gates one incoming downstream tools/call. callJSON uses the downstream
// (post-rename) tool name and the args the agent supplied through the slim
// schema. It returns the upstream call to forward as JSON — pinned values
// injected, defaults filled, the name mapped back to upstream — or an error
// (unknown/dropped tool, violated constraint) carrying the reason to annotate
// back to the agent (SPEC.md §9.1 — never swallow).
func (g *Guard) Gate(ctx context.Context, callJSON []byte) ([]byte, error) {
	return g.eng.Rewrite(ctx, callJSON, g.policy)
}

// Policy returns the frozen policy JSON this guard enforces (e.g. to Validate it
// against a freshly probed upstream). The returned slice is the Guard's own
// buffer; treat it as read-only.
func (g *Guard) Policy() []byte {
	return g.policy
}

// envelope marshals a named-argument object into the single JSON buffer each
// wasm operation expects.
func envelope(fields map[string]json.RawMessage) ([]byte, error) {
	out, err := json.Marshal(fields)
	if err != nil {
		return nil, fmt.Errorf("marshal args: %w", err)
	}
	return out, nil
}

// rawOrNull treats nil/empty input as JSON null so the wasm side gets a
// well-formed (if rejected) document rather than an empty buffer.
func rawOrNull(b []byte) json.RawMessage {
	if len(b) == 0 {
		return json.RawMessage("null")
	}
	return json.RawMessage(b)
}

// --- package-level convenience over a lazily-initialized shared Engine ---

var (
	defaultOnce sync.Once
	defaultEng  *Engine
	defaultErr  error
)

// shared returns the process-wide Engine, instantiating it on first use.
func shared(ctx context.Context) (*Engine, error) {
	defaultOnce.Do(func() {
		defaultEng, defaultErr = New(ctx)
	})
	return defaultEng, defaultErr
}

// Project projects an upstream surface through a policy using the shared Engine.
// See Engine.Project.
func Project(ctx context.Context, upstreamJSON, policyJSON []byte) ([]byte, error) {
	e, err := shared(ctx)
	if err != nil {
		return nil, err
	}
	return e.Project(ctx, upstreamJSON, policyJSON)
}

// Rewrite rewrites a projected call using the shared Engine. See Engine.Rewrite.
func Rewrite(ctx context.Context, callJSON, policyJSON []byte) ([]byte, error) {
	e, err := shared(ctx)
	if err != nil {
		return nil, err
	}
	return e.Rewrite(ctx, callJSON, policyJSON)
}

// Merge merges an overlay onto a base using the shared Engine. See Engine.Merge.
func Merge(ctx context.Context, baseJSON, overlayJSON []byte) ([]byte, error) {
	e, err := shared(ctx)
	if err != nil {
		return nil, err
	}
	return e.Merge(ctx, baseJSON, overlayJSON)
}

// Validate validates a policy against an upstream using the shared Engine. See
// Engine.Validate.
func Validate(ctx context.Context, policyJSON, upstreamJSON []byte) error {
	e, err := shared(ctx)
	if err != nil {
		return err
	}
	return e.Validate(ctx, policyJSON, upstreamJSON)
}

// Mint mints an ephemeral projection using the shared Engine. See Engine.Mint.
func Mint(ctx context.Context, runID string, baseJSON, dynamicJSON, upstreamJSON []byte) ([]byte, error) {
	e, err := shared(ctx)
	if err != nil {
		return nil, err
	}
	return e.Mint(ctx, runID, baseJSON, dynamicJSON, upstreamJSON)
}

// Refire re-mints a recorded resolved policy using the shared Engine. See
// Engine.Refire.
func Refire(ctx context.Context, recordJSON []byte) ([]byte, error) {
	e, err := shared(ctx)
	if err != nil {
		return nil, err
	}
	return e.Refire(ctx, recordJSON)
}
