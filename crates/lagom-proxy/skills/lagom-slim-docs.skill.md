# lagom-slim-docs

A loadable **skill** for a *host orchestrator* (`SPEC.md` §12, §4.2 Tier 0).
This document is inert markdown — lagom never loads it, never connects an agent,
and **never calls a model at runtime**. A host loads it to learn how to author
caveman docs once, at authoring time, and commit them as Tier-1 overrides.

## Why lagom stays model-free

lagom's minting is **deterministic**: the same `(profile, overlays, dynamic
inputs)` produce a byte-identical resolved policy, which is the precondition for
**refire** — re-minting a failed run into an identical server (`SPEC.md` §5.4,
§8.2). A runtime LLM call would inject nondeterminism and break that guarantee,
so doc-slimming is pushed entirely out of lagom and into *this* host-side skill.
The model runs once; its output is committed text that lagom serves verbatim.

## Goal

For each tool a lagom **projection** exposes, rewrite its description in
**caveman** style — grammar-free, content-only, every unnecessary token removed.
The rewrite describes the *projected* (narrowed) **capability surface**, not the
upstream original: **pinned or dropped arguments appear nowhere**.

## Procedure

1. Obtain both surfaces. Run `lagom emit -- <upstream cmd>` to find the server,
   then read `tools/list` from the **upstream** directly and through **lagom**.
   The upstream def is the "full" def; the lagom-served def is the "slim" def.
2. Diff the slim `inputSchema` against the upstream one, per kept tool. Record:
   - args **removed** (pinned) — these must never appear in the slim text;
   - args **constrained** (enum / range / pattern) — state the surviving bound;
   - args that **pass through** unchanged.
3. Use a **small** model to rewrite each description so it:
   - mentions only arguments the agent can actually set;
   - **never** names a pinned or dropped argument;
   - states each surviving constraint inline (e.g. `path` one of a, b);
   - strips filler, grammar, articles, and any token that does not change
     behaviour — caveman style.
4. Commit each rewritten string as a **Tier-1 `description` override** in
   `lagom.toml` (`[tools.<name>] description = "..."`). Once committed it is
   deterministic at runtime (`SPEC.md` §4.2: a shipped-skill Tier-0 output
   becomes a committed Tier-1 override).

## Invariants

- A slim override must **never contradict the schema** (`SPEC.md` §4.2): if the
  schema still exposes an arg, the text may not claim it is fixed, and vice
  versa.
- The original upstream description is retained in lagom's **audit log**; the
  slim text is only what the agent sees.
- **No runtime LLM call.** The model runs once during authoring; lagom serves
  the committed text deterministically and can refire from it unchanged.
