# RELEASING lagom (v0.1.0 and on)

lagom is **UNRELEASED**. Nothing here runs until the maintainer **expressly**
says to release — tagging + publishing + going public are deliberate, mostly
irreversible acts. This file is the exact procedure; the pipelines are wired but
inert until a tag is pushed.

## 0. Prerequisites (one-time)

- **Decide public.** A public article + `pip`/`npm`/`go get`/pkg.go.dev need the
  GitHub repo public. Flip it in repo Settings → Danger Zone. (Irreversible-ish;
  maintainer's call.)
- **Add repo secrets** (Settings → Secrets → Actions): `CARGO_REGISTRY_TOKEN`
  (crates.io), `PYPI_API_TOKEN` (PyPI). For Node: an `NPM_TOKEN` once the npm
  flow is wired (§4).
- **Claim names** if going public: PyPI `lagom` (pyproject `[project].name`) and
  npm `@hylla-io/lagom` (already scoped). `lagom-core` on crates.io.

## 1. Version + gate (every release)

1. Confirm one version everywhere: `Cargo.toml [workspace.package].version`,
   `crates/lagom-node/package.json` `version`; Python wheel version is derived
   from Cargo. Bump together.
2. Green the full gate: `just ci && just go-test && just node-test && just
   py-test && just parity && just examples`. CI mirrors all of these incl. the
   new `parity` job.
3. Update `CHANGELOG`/handoff. Commit.

## 2. Always dry-run first

Push a **pre-release** tag and confirm the `release` workflow is green before the
real one:

```sh
git tag v0.1.0-rc.1 && git push origin v0.1.0-rc.1   # fires release.yml (rc)
```

`cargo publish` supports `--dry-run`; PyPI upload uses `--skip-existing`. Verify
artifacts before the real tag.

## 3. Release the wired faces (crates.io + PyPI)

Pushing a `vX.Y.Z` tag fires `.github/workflows/release.yml`:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

- **crate** job → `cargo publish -p lagom-core` (needs `CARGO_REGISTRY_TOKEN`).
- **python-build/publish** jobs → manylinux + macOS + Windows wheels → PyPI
  (needs `PYPI_API_TOKEN`). Consumers then `pip install lagom`.

## 4. Release Go + Node (maintainer steps — not yet in release.yml)

- **Go** (no CI needed — Go modules publish by git tag; the binding is the `/go`
  subdirectory module, so the tag is **prefixed**):
  ```sh
  git tag go/v0.1.0 && git push origin go/v0.1.0
  ```
  Consumers then `go get github.com/hylla-io/lagom/go@v0.1.0` (drop `GOPRIVATE`
  once the repo is public). Verify on pkg.go.dev.
- **Node** (napi multi-platform) — the napi prebuild→npm flow needs a verified
  matrix before wiring into `release.yml` (per-target `.node` builds published as
  platform sub-packages via `napi prepublish`, main `@hylla-io/lagom` pulling them
  as `optionalDependencies`). Until then, publish from a clean checkout:
  ```sh
  cd crates/lagom-node && npm ci && npm run build && npm publish --access public
  ```
  (single-platform; add the matrix to `release.yml` once dry-run-verified). Needs
  `npm login` / `NPM_TOKEN`.

## 5. After publish

- Bump sand (and other consumers) off the pseudo-version to `@v0.1.0`; have sand
  delete `MapUpstreamDefs` (core now accepts raw MCP defs) and strip the
  `lagom: rewrite:` prefix when relaying gate rejections to an agent.
- If public: the article (`docs/ARTICLE_LAGOM.md`) is "clone and run it" — confirm
  `just build && python3 bench/bench.py` reproduces from the public clone.

## Gate summary

| face | registry | trigger | secret | status |
|---|---|---|---|---|
| Rust `lagom-core` | crates.io | tag `vX.Y.Z` | `CARGO_REGISTRY_TOKEN` | wired (release.yml) |
| Python `lagom` | PyPI | tag `vX.Y.Z` | `PYPI_API_TOKEN` | wired (release.yml) |
| Go `…/go` | git/pkg.go.dev | tag `go/vX.Y.Z` | — | maintainer step (§4) |
| Node `@hylla-io/lagom` | npm | manual | `NPM_TOKEN` | maintainer step; matrix TODO (§4) |

Nothing above happens without an explicit tag push by the maintainer.
