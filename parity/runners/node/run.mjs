// Parity runner -- Node/TypeScript (napi-rs) face of the cross-binding NO-DRIFT
// guard. Reads the shared fixture (parity/fixtures/cases.json), runs every case
// through the built `lagom-node` addon, and prints a deterministic result map to
// stdout:
//
//   {"<case>": {"status": "ok"|"err", "payload": <json-string|null>}, ...}
//
// payload for an ok case is the exact JSON string the binding returned (the byte
// string every face must match); for an err case it is null (the binding throws,
// whose message framing legitimately differs across faces, so the guard compares
// only the ok/err classification for errors). The comparator diffs this against
// the Rust reference.
//
// Usage: node run.mjs <fixture-path> <binding-dir>
//   <binding-dir> is the lagom-node crate dir whose index.js re-exports the addon.

import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

const fixturePath = process.argv[2] ?? "parity/fixtures/cases.json";
const bindingDir = process.argv[3] ?? "crates/lagom-node";

// Import the built napi addon via its generated index.js (CJS) entrypoint.
const bindingEntry = pathToFileURL(resolve(bindingDir, "index.js")).href;
const lagom = (await import(bindingEntry)).default ?? (await import(bindingEntry));

const fixture = JSON.parse(readFileSync(fixturePath, "utf8"));
const upstreamJson = JSON.stringify(fixture.upstream);
const cases = fixture.cases;
const byName = new Map(cases.map((c) => [c.name, c]));

const ok = (payload) => ({ status: "ok", payload });
const errResult = () => ({ status: "err", payload: null });

const isNull = (v) => v === undefined || v === null;

function mintRecord(c) {
  const dynamicJson = isNull(c.dynamic) ? null : JSON.stringify(c.dynamic);
  const cmd = c.upstream_command;
  return lagom.mint(
    c.run_id,
    JSON.stringify(c.base),
    dynamicJson,
    cmd.command ?? "",
    cmd.args ?? [],
    cmd.env ?? [],
  );
}

function runCase(c) {
  try {
    switch (c.op) {
      case "project":
        return ok(lagom.project(upstreamJson, JSON.stringify(c.policy)));
      case "rewrite":
        return ok(lagom.rewrite(JSON.stringify(c.call), JSON.stringify(c.policy)));
      case "merge":
        return ok(lagom.merge(JSON.stringify(c.base), JSON.stringify(c.overlay)));
      case "validate":
        lagom.validate(JSON.stringify(c.policy), upstreamJson);
        // Match the canonical face: validate ok serializes as the JSON null.
        return ok("null");
      case "mint":
        return ok(mintRecord(c));
      case "refire": {
        const src = byName.get(c.mint_of);
        if (!src) throw new Error(`refire references unknown case ${c.mint_of}`);
        const record = mintRecord(src); // referenced mint must succeed
        return ok(lagom.refire(record));
      }
      default:
        throw new Error(`unknown op ${c.op}`);
    }
  } catch (e) {
    // A widening/reject/drift/malformed surfaces as a thrown Error -> err.
    // A genuinely-unexpected error (e.g. unknown op) re-throws to fail loud.
    if (e instanceof Error && /unknown op|references unknown case/.test(e.message)) {
      throw e;
    }
    return errResult();
  }
}

const out = {};
for (const c of cases) {
  out[c.name] = runCase(c);
}

// Sorted keys so the layout matches the other faces (the comparator parses
// structurally, but a stable order keeps a manual diff readable).
const sorted = {};
for (const name of Object.keys(out).sort()) {
  sorted[name] = out[name];
}
process.stdout.write(JSON.stringify(sorted, null, 2) + "\n");
