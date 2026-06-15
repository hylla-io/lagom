# lagom-dynamic-mint

A loadable **skill** for a *host orchestrator* (`SPEC.md` §12, §8). This document
is inert markdown — lagom never loads it, connects an agent, or calls a model. A
host loads it to learn how to mint a per-agent **ephemeral projection**, wire it
into one consuming agent, and tear it down when that agent's life ends.

## Goal

Mint an **ephemeral projection** scoped to a **single consuming agent's
lifecycle** — for example constraining a `path` argument to the exact files that
agent may touch — wire it into that agent alone, and **tear it down at end of
life**. The projection exists only for that one agent and one run, then is
discarded (`SPEC.md` §8.2).

## Procedure

1. **Decide the per-agent narrowing.** Compute it from the agent's task: the
   precise files it may touch, the tools it needs, the values it is allowed.
   This may only **narrow** the integrator base (`SPEC.md` §5.2) — drop tools,
   pin args, tighten constraints. It can never widen the **sealed bounds**.
2. **Express it as an overlay.** Either a `lagom.toml` overlay layered on the
   integrator base, or a builder-face dynamic overlay computed at spawn. Example
   mint-time scoping: pin/constrain `path` to this agent's exact working set.
3. **Validate before wiring.** Run `lagom validate --config <overlay> -- <up>`
   against the live upstream. A non-zero exit means the projection references
   something the upstream no longer provides — do **not** wire a drifted
   projection (`SPEC.md` §5.3).
4. **Wire it into the one agent.** Run `lagom emit --config <overlay> -- <up>`
   to get the stdio-server snippet and place it in *that agent's* harness config
   only (`.mcp.json` / `settings.json`). lagom never edits harness config itself
   (`SPEC.md` §6.5), and the snippet is scoped to this agent — no other agent
   sees this projection.
5. **Tear down at end of life.** When the agent finishes (or is killed), remove
   its snippet and discard the overlay. The mint was ephemeral; nothing it
   minted should outlive the agent.

## Reproducibility despite ephemerality

Ephemeral does **not** mean untraceable. The mint records its fully-resolved
policy plus provenance — id, base profile, overlays, dynamic inputs (`SPEC.md`
§8.2). Because minting invokes no LLM and no nondeterministic input (`SPEC.md`
§5.4), the same sources mint a **byte-identical** projection, so a failed run can
be **refired** from its recorded resolved policy into an identical server. The
append-only **trace** links `run-id → mint record → each rewritten call`.

## Invariants

- One agent, one life: the projection is wired into a single agent and torn down
  when that agent dies — it is not a shared or persistent server.
- **Narrow-only**: any attempt to widen the base surfaces as a **load-time
  error**, never a silent ignore (`SPEC.md` §5.2).
- Ephemeral but **reproducible and traceable** (`SPEC.md` §8.2).
