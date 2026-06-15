package examples

import (
	"context"
	"encoding/json"
	"fmt"

	lagom "github.com/hylla-io/lagom/go"
)

// --- The faked upstreams (stand-ins for real wrapped MCP servers) ---

// RepoUpstream is a small "repo" upstream with three tools: read_file,
// write_file, and delete_path. A branded app projects narrow views of it per
// agent (a reader sees only read_file; a writer also gets write_file).
func RepoUpstream() Upstream {
	defs := []byte(`[
      {
        "name": "read_file",
        "description": "Read a file from the repository at an absolute path.",
        "input_schema": {
          "type": "object",
          "properties": {
            "repo": {"type": "string"},
            "path": {"type": "string"}
          },
          "required": ["repo", "path"]
        }
      },
      {
        "name": "write_file",
        "description": "Write contents to a file in the repository.",
        "input_schema": {
          "type": "object",
          "properties": {
            "repo": {"type": "string"},
            "path": {"type": "string"},
            "contents": {"type": "string"}
          },
          "required": ["repo", "path", "contents"]
        }
      },
      {
        "name": "delete_path",
        "description": "Delete a path from the repository.",
        "input_schema": {
          "type": "object",
          "properties": {"repo": {"type": "string"}, "path": {"type": "string"}},
          "required": ["repo", "path"]
        }
      }
    ]`)
	run := func(_ context.Context, name string, args map[string]json.RawMessage) (string, error) {
		// Echo the resolved upstream call so a test (or agent) can see exactly
		// which upstream tool ran and which args lagom injected.
		out, err := json.Marshal(map[string]any{"tool": name, "args": args})
		if err != nil {
			return "", err
		}
		return string(out), nil
	}
	return Upstream{Defs: defs, Run: run}
}

// SearchUpstream is a second, independent upstream — a "search" service — used to
// prove the multi-upstream-by-composition story: one agent, two wrapped servers.
func SearchUpstream() Upstream {
	defs := []byte(`[
      {
        "name": "search",
        "description": "Search an index for a query string.",
        "input_schema": {
          "type": "object",
          "properties": {
            "index": {"type": "string"},
            "query": {"type": "string"}
          },
          "required": ["index", "query"]
        }
      }
    ]`)
	run := func(_ context.Context, name string, args map[string]json.RawMessage) (string, error) {
		out, err := json.Marshal(map[string]any{"tool": name, "args": args})
		if err != nil {
			return "", err
		}
		return string(out), nil
	}
	return Upstream{Defs: defs, Run: run}
}

// --- The branded, sealed base policy over RepoUpstream ---
//
// This is the integrator's sealed ceiling: drop everything by default, expose
// the repo tools under the app's OWN names ("fetch"/"save"/"remove"), pin `repo`
// to a fixed value the agent never sees, and constrain `path` to the agent's
// sandbox prefix. lagom is invisible — the agent only ever sees this vocabulary.

// BasePolicy is the integrator's branded sealed-ceiling Policy over the repo
// upstream. Per-agent profiles narrow it further; none may widen it (SPEC.md
// §5.2 sealed bounds).
func BasePolicy() []byte {
	return []byte(`{
      "default_presence": "drop",
      "tools": {
        "read_file": {
          "presence": "keep",
          "rename": "fetch",
          "description": {"override": "Read repo file. path under sandbox."},
          "args": {
            "repo": {"pin": "hylla/lagom"},
            "path": {"constrain": {"pattern": "^/sandbox/"}}
          }
        },
        "write_file": {
          "presence": "keep",
          "rename": "save",
          "description": {"override": "Write repo file. path under sandbox."},
          "args": {
            "repo": {"pin": "hylla/lagom"},
            "path": {"constrain": {"pattern": "^/sandbox/"}}
          }
        }
      }
    }`)
}

// --- Two ephemeral per-agent profiles minted from the SAME base + binary ---

// readerOverlay narrows the base to a read-only agent: drop `save` (write_file),
// keeping only `fetch`. A legal narrowing of the sealed base.
func readerOverlay() []byte {
	return []byte(`{
      "default_presence": "drop",
      "tools": {"write_file": {"presence": "drop"}}
    }`)
}

// writerOverlay narrows the base for a writer agent that additionally pins its
// edits to one path — tightening `path` from the base prefix-pattern to a single
// pinned value (a narrowing: a pin is the tightest possible constraint).
func writerOverlay() []byte {
	return []byte(`{
      "default_presence": "drop",
      "tools": {
        "write_file": {"args": {"path": {"pin": "/sandbox/notes.md"}}}
      }
    }`)
}

// UpstreamCommand is the launch descriptor lagom records in a MintRecord so a
// refire can re-spawn the exact upstream. The examples fake the upstream, so the
// command is illustrative — it proves the record carries provenance.
func upstreamCommand() []byte {
	return []byte(`{"command":"repo-mcp","args":["--stdio"],"env":[]}`)
}

// MintProfile mints an ephemeral per-agent projection: it narrows BasePolicy by
// the named overlay into a MintRecord (resolved policy + provenance) the app can
// persist and later refire. runID names the run/agent. Returns the MintRecord
// JSON. A widening overlay fails loud here (SPEC.md §5.2, §8.2).
//
// Known profiles: "reader" (read-only) and "writer" (write pinned to one path).
func MintProfile(ctx context.Context, runID, profile string) ([]byte, error) {
	var overlay []byte
	switch profile {
	case "reader":
		overlay = readerOverlay()
	case "writer":
		overlay = writerOverlay()
	default:
		return nil, fmt.Errorf("examples: unknown profile %q", profile)
	}
	return lagom.Mint(ctx, runID, BasePolicy(), overlay, upstreamCommand())
}

// ResolvedPolicyOf extracts the resolved Policy JSON from a MintRecord (or a
// refired ResolvedPolicy), so a branded server can be built from an ephemeral
// profile's resolved surface.
func ResolvedPolicyOf(recordOrResolved []byte) ([]byte, error) {
	// A MintRecord is {"run_id","sources","resolved":{"policy","upstream"}}; a
	// refired ResolvedPolicy is {"policy","upstream"}. Handle both.
	var asRecord struct {
		Resolved *struct {
			Policy json.RawMessage `json:"policy"`
		} `json:"resolved"`
	}
	if err := json.Unmarshal(recordOrResolved, &asRecord); err == nil && asRecord.Resolved != nil {
		return asRecord.Resolved.Policy, nil
	}
	var asResolved struct {
		Policy json.RawMessage `json:"policy"`
	}
	if err := json.Unmarshal(recordOrResolved, &asResolved); err != nil {
		return nil, fmt.Errorf("examples: decode resolved policy: %w", err)
	}
	if asResolved.Policy == nil {
		return nil, fmt.Errorf("examples: no policy in record/resolved")
	}
	return asResolved.Policy, nil
}
