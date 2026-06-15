package examples_test

import (
	"context"
	"encoding/json"
	"sort"
	"strings"
	"testing"

	lagom "github.com/hylla-io/lagom/go"
	"github.com/hylla-io/lagom/go/examples"
	"github.com/mark3labs/mcp-go/client"
	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

// connect starts an mcp-go in-process client against srv and runs the MCP
// initialize handshake, returning a ready client. The whole exchange is
// in-process (no stdio, no network) — it exercises the real mcp-go protocol
// path an agent's harness would use.
func connect(t *testing.T, ctx context.Context, srv *server.MCPServer) *client.Client {
	t.Helper()
	cli, err := client.NewInProcessClient(srv)
	if err != nil {
		t.Fatalf("new in-process client: %v", err)
	}
	t.Cleanup(func() { _ = cli.Close() })
	if err := cli.Start(ctx); err != nil {
		t.Fatalf("start client: %v", err)
	}
	var init mcp.InitializeRequest
	init.Params.ProtocolVersion = mcp.LATEST_PROTOCOL_VERSION
	init.Params.ClientInfo = mcp.Implementation{Name: "agent", Version: "0.1.0"}
	if _, err := cli.Initialize(ctx, init); err != nil {
		t.Fatalf("initialize: %v", err)
	}
	return cli
}

// listToolNames returns the agent-visible tool names, sorted.
func listToolNames(t *testing.T, ctx context.Context, cli *client.Client) []string {
	t.Helper()
	res, err := cli.ListTools(ctx, mcp.ListToolsRequest{})
	if err != nil {
		t.Fatalf("list tools: %v", err)
	}
	names := make([]string, 0, len(res.Tools))
	for _, tool := range res.Tools {
		names = append(names, tool.Name)
	}
	sort.Strings(names)
	return names
}

// callText calls a tool that is expected to exist and returns its text result
// plus whether it was an error result. A gate rejection of an *existing* tool
// (constraint violation) rides back as an mcp tool-error result (IsError=true,
// SPEC.md §9.1); a call to a tool the agent cannot see at all is a separate
// case handled by callRejected.
func callText(t *testing.T, ctx context.Context, cli *client.Client, name string, args map[string]any) (string, bool) {
	t.Helper()
	var req mcp.CallToolRequest
	req.Params.Name = name
	req.Params.Arguments = args
	res, err := cli.CallTool(ctx, req)
	if err != nil {
		t.Fatalf("call %q: %v", name, err)
	}
	var sb strings.Builder
	for _, c := range res.Content {
		if tc, ok := mcp.AsTextContent(c); ok {
			sb.WriteString(tc.Text)
		}
	}
	return sb.String(), res.IsError
}

// callRejected returns true if the call did not go through — either because the
// tool is not in the agent's surface at all (an mcp protocol "tool not found"
// error, e.g. a dropped tool) or because lagom gated it (an IsError result).
// Both are valid "the agent could not do this" outcomes; neither reaches the
// upstream.
func callRejected(ctx context.Context, cli *client.Client, name string, args map[string]any) bool {
	var req mcp.CallToolRequest
	req.Params.Name = name
	req.Params.Arguments = args
	res, err := cli.CallTool(ctx, req)
	if err != nil {
		return true
	}
	return res.IsError
}

// TestBrandedServerHidesLagomAndEnforces drives the branded server (built from
// BasePolicy over the repo upstream) through the mcp-go in-process client and
// proves the consumption story end to end:
//
//   - tools/list shows the app's OWN names (fetch/save), never the upstream
//     names (read_file/write_file) and never the word "lagom";
//   - the pinned `repo` arg is absent from the slim schemas (hidden);
//   - a valid call is gated: the upstream sees the pin injected and the upstream
//     name restored;
//   - a constraint violation (path outside the sealed prefix) is rejected and
//     annotated back to the agent, never silently forwarded.
func TestBrandedServerHidesLagomAndEnforces(t *testing.T) {
	ctx := context.Background()
	srv, err := examples.NewBrandedServer(ctx, "repo-app", examples.RepoUpstream(), examples.BasePolicy())
	if err != nil {
		t.Fatalf("NewBrandedServer: %v", err)
	}
	cli := connect(t, ctx, srv)

	// Surface is branded and narrowed: fetch + save, nothing else.
	got := listToolNames(t, ctx, cli)
	want := []string{"fetch", "save"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("branded tools = %v, want %v", got, want)
	}

	// No upstream name and no "lagom" leaks into the surface.
	res, _ := cli.ListTools(ctx, mcp.ListToolsRequest{})
	for _, tool := range res.Tools {
		blob, _ := json.Marshal(tool)
		low := strings.ToLower(string(blob))
		if strings.Contains(low, "lagom") {
			t.Errorf("tool %q surface leaks the word lagom: %s", tool.Name, blob)
		}
		if strings.Contains(low, "read_file") || strings.Contains(low, "write_file") {
			t.Errorf("tool %q leaks an upstream name: %s", tool.Name, blob)
		}
		// The pinned `repo` arg must not appear in the slim schema.
		if strings.Contains(string(tool.RawInputSchema), `"repo"`) {
			t.Errorf("tool %q slim schema must not expose the pinned `repo` arg: %s", tool.Name, tool.RawInputSchema)
		}
	}

	// Valid call: the upstream sees the injected pin + restored name.
	out, isErr := callText(t, ctx, cli, "fetch", map[string]any{"path": "/sandbox/a.txt"})
	if isErr {
		t.Fatalf("valid fetch must not error: %s", out)
	}
	var ran struct {
		Tool string                     `json:"tool"`
		Args map[string]json.RawMessage `json:"args"`
	}
	if err := json.Unmarshal([]byte(out), &ran); err != nil {
		t.Fatalf("decode upstream echo: %v (raw %s)", err, out)
	}
	if ran.Tool != "read_file" {
		t.Errorf("gate must map branded fetch -> upstream read_file, got %q", ran.Tool)
	}
	if string(ran.Args["repo"]) != `"hylla/lagom"` {
		t.Errorf("gate must inject the pinned repo, got %s", ran.Args["repo"])
	}

	// Constraint violation: path outside the sealed prefix is rejected + annotated.
	out, isErr = callText(t, ctx, cli, "fetch", map[string]any{"path": "/etc/passwd"})
	if !isErr {
		t.Fatalf("path outside the sealed prefix must reject, got success: %s", out)
	}
	if out == "" {
		t.Error("rejection must carry an annotation, got empty text")
	}
}

// TestTwoEphemeralProfilesTwoSurfaces proves the multi-agent story: TWO
// ephemeral profiles minted from the SAME base policy and the SAME upstream
// produce TWO different slim surfaces and TWO different enforcement envelopes —
// a policy is data, not a compiled artifact (SPEC.md §8; README "Many agents").
func TestTwoEphemeralProfilesTwoSurfaces(t *testing.T) {
	ctx := context.Background()
	up := examples.RepoUpstream()

	// Mint a reader profile and a writer profile from the same base.
	readerRec, err := examples.MintProfile(ctx, "agent-reader", "reader")
	if err != nil {
		t.Fatalf("mint reader: %v", err)
	}
	writerRec, err := examples.MintProfile(ctx, "agent-writer", "writer")
	if err != nil {
		t.Fatalf("mint writer: %v", err)
	}

	readerPolicy, err := examples.ResolvedPolicyOf(readerRec)
	if err != nil {
		t.Fatalf("resolve reader policy: %v", err)
	}
	writerPolicy, err := examples.ResolvedPolicyOf(writerRec)
	if err != nil {
		t.Fatalf("resolve writer policy: %v", err)
	}

	// Reader: only `fetch` is visible (save dropped by the overlay).
	readerSrv, err := examples.NewBrandedServer(ctx, "repo-app", up, readerPolicy)
	if err != nil {
		t.Fatalf("reader server: %v", err)
	}
	readerCli := connect(t, ctx, readerSrv)
	if got := listToolNames(t, ctx, readerCli); strings.Join(got, ",") != "fetch" {
		t.Fatalf("reader surface = %v, want [fetch]", got)
	}

	// Writer: both `fetch` and `save` visible, but `save` has `path` pinned away.
	writerSrv, err := examples.NewBrandedServer(ctx, "repo-app", up, writerPolicy)
	if err != nil {
		t.Fatalf("writer server: %v", err)
	}
	writerCli := connect(t, ctx, writerSrv)
	got := listToolNames(t, ctx, writerCli)
	if strings.Join(got, ",") != "fetch,save" {
		t.Fatalf("writer surface = %v, want [fetch save]", got)
	}

	// The two surfaces genuinely differ (the headline multi-agent assertion).
	if strings.Join(listToolNames(t, ctx, readerCli), ",") == strings.Join(got, ",") {
		t.Fatal("reader and writer surfaces must differ from the same binary")
	}

	// Writer's `save` schema must NOT expose `path` (pinned by the writer overlay)
	// — a per-agent narrowing the base did not have.
	wres, _ := writerCli.ListTools(ctx, mcp.ListToolsRequest{})
	for _, tool := range wres.Tools {
		if tool.Name == "save" && strings.Contains(string(tool.RawInputSchema), `"path"`) {
			t.Errorf("writer `save` must hide the pinned `path`: %s", tool.RawInputSchema)
		}
	}

	// Writer's save call must inject BOTH the base pin (repo) and the per-agent
	// pin (path) the agent never set.
	out, isErr := callText(t, ctx, writerCli, "save", map[string]any{"contents": "hi"})
	if isErr {
		t.Fatalf("writer save must succeed: %s", out)
	}
	var ran struct {
		Tool string                     `json:"tool"`
		Args map[string]json.RawMessage `json:"args"`
	}
	if err := json.Unmarshal([]byte(out), &ran); err != nil {
		t.Fatalf("decode save echo: %v (%s)", err, out)
	}
	if ran.Tool != "write_file" {
		t.Errorf("save must map to write_file, got %q", ran.Tool)
	}
	if string(ran.Args["path"]) != `"/sandbox/notes.md"` {
		t.Errorf("writer overlay must inject the pinned path, got %s", ran.Args["path"])
	}

	// Reader must NOT be able to reach save at all (it is not in its surface).
	if !callRejected(ctx, readerCli, "save", map[string]any{"contents": "x"}) {
		t.Error("reader must not be able to call save")
	}
}

// TestRefireReproducesSurface proves an ephemeral profile is reproducible: a
// MintRecord refired re-mints the recorded resolved policy, and a branded server
// built from the refired surface yields the SAME slim tools/list as the original
// — the refire-from-failure guarantee at the consumption level (SPEC.md §8.2).
func TestRefireReproducesSurface(t *testing.T) {
	ctx := context.Background()
	up := examples.RepoUpstream()

	record, err := examples.MintProfile(ctx, "agent-writer", "writer")
	if err != nil {
		t.Fatalf("mint: %v", err)
	}

	// Original surface from the freshly-minted record.
	origPolicy, err := examples.ResolvedPolicyOf(record)
	if err != nil {
		t.Fatalf("resolve original: %v", err)
	}
	origSrv, err := examples.NewBrandedServer(ctx, "repo-app", up, origPolicy)
	if err != nil {
		t.Fatalf("orig server: %v", err)
	}
	origNames := strings.Join(listToolNames(t, ctx, connect(t, ctx, origSrv)), ",")

	// Refire the record, build a server from the refired resolved policy.
	refired, err := lagom.Refire(ctx, record)
	if err != nil {
		t.Fatalf("Refire: %v", err)
	}
	refiredPolicy, err := examples.ResolvedPolicyOf(refired)
	if err != nil {
		t.Fatalf("resolve refired: %v", err)
	}
	refireSrv, err := examples.NewBrandedServer(ctx, "repo-app", up, refiredPolicy)
	if err != nil {
		t.Fatalf("refire server: %v", err)
	}
	refireNames := strings.Join(listToolNames(t, ctx, connect(t, ctx, refireSrv)), ",")

	if origNames != refireNames {
		t.Fatalf("refire must reproduce the surface: orig %q != refired %q", origNames, refireNames)
	}
	// And the refired resolved policy must equal the recorded one byte-for-byte.
	if !json.Valid(refiredPolicy) || string(refiredPolicy) != string(origPolicy) {
		t.Errorf("refired resolved policy must match the recorded one:\n orig: %s\nrefired: %s", origPolicy, refiredPolicy)
	}
}

// TestTwoUpstreamsComposeForOneAgent proves the multi-mcp story: two independent
// upstreams each wrapped by their own branded lagom server compose into one
// agent's toolset (SPEC.md §10; README "Many upstreams for one agent"). Each
// wrapper is its own slim, independently-branded surface; lagom narrows one
// upstream per projection and composition does the rest.
func TestTwoUpstreamsComposeForOneAgent(t *testing.T) {
	ctx := context.Background()

	// Wrap upstream #1 (repo) -> branded `fetch`.
	repoSrv, err := examples.NewBrandedServer(ctx, "repo-app", examples.RepoUpstream(), examples.BasePolicy())
	if err != nil {
		t.Fatalf("repo server: %v", err)
	}
	// Wrap upstream #2 (search) under its own brand: keep search as `lookup`,
	// pin the index the agent never sees.
	searchPolicy := []byte(`{
      "default_presence": "drop",
      "tools": {
        "search": {
          "presence": "keep",
          "rename": "lookup",
          "description": {"override": "Search index for query."},
          "args": {"index": {"pin": "docs"}}
        }
      }
    }`)
	searchSrv, err := examples.NewBrandedServer(ctx, "search-app", examples.SearchUpstream(), searchPolicy)
	if err != nil {
		t.Fatalf("search server: %v", err)
	}

	// One agent, two wrapped servers in its toolset (the harness would list both
	// in .mcp.json; here we connect a client to each).
	repoCli := connect(t, ctx, repoSrv)
	searchCli := connect(t, ctx, searchSrv)

	repoNames := listToolNames(t, ctx, repoCli)
	searchNames := listToolNames(t, ctx, searchCli)
	if strings.Join(repoNames, ",") != "fetch,save" {
		t.Errorf("repo wrapper surface = %v, want [fetch save]", repoNames)
	}
	if strings.Join(searchNames, ",") != "lookup" {
		t.Errorf("search wrapper surface = %v, want [lookup]", searchNames)
	}

	// Each wrapper enforces its own pin independently.
	out, isErr := callText(t, ctx, searchCli, "lookup", map[string]any{"query": "lagom"})
	if isErr {
		t.Fatalf("lookup must succeed: %s", out)
	}
	var ran struct {
		Tool string                     `json:"tool"`
		Args map[string]json.RawMessage `json:"args"`
	}
	if err := json.Unmarshal([]byte(out), &ran); err != nil {
		t.Fatalf("decode lookup echo: %v (%s)", err, out)
	}
	if ran.Tool != "search" || string(ran.Args["index"]) != `"docs"` {
		t.Errorf("search wrapper must map lookup->search and inject index=docs, got %s", out)
	}
}
