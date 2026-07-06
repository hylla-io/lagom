# RELEASING lagom (v0.1.0 and on)

lagom is **UNRELEASED**. Nothing here runs until the maintainer **expressly**
says to release — tagging + publishing + going public are deliberate, mostly
irreversible acts. This file is the exact procedure; the pipelines are wired but
inert until a tag is pushed.

**Safety property:** a pre-release tag (any tag containing `-`, e.g.
`v0.1.0-rc.1`) runs every *build* job in `release.yml` but **publishes
nothing** — the crate/wheel/npm versions carry no rc suffix, so publishing on an
rc tag would irrevocably ship the final version number. The rc tag *is* the dry
run.

**Registry-optional:** the primary install path for every face is straight from
git (`cargo install --git`, git dependencies, `pip install git+…`, `go get`),
which needs no registry account on either side. Each registry publish job in
`release.yml` **skips green when its token secret is absent**, so a tag-only
release is a complete release; add tokens per registry whenever wider
distribution is wanted.

## 0. Prerequisites (one-time)

- **Decide public.** ✅ Done 2026-07-06 — the repo is public; Dependabot
  alerts/security updates and private vulnerability reporting are enabled;
  branch protection requires the seven CI jobs.
- **Add repo secrets — each optional, per registry** (Settings → Secrets →
  Actions): `CARGO_REGISTRY_TOKEN` (crates.io), `PYPI_API_TOKEN` (PyPI),
  `NPM_TOKEN` (npm registry — used by `pnpm publish`; the npm *client* is never
  used). A missing token skips that registry's publish job green.
- **Names**: PyPI **`hylla-lagom`** (the PyPI name `lagom` is an unrelated DI
  library; the module still imports as `lagom`), npm **`@hylla-io/lagom`** plus
  the five platform packages `@hylla-io/lagom-{darwin-x64,darwin-arm64,
  linux-x64-gnu,linux-arm64-gnu,win32-x64-msvc}` (published automatically), and
  crates.io **`lagom-core`, `lagom-audit`, `lagom-config`, `lagom-proxy`,
  `lagom-cli`** (the full chain, so `cargo install lagom-cli` works).
- **Enable Dependabot + private vulnerability reporting** (Settings →
  Security). `.github/dependabot.yml` enforces the 72-hour update cooldown;
  `SECURITY.md` documents the reporting channel.
- **Branch protection** on `main`: require the `ci` workflow's jobs
  (check, e2e, python, node, wasm-go, parity, supply-chain) green before merge.

## 1. Version + gate (every release)

1. Confirm one version everywhere: `Cargo.toml [workspace.package].version`
   (also referenced by the `lagom-*` entries in `[workspace.dependencies]`),
   `crates/lagom-node/package.json` `version` **and its
   `optionalDependencies` pins**; the Python wheel version derives from Cargo.
   Bump together.
2. Green the full gate: `just ci && just e2e && just go-test && just node-test
   && just py-test && just parity && just examples`. CI mirrors all of these
   plus the `supply-chain` job (`cargo deny check` + `govulncheck`).
3. Update `CHANGELOG.md`. Commit.

## 2. Always dry-run first (the rc tag)

```sh
git tag v0.1.0-rc.1 && git push origin v0.1.0-rc.1
```

This fires `release.yml` with publishing disabled: the crates are `cargo
package`d (metadata check), wheels build on all three OSes, and the napi addon
builds for all five npm targets. Confirm everything is green, download and
spot-check the artifacts, then proceed.

## 3. Release — one tag does everything

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`release.yml` then:

- **crates** — publishes the chain in dependency order: `lagom-core` →
  `lagom-audit` → `lagom-config` → `lagom-proxy` → `lagom-cli`
  (`CARGO_REGISTRY_TOKEN`). Consumers: `cargo install lagom-cli`, or
  `lagom-core = "0.1"` as a library.
- **python-build/publish** — manylinux + macOS + Windows wheels → PyPI as
  **`hylla-lagom`** (`PYPI_API_TOKEN`). Consumers: `pip install hylla-lagom` /
  `uv add hylla-lagom`, then `import lagom`.
- **npm-build/publish** — napi addon per target (darwin x64/arm64, linux
  x64/arm64-gnu, win32 x64-msvc), then `napi create-npm-dirs` + `napi
  artifacts` and `pnpm publish` of the five platform packages plus the root
  `@hylla-io/lagom` (`NPM_TOKEN`; no npm client anywhere). Consumers:
  `npm install @hylla-io/lagom` (or bun/pnpm equivalents).
- **go-tag** — pushes `go/v0.1.0` at the same commit; the `/go` subdirectory
  module publishes by that tag alone. Consumers:
  `go get github.com/hylla-io/lagom/go@v0.1.0` (drop `GOPRIVATE` once public).
  Verify on pkg.go.dev.

## 4. After publish

- Bump sand (and other consumers) off the pseudo-version to `@v0.1.0`; have sand
  delete `MapUpstreamDefs` (core now accepts raw MCP defs) and strip the
  `lagom: rewrite:` prefix when relaying gate rejections to an agent.
- Update the root README install section: swap the from-git instructions to the
  registry ones (they are written side by side; delete the "until published"
  qualifiers).
- If public: the article (`docs/ARTICLE_LAGOM.md`) is "clone and run it" —
  confirm `just build && python3 bench/bench.py` reproduces from the public
  clone.

## Gate summary

| face | registry | trigger | secret | status |
|---|---|---|---|---|
| every face | **git (primary)** | tag `vX.Y.Z` (pin) or `main` | — | live — public repo, no account needed |
| Rust chain (5 crates) | crates.io | tag `vX.Y.Z` | `CARGO_REGISTRY_TOKEN` | wired; skips green when token unset |
| Python `hylla-lagom` | PyPI | tag `vX.Y.Z` | `PYPI_API_TOKEN` | wired; skips green when token unset |
| Node `@hylla-io/lagom` + 5 platform pkgs | npm (via pnpm) | tag `vX.Y.Z` | `NPM_TOKEN` | wired; skips green when token unset |
| Go `…/go` | git/pkg.go.dev | tag `vX.Y.Z` → auto `go/vX.Y.Z` | — | wired (release.yml go-tag job) |

Publishing to crates.io becomes non-optional the day a downstream crate must
itself publish to crates.io with lagom as a dependency (crates.io forbids git
dependencies in published crates).

Nothing above happens without an explicit tag push by the maintainer, and a
`-rc` tag never publishes.
