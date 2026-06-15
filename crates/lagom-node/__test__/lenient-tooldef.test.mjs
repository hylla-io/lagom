// Lenient ToolDef deserialization, proven through the Node binding (lagom-core
// `tooldef.rs`). The core accepts the MCP-native camelCase `inputSchema` as an
// alias of `input_schema` and normalizes a missing or JSON-`null` schema to `{}`,
// so a caller can hand `project()` the raw upstream `tools/list` with no
// field-mapping shim. Output stays canonical snake_case `input_schema` (the
// downstream contract is unchanged). NO DRIFT vs the Rust `tooldef.rs` tests.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const lagom = require(join(here, "..", "index.js"));

// A passthrough policy: keep every tool unchanged, so the only transform is the
// core's own (de)serialization — isolating the lenient-input behavior.
const passthrough = JSON.stringify({ default_presence: "keep" });

test("project() accepts a RAW MCP surface with no field-mapping shim", () => {
  // (a) camelCase `inputSchema`, (b) `inputSchema: null`, (c) schema omitted —
  // exactly the shapes a real MCP `tools/list` emits. All three must succeed.
  const rawMcp = JSON.stringify([
    {
      name: "camel",
      description: "camelCase inputSchema.",
      inputSchema: { type: "object", properties: { q: { type: "string" } } },
    },
    { name: "null_schema", description: "null schema.", inputSchema: null },
    { name: "missing_schema", description: "no schema key." },
  ]);

  const projected = JSON.parse(lagom.project(rawMcp, passthrough));

  assert.deepEqual(
    projected.map((d) => d.name),
    ["camel", "null_schema", "missing_schema"],
    "all three raw-MCP tools must project (none rejected)",
  );

  // Projected output is canonical snake_case `input_schema` — never camelCase.
  for (const def of projected) {
    assert.ok(
      "input_schema" in def,
      `projected output must use snake_case input_schema (${def.name})`,
    );
    assert.ok(
      !("inputSchema" in def),
      `projected output must NOT carry camelCase inputSchema (${def.name})`,
    );
  }

  // (a) camelCase input is carried through verbatim under the canonical key.
  assert.deepEqual(projected[0].input_schema, {
    type: "object",
    properties: { q: { type: "string" } },
  });

  // (b) null schema and (c) missing schema both normalize to the empty object {}.
  assert.deepEqual(projected[1].input_schema, {}, "null schema normalizes to {}");
  assert.deepEqual(projected[2].input_schema, {}, "missing schema normalizes to {}");
});
