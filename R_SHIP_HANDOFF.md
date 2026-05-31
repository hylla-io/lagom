# R-SHIP-LAGOM — Handoff to Dev

**Date:** 2026-05-30
**Tillsyn refinement:** `331506f2-47d6-41a2-bfdd-152d40fb1e2a` (R-SHIP-LAGOM)
**Source-of-truth sibling:** `ta` (architecture) + `tillsyn` (CASCADE_METHODOLOGY.md canon)
**Memory rule:** `feedback_no_sibling_git_mutations` — orch wrote files only; ALL git is yours.

lagom is a **fresh Go-only sibling**. Orch laid down the full agent + build infrastructure but cannot `git init` / `go mod init` (yours — naming + git are dev decisions). This handoff is the bootstrap playbook.

---

## What orch wrote (no git touched)

| Path | What | Source |
|---|---|---|
| `bin/agent-dispatch.sh` + `bin/agent-audit-toon.py` | byte-identical dispatch + audit | ta |
| `.claude/hooks/ta_action_gate.py` + `.claude/hooks/post_tooluse_agent_audit.py` | byte-identical gate + audit hooks | ta |
| `.claude/agents/<persona>/settings.json` × 7 (Go-only) | per-persona tool gates, `mcp__tillsyn__*` stripped | ta |
| `.claude/agents/<persona>.md` × 7 | Go-only personas, Path B 2.2.A (tillsyn refs inert) | ta |
| `.claude/settings.json` | Go-only allow/deny + PreToolUse Bash hook + PostToolUse Agent hook | orch |
| `CASCADE_METHODOLOGY.md` | methodology canon (sha256 `87708e81…`) | tillsyn |
| `CLAUDE.md` | **generic** Go-quality CLAUDE.md (no domain specifics) | orch |
| `magefile.go` | canonical Go-only magefile, BYTE-IDENTICAL with bage (sha256 `7774bff…`) | orch (canonical) |
| `go.mod` | skeleton: `module github.com/hylla-io/lagom` + `go 1.26.2` + laslig require | orch |
| `cmd/lagom/main.go` | minimal compilable entrypoint | orch |
| `.gitignore` | standard agent-runtime ignores | orch |

The 7 Go-only personas: ta-go-builder, ta-go-build-qa-proof, ta-go-build-qa-falsification, ta-go-plan-qa-proof, ta-go-plan-qa-falsification, ta-go-planning, ta-closeout.

## The magefile is the canonical 12-target shape

`mage -l` (after bootstrap) will show: Test, TestPkg, TestFunc(pkg,fn), Race, RacePkg, Format, FormatFile, FormatCheck, Vet, VetPkg, Tidy, CI + Build + Clean, with hyphenated aliases (`check`→CI, `fmt`→Format, `format-check`, `format-file`, `test-func`, `test-pkg`, `race-pkg`, `vet-pkg`). It is BYTE-IDENTICAL to bage's magefile (project-agnostic: `Build` globs `cmd/*/main.go`).

---

## Bootstrap playbook (YOUR hands — use LATEST versions)

The dev directive is **use the latest versions of everything**. Run from `/Users/evanschultz/Documents/Code/hylla/lagom`:

1. **git init + remote**
   ```sh
   git init
   git branch -M main
   gh repo create hylla-io/lagom --private --source=. --remote=origin   # or your preferred remote setup
   ```
