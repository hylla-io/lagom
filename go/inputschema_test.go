package lagom

import (
	"context"
	"strings"
	"testing"
)

// TestProjectAcceptsRawMCPInputSchema proves the core inputSchema alias end to
// end through the Go binding: a consumer can hand the RAW upstream tools/list
// surface — MCP camelCase `inputSchema`, plus null and absent schemas — straight
// to lagom with NO field-mapping shim. This is the #1 consumer gotcha
// (SAND_LAGOM_FINDINGS.md §4); the core now absorbs it so every binding does.
func TestProjectAcceptsRawMCPInputSchema(t *testing.T) {
	ctx := context.Background()
	// Exactly what an mcp-go client yields from a real upstream tools/list:
	// camelCase inputSchema, a zero-arg tool with null schema, and one omitting it.
	upstream := []byte(`[
	  {"name":"echo","description":"e","inputSchema":{"type":"object","properties":{"message":{"type":"string"}}}},
	  {"name":"ping","inputSchema":null},
	  {"name":"noop"}
	]`)
	pol := []byte(`{"default_presence":"keep","tools":{}}`)

	out, err := Project(ctx, upstream, pol)
	if err != nil {
		t.Fatalf("Project must accept raw MCP camelCase defs with no shim: %v", err)
	}
	s := string(out)
	for _, name := range []string{`"echo"`, `"ping"`, `"noop"`} {
		if !strings.Contains(s, name) {
			t.Errorf("tool %s missing from projection: %s", name, s)
		}
	}
	// Output stays canonical snake_case — downstream contract unchanged.
	if !strings.Contains(s, `"input_schema"`) || strings.Contains(s, `"inputSchema"`) {
		t.Errorf("projection must emit snake_case input_schema only: %s", s)
	}
}
