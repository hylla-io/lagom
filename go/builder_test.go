package lagom

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

// TestPolicyBuilder proves the Go builder emits a Policy the real engine accepts
// and that it behaves identically to a hand-written policy: a dropped tool is
// gone, a pinned arg is hidden from the schema and injected on the call.
func TestPolicyBuilder(t *testing.T) {
	ctx := context.Background()
	upstream := []byte(`[
	  {"name":"echo","description":"e","input_schema":{"type":"object","properties":{"message":{"type":"string"},"token":{"type":"string"}},"required":["message","token"]}},
	  {"name":"secret","input_schema":{"type":"object","properties":{}}}
	]`)

	pol, err := NewPolicyBuilder().
		Drop("secret").
		Pin("echo", "token", "LOCKED").
		Build()
	if err != nil {
		t.Fatalf("Build: %v", err)
	}

	out, err := Project(ctx, upstream, pol)
	if err != nil {
		t.Fatalf("Project: %v", err)
	}
	s := string(out)
	if strings.Contains(s, `"secret"`) {
		t.Error("dropped tool `secret` must be absent from the projection")
	}
	if !strings.Contains(s, `"echo"`) {
		t.Error("`echo` must survive")
	}
	if strings.Contains(s, `"token"`) {
		t.Error("pinned arg `token` must be hidden from the projected schema")
	}

	call, err := Rewrite(ctx, []byte(`{"name":"echo","arguments":{"message":"hi"}}`), pol)
	if err != nil {
		t.Fatalf("Rewrite: %v", err)
	}
	var c struct {
		Arguments map[string]any `json:"arguments"`
	}
	if err := json.Unmarshal(call, &c); err != nil {
		t.Fatal(err)
	}
	if c.Arguments["token"] != "LOCKED" {
		t.Errorf("pin not injected on call: %v", c.Arguments)
	}
}

// TestSealedBuilder proves a sealed builder drops everything except kept tools.
func TestSealedBuilder(t *testing.T) {
	ctx := context.Background()
	upstream := []byte(`[{"name":"a","input_schema":{"type":"object"}},{"name":"b","input_schema":{"type":"object"}}]`)
	pol, err := SealedPolicyBuilder().Keep("a").Build()
	if err != nil {
		t.Fatal(err)
	}
	out, err := Project(ctx, upstream, pol)
	if err != nil {
		t.Fatal(err)
	}
	s := string(out)
	if !strings.Contains(s, `"a"`) || strings.Contains(s, `"b"`) {
		t.Errorf("sealed+keep(a) must yield only `a`: %s", s)
	}
}
