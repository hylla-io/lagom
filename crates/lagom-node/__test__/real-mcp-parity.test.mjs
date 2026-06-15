// Real-MCP parity: lagom-node `project()` must reproduce the Rust-produced slim
// projection of REAL captured MCP servers byte-for-byte (Node == Rust, NO DRIFT).
//
// `bench/raw/<server>.full.json` is the lagom passthrough projection of a real
// server's `tools/list`; `<server>.slim.json` is the Rust-produced sealed+allowlist
// projection — the source of truth. For each server we build the allowlist policy
// from the slim tool names, run the real full surface through the Node `project()`,
// re-serialize the core output into the MCP wire shape the stdio proxy emits
// (MCP-native camelCase `inputSchema`, `description` omitted when absent,
// alphabetical keys, 2-space indent — matching how the slim fixtures were
// captured), and assert the result equals the slim fixture byte-for-byte.
//
// Side effect: rewrites `bench/parity_real_node.jsonl` (one row per server +
// a leading `_note`) so the recorded parity result stays current with the binary.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync, writeFileSync } from "node:fs";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const lagom = require(join(here, "..", "index.js"));

const RAW = join(here, "..", "..", "..", "bench", "raw");
const PARITY_OUT = join(here, "..", "..", "..", "bench", "parity_real_node.jsonl");

const SERVERS = [
  "everything",
  "everything-2",
  "filesystem",
  "memory",
  "sequential-thinking",
];

// Re-serialize the core's projected defs into the MCP wire shape the stdio proxy
// emits and that the slim fixtures were captured in: MCP-native camelCase
// `inputSchema`, `description` omitted when absent, keys alphabetical, 2-space
// indent (json.dumps(..., indent=2)). This is the inverse of the lenient input
// the core now accepts on the way IN.
function toMcpWire(defs) {
  return defs.map((t) => {
    const o = {};
    if (t.description !== undefined && t.description !== null) {
      o.description = t.description;
    }
    o.inputSchema = t.input_schema;
    o.name = t.name;
    return o;
  });
}

function allowPolicy(slimDefs) {
  const tools = {};
  for (const t of slimDefs) tools[t.name] = { presence: "keep" };
  return JSON.stringify({ default_presence: "drop", tools });
}

const NOTE =
  "Byte-identity parity of lagom-node project() vs the Rust-produced " +
  "<server>.slim.json (the source of truth). Each row asserts the Node binding " +
  "reproduces the Rust slim projection byte-for-byte once the core output is " +
  "re-serialized into the MCP wire shape the stdio proxy emits (MCP-native " +
  "camelCase inputSchema, description omitted when absent, alphabetical keys, " +
  "2-space indent). These are byte-identity results, NOT token counts.";

test("project() matches the Rust slim projection of every real MCP server byte-for-byte", () => {
  const lines = [JSON.stringify({ _note: NOTE })];

  for (const server of SERVERS) {
    const full = readFileSync(join(RAW, `${server}.full.json`), "utf8");
    const slim = readFileSync(join(RAW, `${server}.slim.json`), "utf8");
    const fullDefs = JSON.parse(full);
    const slimDefs = JSON.parse(slim);

    const projected = JSON.parse(lagom.project(full, allowPolicy(slimDefs)));
    const rebuilt = JSON.stringify(toMcpWire(projected), null, 2);

    assert.equal(
      rebuilt,
      slim,
      `Node project() must equal ${server}.slim.json byte-for-byte (Node == Rust drift)`,
    );

    lines.push(
      JSON.stringify({
        server,
        full_tool_count: fullDefs.length,
        slim_tool_count: slimDefs.length,
        slim_byte_len: Buffer.byteLength(slim, "utf8"),
        match: rebuilt === slim,
      }),
    );
  }

  writeFileSync(PARITY_OUT, lines.join("\n") + "\n");
});