2. **Resolve modules at LATEST**
   ```sh
   go get github.com/evanmschultz/laslig@latest   # bumps the magefile's laslig dep to latest (skeleton pins v0.2.4)
   go mod tidy                                      # fills indirects + generates go.sum
   ```
   - The skeleton `go.mod` pins `laslig v0.2.4` (ta's current) as a sane floor; `@latest` + tidy moves it forward.
   - `mage` auto-installs `gofumpt@latest` on first Format/FormatCheck (the magefile's `ensureGofumpt`).
   - Use the latest `go` toolchain (skeleton declares `go 1.26.2`; bump the directive if you're on newer).
   - Install the latest mage: `go install github.com/magefile/mage@latest`.
3. **Verify the gate**
   ```sh
   mage ci   # FormatCheck + Vet + Cover (race+cover) + Tidy. 0 tests yet → Cover is 0% with no floor, passes.
   ```
4. **Smoke a persona** (optional; after `mage install`-equivalent + Claude Code restart from this dir)
   - Dispatch `ta-go-builder` with a tiny task; confirm `ta_action_gate.py` blocks `git commit` for the dispatched agent.
5. **Commit + push**
   ```sh
   git add -A   # or explicit; review `git status` first
   git commit -m "chore: bootstrap lagom — agent infra + canonical magefile + skeleton"
   git push -u origin main
   gh run watch --exit-status   # once a .github/workflows/ci.yml exists (orch writes it after you init — see below)
   ```
6. **Hylla ingest** (after push + CI green)
   ```
   mcp__hylla__hylla_ingest(source_url="https://github.com/hylla-io/lagom.git", ref="<SHA>", branch="main", enrichment_mode="full_enrichment", stream=true)
   ```
7. **Tell orch** the SHA + ingest task id so R-SHIP-LAGOM closes. Orch then writes `.github/workflows/ci.yml` (it deferred that to after `go mod init` so the module path is known).

---

## What goes in CLAUDE.md + how to structure it

The current `CLAUDE.md` is **intentionally generic** — it has the LOAD-BEARING cross-project scaffolding but NO domain content (lagom's domain isn't decided yet). When you start lagom for real, fill it in following this structure (mirrors tillsyn/ta/valv/sand CLAUDE.mds):

1. **Title + one-line scope** — `# CLAUDE.md — project guidance for lagom` + what lagom IS (the domain decision).
2. **Architecture & Cascade Tracking section** (KEEP — already present) — terse agent-infra sync record + the "ta MCP not tillsyn" rule (the P5 tidy moved the full synced-files list to this handoff).
3. **Cascade Methodology section** (KEEP — already present) — references `CASCADE_METHODOLOGY.md`; ta is the cascade-tracking MCP, NOT tillsyn.
4. **Go Development Rules** (KEEP/extend) — hexagonal, TDD, idiomatic, error-wrapping, etc. Add domain-specific architecture once decided (package layout, ports/adapters boundaries).
5. **Build Verification** (KEEP — already present) — the canonical 12-target shape table. Update only if lagom adds project-specific targets (like valv's Dev.*/Golden or poly's UI*).
6. **Hylla discipline** (KEEP — already present) — Go-only evidence source; update the artifact ref to `github.com/hylla-io/lagom@main` once the GitHub repo exists.
7. **Project Structure** (ADD when domain known) — package table (`cmd/lagom`, `internal/<domain>`, etc.) like tillsyn's.
8. **Tech Stack** (ADD when domain known) — the actual libs lagom uses.
9. **Domain-specific rules** (ADD) — whatever lagom's domain demands.

**P5 light tidy applied (2026-05-30):** undated the `2026-05-29 Architecture Sync (LOAD-BEARING)` header → `## Architecture & Cascade Tracking` (synced-files list moved here to the handoff — it duplicated CLAUDE.md), undated `## Cascade Methodology — Plan Down, Build Up`, added the TaskCreate dual-use note. ~6.3k, recursive flow stays in-file. **The generic skeleton otherwise stands — do NOT pare it further;** fill in domain sections (Project Structure, Tech Stack, domain rules) when lagom's domain is decided. Stage `CLAUDE.md` with the bootstrap commit.

---

## What's still orch's after you bootstrap

- `.github/workflows/ci.yml` (orch writes it post-`go mod init`, calling `mage ci`).
- Final R-SHIP-LAGOM verdict comment once you confirm SHA + CI green + ingest.

## Reference siblings

- ta (`/Users/evanschultz/Documents/Code/hylla/ta/main`) — the architecture source-of-truth (full FE+Go).
- sand / valv (`…/sand/main`, `…/valv/main`) — Go-only siblings with the same persona set + canonical magefile shape.
