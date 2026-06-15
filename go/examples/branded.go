// Package examples shows how an app embeds the lagom Go binding to serve a
// slim, branded MCP through mcp-go — with lagom invisible to the agent.
//
// It is built around three proofs the first consumer (sand) needs, all over the
// in-process lagom engine (no cgo, no subprocess) and driven through mcp-go's
// in-process client/server (no network, no stdio):
//
//   - BrandedServer wires a real mcp-go server from a lagom Guard: the agent
//     sees only the app's own tool names and docs (SlimDefs), every call is
//     gated by lagom (Gate injects pins, maps names back, rejects), and the name
//     "lagom" never reaches the agent — branding lives entirely in the Policy
//     the app supplies (SPEC.md §2, §7.2; CONTEXT.md "Branding").
//   - Two ephemeral profiles (reader, writer) minted from the SAME binary and
//     the SAME upstream produce two different slim surfaces — the multi-agent
//     story: a policy is data, not a compiled artifact (SPEC.md §8; README
//     "Many agents").
//   - Two upstreams each wrapped separately compose into one agent's toolset —
//     the multi-mcp story (SPEC.md §10; README "Many upstreams for one agent").
//
// lagom only narrows tools; the upstream itself is faked here by an in-package
// Upstream so the examples are hermetic (no external MCP server to install).
package examples

import (
	"context"
	"encoding/json"
	"fmt"

	lagom "github.com/hylla-io/lagom/go"
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

// Upstream is a stand-in for a real upstream MCP server: a set of full-fidelity
// tool defs plus a handler that runs each call. In production this is the wrapped
// child process; here it is in-package so the examples are hermetic. lagom never
// authors the upstream — it only re-presents a projection of it (CONTEXT.md
// "Upstream server").
type Upstream struct {
	// Defs is the full upstream tools/list as JSON (the lagom-core ToolDef array
	// shape: name, description, input_schema), exactly what an app would probe
	// from the real upstream and hand to lagom.
	Defs []byte
	// Run executes a fully-formed upstream call (post-gate: pins injected,
	// upstream name restored) and returns its text result.
	Run func(ctx context.Context, name string, args map[string]json.RawMessage) (string, error)
}

// upstreamCall is the lagom-core ToolCall shape Guard.Gate returns: the upstream
// tool name with the agent's args plus any pins lagom injected.
type upstreamCall struct {
	Name      string                     `json:"name"`
	Arguments map[string]json.RawMessage `json:"arguments"`
}

// slimDef is the subset of a projected tool def the branded server needs to
// register a tool: the branded name, the branded description, and the slim input
// schema (pinned args already pruned).
type slimDef struct {
	Name        string          `json:"name"`
	Description *string         `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

// NewBrandedServer builds a branded mcp-go server that serves the slim
// projection of upstream defined by policyJSON, with lagom invisible.
//
// serverName is the app's OWN server identity (never "lagom"). policyJSON is the
// app's branded Policy as JSON — it carries the renamed tool names, the override
// descriptions, and which tools survive; lagom reads nothing itself. The Guard
// projects the slim surface once (failing loud here on drift/malformed input),
// each slim def is registered as an mcp-go tool with its slim schema verbatim,
// and each handler gates the incoming call through lagom before forwarding the
// rewritten upstream call to up.Run.
//
// A gate rejection (dropped tool, hidden original name, violated constraint) is
// surfaced to the agent as an mcp tool-error result carrying lagom's annotation
// — never swallowed (SPEC.md §9.1).
func NewBrandedServer(ctx context.Context, serverName string, up Upstream, policyJSON []byte) (*server.MCPServer, error) {
	guard, err := lagom.NewGuard(ctx, up.Defs, policyJSON)
	if err != nil {
		return nil, fmt.Errorf("examples: build guard for %q: %w", serverName, err)
	}

	var defs []slimDef
	if err := json.Unmarshal(guard.SlimDefs(), &defs); err != nil {
		return nil, fmt.Errorf("examples: decode slim defs for %q: %w", serverName, err)
	}

	srv := server.NewMCPServer(serverName, "0.1.0")
	for _, d := range defs {
		tool := mcp.Tool{Name: d.Name, RawInputSchema: d.InputSchema}
		if d.Description != nil {
			tool.Description = *d.Description
		}
		srv.AddTool(tool, gateHandler(guard, up))
	}
	return srv, nil
}

// gateHandler returns an mcp-go tool handler that gates one downstream call
// through the Guard and forwards the rewritten upstream call to up.Run. The
// branded (downstream) name and the agent-supplied args go in; lagom maps the
// name back, injects pins, fills defaults, and enforces constraints.
func gateHandler(guard *lagom.Guard, up Upstream) server.ToolHandlerFunc {
	return func(ctx context.Context, req mcp.CallToolRequest) (*mcp.CallToolResult, error) {
		// Re-marshal the downstream call into the lagom-core ToolCall shape.
		downstream, err := json.Marshal(map[string]any{
			"name":      req.Params.Name,
			"arguments": req.GetArguments(),
		})
		if err != nil {
			return nil, fmt.Errorf("examples: marshal downstream call: %w", err)
		}

		gated, err := guard.Gate(ctx, downstream)
		if err != nil {
			// lagom rejected the call (dropped/renamed-away tool, constraint
			// violation). Relay the annotation to the agent as a tool error
			// rather than swallowing it (SPEC.md §9.1).
			return mcp.NewToolResultError(err.Error()), nil
		}

		var call upstreamCall
		if err := json.Unmarshal(gated, &call); err != nil {
			return nil, fmt.Errorf("examples: decode gated call: %w", err)
		}

		text, err := up.Run(ctx, call.Name, call.Arguments)
		if err != nil {
			return mcp.NewToolResultError(err.Error()), nil
		}
		return mcp.NewToolResultText(text), nil
	}
}
