package lagom_test

import (
	"context"
	"encoding/json"
	"testing"

	lagom "github.com/hylla-io/lagom/go"
)

// upstreamDefs is a two-tool upstream surface: a `search` tool with an
// `artifact` and `query` arg, and a `write_file` tool.
const upstreamDefs = `[
  {
    "name": "search",
    "description": "Search.",
    "input_schema": {
      "type": "object",
      "properties": {
        "artifact": {"type": "string"},
        "query": {"type": "string"}
      },
      "required": ["artifact", "query"]
    }
  },
  {
    "name": "write_file",
    "description": "Write.",
    "input_schema": {"type": "object", "properties": {}}
  }
]`

// sealedKeepSearchPinArtifact: drop everything by default, keep `search`, pin
// its `artifact` arg to "hylla" (removing it from the projected schema).
const sealedKeepSearchPinArtifact = `{
  "default_presence": "drop",
  "tools": {
    "search": {"presence": "keep", "args": {"artifact": {"pin": "hylla"}}}
  }
}`

type toolDef struct {
	Name        string          `json:"name"`
	Description *string         `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

type toolCall struct {
	Name      string                     `json:"name"`
	Arguments map[string]json.RawMessage `json:"arguments"`
}

// TestProjectDropsToolAndHidesPinnedArg is the core in-process proof required by
// the task: through the embedded wasm engine (no cgo, no subprocess),
// project() drops `write_file` and prunes the pinned `artifact` property from
// `search`'s schema while keeping `query`.
func TestProjectDropsToolAndHidesPinnedArg(t *testing.T) {
	ctx := context.Background()

	out, err := lagom.Project(ctx, []byte(upstreamDefs), []byte(sealedKeepSearchPinArtifact))
	if err != nil {
		t.Fatalf("Project: %v", err)
	}

	var defs []toolDef
	if err := json.Unmarshal(out, &defs); err != nil {
		t.Fatalf("unmarshal projected defs: %v (raw: %s)", err, out)
	}

	if len(defs) != 1 {
		t.Fatalf("expected 1 projected tool (write_file dropped), got %d: %s", len(defs), out)
	}
	if defs[0].Name != "search" {
		t.Fatalf("expected kept tool to be search, got %q", defs[0].Name)
	}

	var schema struct {
		Properties map[string]json.RawMessage `json:"properties"`
	}
	if err := json.Unmarshal(defs[0].InputSchema, &schema); err != nil {
		t.Fatalf("unmarshal input_schema: %v", err)
	}
	if _, present := schema.Properties["artifact"]; present {
		t.Errorf("pinned arg `artifact` must be hidden from the projected schema, but it is present: %s", defs[0].InputSchema)
	}
	if _, present := schema.Properties["query"]; !present {
		t.Errorf("non-pinned arg `query` must remain visible, but it is absent: %s", defs[0].InputSchema)
	}
}

// TestRewriteInjectsPinAndRejectsConstraint proves rewrite() re-injects the
// pinned value the agent never saw, and that a constraint violation surfaces as
// an annotated Go error (never swallowed).
func TestRewriteInjectsPinAndRejectsConstraint(t *testing.T) {
	ctx := context.Background()
	policy := `{
      "tools": {
        "search": {
          "args": {
            "artifact": {"pin": "hylla"},
            "query": {"constrain": {"enum": ["a", "b"]}}
          }
        }
      }
    }`

	out, err := lagom.Rewrite(ctx, []byte(`{"name":"search","arguments":{"query":"a"}}`), []byte(policy))
	if err != nil {
		t.Fatalf("Rewrite (valid): %v", err)
	}
	var call toolCall
	if err := json.Unmarshal(out, &call); err != nil {
		t.Fatalf("unmarshal rewritten call: %v", err)
	}
	if string(call.Arguments["artifact"]) != `"hylla"` {
		t.Errorf("pin must be injected: artifact = %s", call.Arguments["artifact"])
	}

	_, err = lagom.Rewrite(ctx, []byte(`{"name":"search","arguments":{"query":"z"}}`), []byte(policy))
	if err == nil {
		t.Fatal("out-of-enum query must reject with an error, got nil")
	}
}

// TestMergeRejectsWidening proves the narrow-only merge: re-adding a tool the
// base dropped is widening and must fail (the sandbox enforcement).
func TestMergeRejectsWidening(t *testing.T) {
	ctx := context.Background()
	base := `{"default_presence":"drop"}`
	overlay := `{"tools":{"search":{"presence":"keep"}}}`

	if _, err := lagom.Merge(ctx, []byte(base), []byte(overlay)); err == nil {
		t.Fatal("re-adding a dropped tool is widening and must error, got nil")
	}
}

// TestValidateFlagsDrift proves validate() passes a clean policy and fails loud
// when the policy references a tool the upstream does not expose.
func TestValidateFlagsDrift(t *testing.T) {
	ctx := context.Background()

	clean := `{"tools":{"search":{"args":{"artifact":{"pin":"x"}}}}}`
	if err := lagom.Validate(ctx, []byte(clean), []byte(upstreamDefs)); err != nil {
		t.Fatalf("clean policy must validate, got: %v", err)
	}

	drifted := `{"tools":{"ghost":{"args":{"x":{"pin":1}}}}}`
	if err := lagom.Validate(ctx, []byte(drifted), []byte(upstreamDefs)); err == nil {
		t.Fatal("policy referencing a vanished tool must drift-error, got nil")
	}
}

// TestMalformedInputIsError proves malformed JSON returns an error rather than
// panicking or silently succeeding.
func TestMalformedInputIsError(t *testing.T) {
	ctx := context.Background()
	if _, err := lagom.Project(ctx, []byte("not json"), []byte("{}")); err == nil {
		t.Fatal("malformed upstream JSON must error, got nil")
	}
}

// brandedPolicy is a sealed, branded policy: keep `search` exposed under the
// app's own name `find` with the app's own description, pin `artifact` (hidden),
// drop everything else. This is exactly what a branded app (sand) hands lagom.
const brandedPolicy = `{
  "default_presence": "drop",
  "tools": {
    "search": {
      "presence": "keep",
      "rename": "find",
      "description": {"override": "App find."},
      "args": {"artifact": {"pin": "hylla"}}
    }
  }
}`

// TestGuardSlimDefsAreBrandedAndNarrowed proves the one-call helper: NewGuard
// projects a slim surface carrying the app's OWN tool name + description (lagom
// invisible), drops the rest, and prunes the pinned arg — all without the app
// calling Project itself.
func TestGuardSlimDefsAreBrandedAndNarrowed(t *testing.T) {
	ctx := context.Background()

	g, err := lagom.NewGuard(ctx, []byte(upstreamDefs), []byte(brandedPolicy))
	if err != nil {
		t.Fatalf("NewGuard: %v", err)
	}

	var defs []toolDef
	if err := json.Unmarshal(g.SlimDefs(), &defs); err != nil {
		t.Fatalf("unmarshal slim defs: %v (raw: %s)", err, g.SlimDefs())
	}
	if len(defs) != 1 {
		t.Fatalf("expected 1 slim tool (write_file dropped), got %d: %s", len(defs), g.SlimDefs())
	}
	if defs[0].Name != "find" {
		t.Errorf("slim def must carry the app's branded name `find`, got %q", defs[0].Name)
	}
	if defs[0].Description == nil || *defs[0].Description != "App find." {
		t.Errorf("slim def must carry the app's branded description, got %v", defs[0].Description)
	}
	var schema struct {
		Properties map[string]json.RawMessage `json:"properties"`
	}
	if err := json.Unmarshal(defs[0].InputSchema, &schema); err != nil {
		t.Fatalf("unmarshal input_schema: %v", err)
	}
	if _, present := schema.Properties["artifact"]; present {
		t.Errorf("pinned arg `artifact` must be hidden from the slim schema: %s", defs[0].InputSchema)
	}
	if _, present := schema.Properties["query"]; !present {
		t.Errorf("non-pinned arg `query` must remain visible: %s", defs[0].InputSchema)
	}
}

// TestGuardGateInjectsPinAndRejectsDropped proves Gate maps the branded name
// back to upstream, injects the pin the agent never saw, and rejects a dropped
// tool — the gating half of the helper, no Rewrite call by the app.
func TestGuardGateInjectsPinAndRejectsDropped(t *testing.T) {
	ctx := context.Background()

	g, err := lagom.NewGuard(ctx, []byte(upstreamDefs), []byte(brandedPolicy))
	if err != nil {
		t.Fatalf("NewGuard: %v", err)
	}

	// Agent calls the branded name with only what it can see.
	out, err := g.Gate(ctx, []byte(`{"name":"find","arguments":{"query":"x"}}`))
	if err != nil {
		t.Fatalf("Gate (valid branded call): %v", err)
	}
	var call toolCall
	if err := json.Unmarshal(out, &call); err != nil {
		t.Fatalf("unmarshal gated call: %v", err)
	}
	if call.Name != "search" {
		t.Errorf("Gate must map branded `find` back to upstream `search`, got %q", call.Name)
	}
	if string(call.Arguments["artifact"]) != `"hylla"` {
		t.Errorf("Gate must inject the pin: artifact = %s", call.Arguments["artifact"])
	}

	// A dropped tool (and the un-renamed original name) must reject.
	if _, err := g.Gate(ctx, []byte(`{"name":"write_file","arguments":{}}`)); err == nil {
		t.Error("Gate must reject a dropped tool, got nil")
	}
	if _, err := g.Gate(ctx, []byte(`{"name":"search","arguments":{}}`)); err == nil {
		t.Error("Gate must reject the hidden original name of a renamed tool, got nil")
	}
}

// TestGuardMalformedPolicyIsError proves a bad policy fails loud at construction,
// not silently at first use.
func TestGuardMalformedPolicyIsError(t *testing.T) {
	ctx := context.Background()
	if _, err := lagom.NewGuard(ctx, []byte(upstreamDefs), []byte("not json")); err == nil {
		t.Fatal("malformed policy must error at NewGuard, got nil")
	}
}

// mintRecord / resolvedPolicy mirror the JSON shapes Mint/Refire exchange, so
// the test can assert on the resolved projection an app would persist and refire.
type resolvedPolicy struct {
	Policy   json.RawMessage `json:"policy"`
	Upstream json.RawMessage `json:"upstream"`
}

type mintRecord struct {
	RunID    string          `json:"run_id"`
	Sources  json.RawMessage `json:"sources"`
	Resolved resolvedPolicy  `json:"resolved"`
}

// TestMintThenRefireRoundTrips proves the ephemeral mint/refire loop the first
// consumer (sand) needs (SPEC.md §8.2): Mint narrows a base policy by a per-agent
// dynamic overlay into a persistable MintRecord, and Refire reproduces the
// recorded resolved policy byte-for-byte — the exact projection, re-mintable
// even after the source config drifts.
func TestMintThenRefireRoundTrips(t *testing.T) {
	ctx := context.Background()

	// base: keep `search`; dynamic per-agent overlay: drop it (a legal narrowing).
	base := `{"default_presence":"keep","tools":{"search":{"presence":"keep"}}}`
	dynamic := `{"default_presence":"keep","tools":{"search":{"presence":"drop"}}}`
	upstream := `{"command":"srv","args":["-y"],"env":[]}`

	recordJSON, err := lagom.Mint(ctx, "agent-7", []byte(base), []byte(dynamic), []byte(upstream))
	if err != nil {
		t.Fatalf("Mint: %v", err)
	}
	var record mintRecord
	if err := json.Unmarshal(recordJSON, &record); err != nil {
		t.Fatalf("unmarshal mint record: %v (raw: %s)", err, recordJSON)
	}
	if record.RunID != "agent-7" {
		t.Errorf("record must carry the run id, got %q", record.RunID)
	}
	// The dynamic overlay must have narrowed the resolved policy (search dropped).
	var resolvedTools struct {
		Tools map[string]struct {
			Presence *string `json:"presence"`
		} `json:"tools"`
	}
	if err := json.Unmarshal(record.Resolved.Policy, &resolvedTools); err != nil {
		t.Fatalf("unmarshal resolved policy: %v", err)
	}
	if p := resolvedTools.Tools["search"].Presence; p == nil || *p != "drop" {
		t.Errorf("dynamic overlay must drop search in the resolved policy, got %v", p)
	}

	// Refire reproduces the recorded resolved policy byte-for-byte.
	refired, err := lagom.Refire(ctx, recordJSON)
	if err != nil {
		t.Fatalf("Refire: %v", err)
	}
	want, err := json.Marshal(record.Resolved)
	if err != nil {
		t.Fatalf("marshal recorded resolved: %v", err)
	}
	if string(refired) != string(want) {
		t.Errorf("Refire must reproduce the recorded resolved policy byte-for-byte:\n got: %s\nwant: %s", refired, want)
	}
}

// TestMintRejectsWideningOverlay proves the in-code mint path enforces the
// narrow-only sandbox: a dynamic overlay re-keeping a tool the base dropped is
// widening and must fail (SPEC.md §5.2).
func TestMintRejectsWideningOverlay(t *testing.T) {
	ctx := context.Background()
	base := `{"default_presence":"keep","tools":{"search":{"presence":"drop"}}}`
	widening := `{"default_presence":"keep","tools":{"search":{"presence":"keep"}}}`
	upstream := `{"command":"srv"}`

	if _, err := lagom.Mint(ctx, "r", []byte(base), []byte(widening), []byte(upstream)); err == nil {
		t.Fatal("a widening dynamic overlay must error, got nil")
	}
}

// TestRefireMalformedRecordIsError proves a bad record fails loud, never
// silently producing an empty resolved policy.
func TestRefireMalformedRecordIsError(t *testing.T) {
	ctx := context.Background()
	if _, err := lagom.Refire(ctx, []byte("not json")); err == nil {
		t.Fatal("malformed mint record must error, got nil")
	}
}
