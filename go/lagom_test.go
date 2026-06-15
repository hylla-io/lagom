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
