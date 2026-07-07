# Roadmap

## Distribution

lagom **v0.1.0 is released git-first**: every face installs straight from this
repo (`cargo install --git`, git dependencies, `pip install git+…`, `go get` —
see the [README install section](README.md#install)). Go is fully released —
`go get github.com/hylla-io/lagom/go@v0.1.0` — since Go needs no registry.

**Registry publishing is planned, not abandoned.** The pipeline is already
wired and proven (the release workflow builds wheels for three OSes and the
napi addon for five targets on every tag; publish jobs skip green until their
token secret exists). Each registry switches on the day its token is added —
no code changes needed:

| registry | package(s) | status | planned trigger |
|---|---|---|---|
| crates.io | `lagom-core`, `lagom-audit`, `lagom-config`, `lagom-proxy`, `lagom-cli` | planned | first downstream crate that must itself publish to crates.io (git deps are forbidden there), docs.rs, or name-squat protection — whichever bites first |
| PyPI | `hylla-lagom` (imports as `lagom`) | planned | first Python consumer who shouldn't need a Rust toolchain |
| npm | `@hylla-io/lagom` + 5 platform packages | planned | first TS/Node consumer beyond path installs |

Until then, pin git tags (`v0.1.0`) for reproducible installs.

## Engine (deferred by design, `SPEC.md` §11)

- Resource / prompt narrowing (0.1.0 narrows tools only; resources/prompts
  pass through — see the README security model).
- HTTP / SSE / streamable-HTTP transport; attach-to-running upstream.
- Per-call authorization hooks (`SPEC.md` §8.3).
- Restart/supervision policy for the upstream child.

## Standing policies

- **72-hour dependency cooldown** — no dependency version younger than 72h is
  ever adopted, automated or manual (`SECURITY.md`, `.github/dependabot.yml`).
- Releases fire only from maintainer-pushed tags; pre-release (`-rc`) tags
  build everything and publish nothing.
