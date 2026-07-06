# Security policy

lagom is a security-relevant library: it narrows what an agent can see and do
through a wrapped MCP server. Its exact enforcement boundary — what it does and
deliberately does not defend against — is documented in the
[Security model](README.md#security-model--what-lagom-does-and-does-not-enforce)
section of the README. Please read that before reporting; "lagom does not
restrict a server it does not wrap" is by design, not a vulnerability.

## Reporting a vulnerability

**Do not open a public issue for a security bug.** Use GitHub's private
vulnerability reporting ("Report a vulnerability" under the repo's Security
tab). Include a minimal reproduction — ideally a policy + a JSON-RPC transcript
showing a call that reaches the upstream without being rewritten, or a
projection that leaks something the policy says is hidden.

What counts as a vulnerability (examples):

- A `tools/call` that reaches the upstream bypassing `rewrite` (pin injection,
  constraint checks, drops) — e.g. via message framing, parser differentials,
  or protocol shapes the bridge forwards unexamined.
- A `merge`/`mint` overlay that **widens** the sealed base without a
  `MergeError`.
- A projected `tools/list` that leaks a pinned/hidden argument or a dropped
  tool.
- Determinism breaks that make a refired projection differ from its record.

You should expect an acknowledgement within a few days. Until a fix ships,
coordinated disclosure is appreciated.

## Supply chain

- CI runs `cargo deny check` (RustSec advisories, license allowlist, source
  policy) and `govulncheck` on every push and PR.
- Dependabot updates run with a **72-hour cooldown** (`cooldown.default-days: 3`
  in `.github/dependabot.yml`): no dependency version younger than 3 days is
  ever proposed, shrinking the freshly-poisoned-release window. The same rule
  binds **manual** bumps in any ecosystem: no version younger than 72 hours is
  adopted, security fixes included (project policy, `CLAUDE.md`).
- Releases fire only from maintainer-pushed version tags
  (`.github/workflows/release.yml`); pre-release tags publish nothing.
