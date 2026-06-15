// Smoke test for the lagom Node/TypeScript binding (SPEC.md §7.2).
//
// Runs against the `.node` addon + generated `index.js` built by the napi CLI
// (see the `just node-build` recipe). Proves the binding imports and that the
// headline transforms work end-to-end through Node — identical assertions to the
// Python smoke test (NO DRIFT): a sealed policy drops a tool and a pin prunes an
// argument from the projected schema, and rewrite() rejects an out-of-enum call.

import assert from "node:assert/strict";
import { test } from "node:test";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
// The napi CLI emits index.js/index.d.ts + the .node addon in the crate root.
const lagom = require(join(here, "..", "index.js"));

const upstream = JSON.stringify([
  {
    name: "search",
    description: "Search.",
    input_schema: {
      type: "object",
      properties: {
        artifact: { type: "string" },
        query: { type: "string" },
      },
      required: ["artifact", "query"],
    },
  },
  {
    name: "write_file",
    description: "Write a file.",
    input_schema: { type: "object", properties: {} },
  },
]);

test("import exposes the binding surface", () => {
  for (const name of [
    "project",
    "rewrite",
    "merge",
    "validate",
    "mint",
    "refire",
    "mintStdioServer",
    "shippedSkills",
    "emitSkills",
    "PolicyBuilder",
    "Guard",
  ]) {
    assert.ok(name in lagom, `missing export: ${name}`);
  }
});

test("mint() then refire() round-trips an ephemeral projection (SPEC.md §8.2)", () => {
  // base keeps `search`; a per-agent dynamic overlay drops it (a legal narrowing).
  const base = JSON.stringify({
    default_presence: "keep",
    tools: { search: { presence: "keep" } },
  });
  const dynamic = JSON.stringify({
    default_presence: "keep",
    tools: { search: { presence: "drop" } },
  });

  const record = JSON.parse(lagom.mint("agent-7", base, dynamic, "srv", ["-y"]));
  assert.equal(record.run_id, "agent-7");
  assert.equal(
    record.resolved.policy.tools.search.presence,
    "drop",
    "dynamic overlay must drop search in the resolved policy",
  );

  const refired = JSON.parse(lagom.refire(JSON.stringify(record)));
  assert.deepEqual(
    refired,
    record.resolved,
    "refire reproduces the recorded resolved projection",
  );

  // A widening dynamic overlay is rejected (the sandbox enforcement).
  const sealedBase = JSON.stringify({
    default_presence: "keep",
    tools: { search: { presence: "drop" } },
  });
  const widening = JSON.stringify({
    default_presence: "keep",
    tools: { search: { presence: "keep" } },
  });
  assert.throws(() => lagom.mint("r", sealedBase, widening, "srv"));
});

test("Guard wires a slim, branded MCP in two calls (SPEC.md §2, §7.2)", () => {
  // The branded policy: keep `search` under the app's OWN name `find` with the
  // app's own description, pin `artifact` (hidden), drop the rest. lagom invisible.
  const policy = JSON.stringify({
    default_presence: "drop",
    tools: {
      search: {
        presence: "keep",
        rename: "find",
        description: { override: "App find." },
        args: { artifact: { pin: "hylla" } },
      },
    },
  });

  const guard = new lagom.Guard(upstream, policy);

  // slimDefs(): branded name + description, write_file dropped, pin hidden.
  const defs = JSON.parse(guard.slimDefs());
  assert.deepEqual(
    defs.map((d) => d.name),
    ["find"],
    "branded name, rest dropped",
  );
  assert.equal(defs[0].description, "App find.", "app's own description");
  assert.ok(!("artifact" in defs[0].input_schema.properties), "pin hidden");
  assert.ok("query" in defs[0].input_schema.properties);

  // gate(): branded name maps back to upstream, pin injected.
  const gated = JSON.parse(
    guard.gate(JSON.stringify({ name: "find", arguments: { query: "x" } })),
  );
  assert.equal(gated.name, "search", "branded name maps back to upstream");
  assert.equal(gated.arguments.artifact, "hylla", "pin injected");

  // gate(): dropped tool and the hidden original name both reject.
  for (const bad of ["write_file", "search"]) {
    assert.throws(
      () => guard.gate(JSON.stringify({ name: bad, arguments: {} })),
      `gate must reject ${bad}`,
    );
  }
});

test("shipped skills are bundled (SPEC.md §12)", () => {
  const bundled = new Map(lagom.shippedSkills());
  assert.deepEqual(
    new Set(bundled.keys()),
    new Set(["lagom-slim-docs.skill.md", "lagom-dynamic-mint.skill.md"]),
  );
  for (const body of bundled.values()) {
    assert.ok(body.trim().length > 0);
  }
});

test("project() drops a tool and pins+hides an arg", () => {
  const b = lagom.PolicyBuilder.sealed();
  b.keep("search");
  b.pin("search", "artifact", JSON.stringify("hylla"));
  const policy = b.build();

  const projected = JSON.parse(lagom.project(upstream, policy));

  const names = projected.map((d) => d.name);
  assert.deepEqual(names, ["search"], "write_file must be dropped");

  const props = projected[0].input_schema.properties;
  assert.ok(!("artifact" in props), "pinned arg must be pruned from the schema");
  assert.ok("query" in props, "unpinned arg must survive");
});

test("rewrite() injects the pin and rejects an out-of-enum call", () => {
  const b = new lagom.PolicyBuilder();
  b.pin("search", "artifact", JSON.stringify("hylla"));
  b.constrainEnum("search", "query", JSON.stringify(["a", "b"]));
  const policy = b.build();

  const ok = JSON.parse(
    lagom.rewrite(
      JSON.stringify({ name: "search", arguments: { query: "a" } }),
      policy,
    ),
  );
  assert.equal(ok.arguments.artifact, "hylla", "pin must be injected");

  assert.throws(
    () =>
      lagom.rewrite(
        JSON.stringify({ name: "search", arguments: { query: "z" } }),
        policy,
      ),
    /violates its constraint/i,
    "out-of-enum query must throw, annotated with the violated bound (SPEC.md §9.1)",
  );
});

test("merge() rejects a widening overlay (sandbox enforcement)", () => {
  const base = lagom.PolicyBuilder.sealed().build();
  const over = new lagom.PolicyBuilder();
  over.keep("search");
  assert.throws(() => lagom.merge(base, over.build()));
});

test("validate() fails loud on drift", () => {
  const b = new lagom.PolicyBuilder();
  b.pin("ghost", "x", "1");
  assert.throws(() => lagom.validate(b.build(), upstream));
});

test("malformed JSON throws rather than panics", () => {
  assert.throws(() => lagom.project("not json", "{}"));
});
