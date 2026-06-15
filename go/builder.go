package lagom

import "encoding/json"

// PolicyBuilder is a typed, fluent builder for a lagom Policy from Go, mirroring
// the Python and TypeScript PolicyBuilder exactly (NO DRIFT). Methods narrow the
// projection; Build emits the Policy as JSON ready for Project / Rewrite / Merge
// / Validate / NewGuard / Mint. The builder reads nothing itself — branding
// (renamed names, slim descriptions, dropped tools) lives entirely in the policy
// it produces.
//
// It is pure Go (no wasm): it constructs the same Policy JSON the engine
// consumes, so its output flows through the shared core identically to every
// other binding.
type PolicyBuilder struct {
	defaultPresence string
	tools           map[string]*toolPolicy
}

type toolPolicy struct {
	Presence    *string           `json:"presence,omitempty"`
	Rename      *string           `json:"rename,omitempty"`
	Description map[string]string `json:"description,omitempty"`
	Args        map[string]any    `json:"args,omitempty"`
}

// NewPolicyBuilder returns a passthrough builder: keeps every tool unless
// narrowed (default_presence = keep).
func NewPolicyBuilder() *PolicyBuilder {
	return &PolicyBuilder{defaultPresence: "keep", tools: map[string]*toolPolicy{}}
}

// SealedPolicyBuilder returns a sealed builder: drops every tool unless
// explicitly Keep'd (default_presence = drop) — the basis of an allowlist sandbox.
func SealedPolicyBuilder() *PolicyBuilder {
	return &PolicyBuilder{defaultPresence: "drop", tools: map[string]*toolPolicy{}}
}

func (b *PolicyBuilder) tool(name string) *toolPolicy {
	tp := b.tools[name]
	if tp == nil {
		tp = &toolPolicy{}
		b.tools[name] = tp
	}
	return tp
}

func strptr(s string) *string { return &s }

// Keep explicitly keeps a tool (overrides a sealed default presence).
func (b *PolicyBuilder) Keep(tool string) *PolicyBuilder { b.tool(tool).Presence = strptr("keep"); return b }

// Drop removes a tool from the projected surface.
func (b *PolicyBuilder) Drop(tool string) *PolicyBuilder { b.tool(tool).Presence = strptr("drop"); return b }

// Rename exposes a tool under a different downstream name.
func (b *PolicyBuilder) Rename(tool, name string) *PolicyBuilder { b.tool(tool).Rename = strptr(name); return b }

// Describe replaces a tool's description with integrator-authored slim text (Tier-1 override).
func (b *PolicyBuilder) Describe(tool, text string) *PolicyBuilder {
	b.tool(tool).Description = map[string]string{"override": text}
	return b
}

func (b *PolicyBuilder) setArg(tool, arg string, ap any) *PolicyBuilder {
	tp := b.tool(tool)
	if tp.Args == nil {
		tp.Args = map[string]any{}
	}
	tp.Args[arg] = ap
	return b
}

// Pin fixes a tool's arg to value: removed from the projected schema, injected on every call.
func (b *PolicyBuilder) Pin(tool, arg string, value any) *PolicyBuilder {
	return b.setArg(tool, arg, map[string]any{"pin": value})
}

// Default supplies a tool's arg when the agent omits it (stays visible, unlike Pin).
func (b *PolicyBuilder) Default(tool, arg string, value any) *PolicyBuilder {
	return b.setArg(tool, arg, map[string]any{"default": value})
}

// ConstrainEnum restricts a tool's arg to an enum subset.
func (b *PolicyBuilder) ConstrainEnum(tool, arg string, values []any) *PolicyBuilder {
	return b.setArg(tool, arg, map[string]any{"constrain": map[string]any{"enum": values}})
}

// ConstrainRange restricts a numeric arg to an inclusive [min,max]; nil bound = open.
func (b *PolicyBuilder) ConstrainRange(tool, arg string, min, max *float64) *PolicyBuilder {
	r := map[string]any{}
	if min != nil {
		r["min"] = *min
	}
	if max != nil {
		r["max"] = *max
	}
	return b.setArg(tool, arg, map[string]any{"constrain": map[string]any{"range": r}})
}

// ConstrainPattern restricts a string arg to match a regex pattern.
func (b *PolicyBuilder) ConstrainPattern(tool, arg, pattern string) *PolicyBuilder {
	return b.setArg(tool, arg, map[string]any{"constrain": map[string]any{"pattern": pattern}})
}

// Build emits the authored Policy as JSON.
func (b *PolicyBuilder) Build() ([]byte, error) {
	return json.Marshal(map[string]any{"default_presence": b.defaultPresence, "tools": b.tools})
}
