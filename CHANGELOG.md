# Changelog

All notable changes to lagom. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions follow SemVer
(pre-1.0: breaking changes may land in minor versions).

## [Unreleased]

## [0.1.1] — 2026-07-29

Security release: three authority/diagnostic defects found by adversarial
review of the shipped 0.1.0 surface, plus one privacy defect in the published
Go blob. No API removals; one additive proxy API and one deliberate widening
in `merge` (both below).

### Security

- **`tools/list` projection could be bypassed by a spec-compliant upstream**
  (`682538c`, `crates/lagom-proxy/src/bridge.rs`). Projection was gated on id
  correlation keyed on an exact `serde_json::Value`, so a peer that echoed a
  numerically-equal id in another representation (`2.0` → `2`, or `"2"`) missed
  the pending entry and its response took the verbatim-forward arm — handing
  the agent the RAW upstream surface, dropped tools and pinned arguments
  included. The id representation is downstream-controlled, so no misbehaving
  server was required. Two independent defences now: ids normalise to one
  correlation key (`IdKey`), and projection is **shape-gated** — a message
  whose `/result/tools` is an array is narrowed whether or not it correlated;
  correlation only decides whether the event is logged as anomalous. What is
  enforced: for an upstream line `serde_json` parses into a single JSON
  *object*, the surface is projected before it is written downstream, and the
  arm that cannot re-serialise the projected value emits a JSON-RPC internal
  error instead of the original bytes. Locked by
  `float_id_echoed_as_integer_is_still_projected`,
  `integer_id_echoed_as_float_is_still_projected`,
  `integer_id_echoed_as_string_is_still_projected`,
  `string_id_echoed_as_integer_is_still_projected`,
  `uncorrelated_tool_surface_is_projected_not_leaked`,
  `unkeyable_request_id_still_yields_a_projected_response`,
  `server_request_reusing_a_pending_id_does_not_evict_it`.
  Paths that sentence does **not** cover, carried forward from the code's own
  notes: a top-level JSON-RPC batch array defeats both gates, so
  `pump_upstream` now **drops** it
  (`batched_upstream_surface_is_dropped_not_forwarded`) — its live
  reachability is unverified, so treat the drop as defence-in-depth, not a
  closed attack path; an upstream line that is not valid JSON is still
  forwarded verbatim (real servers log to stdout); a tool surface at any
  pointer other than `/result/tools` is passthrough by construction.
- **`tools/call` naming a case- or whitespace-variant of a dropped tool was
  forwarded upstream instead of refused** (`d373b47`; claims scoped to their
  locks in `e95a48f`). Under a default-keep policy that drops `search`, the
  names `SEARCH`, `Search`, `"search "` and `""` (the proxy's default for a
  missing `params.name`) fell through to `default_presence` and were
  forwarded, so an upstream that matches names leniently could run the dropped
  tool with none of its presence/pin/constraint rules applied. Those names are
  now refused, with the shadowed governed name stated in the rejection. Case
  folding is used **to deny only**, never to widen a match. Locked by
  `case_variant_of_a_governed_name_is_refused`,
  `case_variant_cannot_evade_a_pin`,
  `whitespace_variant_is_refused_listed_or_not`,
  `empty_tool_name_is_refused_under_passthrough`,
  `rename_still_resolves_exactly_while_its_variants_are_refused`.
  Residuals: the refusal covers case and whitespace variants of names the
  policy *governs* (rule keys plus renames); other spellings an upstream might
  resolve leniently — interior zero-width/bidi characters, NFKC-equivalent or
  full-width forms, `-`/`_` swaps — are not caught unless `default_presence`
  is `drop`. An **ungoverned** name is still forwarded verbatim under
  `default_presence = keep` (`SPEC.md` §3's specified default, locked by
  `unrelated_unlisted_name_still_passes_through`); closing that requires
  retaining the upstream `tools/list` surface, which `rewrite`'s
  `(&ToolCall, &Policy)` signature does not admit.
- **A resolved policy that gates nothing now says so** (`7d64e37`). `serve`,
  `refire` and `validate` print a stderr diagnostic when the resolved policy
  restricts nothing, naming the cause and the remedy — previously such a run
  was silent, so an operator could believe they were gated when they were not.
  The predicate is semantic, not structural equality with the default:
  `rename`, a `description` override, an arg `default`, an explicit
  `presence = "keep"` and a bare `[tools.<name>]` table all leave every tool
  callable with every argument, and a 0-byte or comment-only `lagom.toml` *is*
  discovered yet lowers to passthrough. When discovery found nothing, the
  searched candidate paths are listed. stderr only — `serve`/`refire` own
  stdout as the JSON-RPC channel — and never a refusal to start:
  passthrough-when-unconfigured stays specified behavior (`SPEC.md` §3).
  Residual: any policy/tool/arg shape this build does not recognise counts as
  restricting so the warning stays silent (printing "nothing is gated" about a
  value we could not read would be a false claim), so a future
  *non*-restricting `ArgPolicy`/`ToolPolicy` variant re-opens the silent case
  until the recognised-key lists are extended.
- **Privacy: the committed `lagom.wasm` embedded absolute build-machine
  paths** (`0b6bc44`). The blob published as `go/v0.1.0` carries 71
  occurrences of `/Users/<developer>` — the developer's macOS home path and
  username — as panic-location metadata (`file!()` / `#[track_caller]` strings
  in `.rodata`), which `strip = true`, LTO and `opt-level` do **not** remove
  (measured 2026-07-29 with `rg -a -o '/Users/' <blob> | wc -l`: 71 at
  `v0.1.0`, 84 for the intermediate rebuild at `e6c2976`, **0** at
  `0b6bc44`). `just wasm` now builds with `--remap-path-prefix` over the
  registry, the rustup toolchain, the sysroot and the workspace root, and
  fails the recipe if `strings … | rg "$HOME|$(whoami)"` matches again.
  Cargo's `trim-paths` would be the idiomatic fix but is unstable in cargo
  1.97.0. Scope of the remap: those four root families, plus sysroot paths
  rustc already rewrites to `/rustc/<hash>/`; it is not a claim that the blob
  is free of every machine-specific string, and the tripwire is a textual grep
  of two values, so it does not detect a leak containing neither.
  **Consumer impact — the old artifact cannot be recalled.** The blob ships
  inside the `go/vX` module zip, which the Go module proxy caches immutably
  (`justfile`, `wasm` recipe), so `go/v0.1.0` keeps serving the leaking blob
  for as long as that cache exists. Go consumers must move to `go/v0.1.1` to
  get the de-leaked artifact; no fix reaches an already-published version. The
  Rust, npm and PyPI faces do not embed this blob.

### Fixed

- **`merge` sealed every rename against a synthesized base, making `rename`
  unexpressible through the mint faces** (`88cb040`). Sealing keyed on the
  overlay *supplying* a rename rather than on a naming bound having been *set*,
  and a tool the base never mentioned is synthesized from
  `ToolPolicy::default()` — which is how the bindings arrive
  (`lagom-node/src/lib.rs:221` and `lagom-py/src/lib.rs:222` mint with
  `config_paths: []`, so `lagom_proxy::mint` folds the whole built policy as an
  overlay onto `Policy::passthrough()`; the Go face reaches `merge` directly).
  Now `(base None, overlay Some)` is admitted and `(base Some, overlay Some)`
  passes only as an identical restatement; redirecting a base-set rename is
  still rejected (`overlay_may_name_a_tool_the_base_never_bound`,
  `overlay_may_name_a_listed_tool_with_no_rename_bound`,
  `overlay_may_not_redirect_a_sealed_rename`). Rename is vocabulary, not
  authority — every bound stays keyed by upstream tool, so a rename can neither
  detach a tool's pins/constraints nor borrow another tool's. Residuals: an
  overlay-authored name colliding with another kept tool's projected name is
  caught by `validate`'s projected-name check, which runs on the serve path
  (`spawn_and_validate`) but not in the in-process `Guard` face; a new name
  shadowing a kept tool while the renamed tool is dropped is admitted and makes
  the victim name uncallable — lost availability, not gained authority.
- **The bridge could outlive the session and orphan the upstream child**
  (`682538c`). Teardown awaited *both* pump directions (`tokio::join!`), so an
  upstream that answered the drift probe and then ignored stdin EOF kept lagom
  running after downstream EOF — `kill_on_drop` cannot fire while the future
  owning the `Child` is still suspended, and the child outlived a `SIGTERM`ed
  lagom at PPID 1. The first direction to close now ends the session
  (`tokio::select!`), the loser is aborted, and the child is terminated and
  reaped: bounded 2s wait for a self-exit after stdin close, then `SIGKILL` +
  `wait` (`downstream_eof_ends_the_session_and_reaps_a_stuck_upstream` asserts
  `ps` reports no such pid). Residuals: `SIGKILL` goes to the child pid, not a
  process group, so processes the upstream itself spawned survive and reparent;
  a child in uninterruptible (`D`) sleep does not act on the signal until that
  sleep completes, and teardown blocks with it; a `Server` reaching neither
  `run_with` nor `validate_only` still falls back to `kill_on_drop`'s
  best-effort reaping. Aborting the losing pump can drop a message it had
  already read and truncate a partly written downstream line — accepted, that
  peer just went away.
- **A silent upstream hung startup with no diagnostic** (`682538c`). The drift
  probe read until its response or EOF with no bound — `crates/lagom-proxy/`
  contained no `timeout` at all before this commit (verified:
  `git grep -i timeout 0f5d964 -- crates/lagom-proxy/` matches nothing). Each
  probe phase (`initialize`, `tools/list`) is now bounded at 10s, wrapping the
  whole loop so skipped log lines cannot extend it, and fails with
  `ErrorKind::TimedOut` rather than `UnexpectedEof` so callers can branch on
  the distinction
  (`a_silent_upstream_fails_the_probe_loudly_instead_of_hanging`). Not
  configurable: no timeout field is plumbed through `ResolvedPolicy`.

### Added

- **`lagom_proxy::validate_only` + `lagom_proxy::Teardown`** (`c9caf3f`), with
  `lagom validate` migrated onto them (`ae34912`). Validation used to return a
  live `Server` the caller had to remember to drop, and teardown was reported
  on stderr only. `validate_only` hands back the drift verdict in the `Result`
  and the teardown outcome in the `Ok` payload — `Exited`, `Killed` (upstream
  ignored MCP's stdin-close shutdown), or `Unreaped(io::Error)` (pid may still
  be running or zombied) — so `Ok` alone does not mean "cleaned up";
  `Teardown` is `#[must_use]`. `lagom validate` branches on all three, reports
  the two unclean arms on stderr, and keeps the exit code the drift verdict
  (`validate_only_accepts_a_matching_upstream`,
  `validate_only_reports_drift_instead_of_succeeding`,
  `validate_only_reaps_an_upstream_that_ignores_stdin_eof`,
  `validate_only_distinguishes_a_clean_exit_from_a_kill`). Residuals: a caller
  reading only the exit status or stdout sees "validated" while an `Unreaped`
  pid may still be running; the drift and probe-failure arms return from inside
  `spawn_and_validate` and report no `Teardown` at all; `Server::run_with`
  deliberately does not return the outcome, so on the serve path a teardown
  failure is observable on stderr only.
- **A pre-flight failure now answers `initialize` with an id-echoed JSON-RPC
  error** (`ae34912`; code `-32002`, distinct from the `-32001` a rejected
  `tools/call` carries). Previously such a failure wrote stderr and exited,
  closing stdout without a JSON-RPC byte, so five distinct causes —
  upstream-command parse / `current_dir`; config discovery/IO/parse/lower and
  overlay merge; audit-log open and mint-record write; an unreadable mint
  record; and `spawn_and_validate`'s spawn, probe and drift arms — all reached
  the harness as the same opaque "connection closed". `serve` and `refire` are
  split at a pre-flight/bridge seam (`Preflighted`) so the reply is written
  only while nothing else owns stdout; the message is the same `CliError`
  rendering stderr gets. Enforced: exactly one JSON-RPC error line, carrying
  the harness's id and no `result` key
  (`preflight_failure_answers_initialize_with_one_jsonrpc_error`,
  `preflight_reply_is_confined_to_the_pre_bridge_arm`). Silent by design,
  stdout byte-empty, for a harness that writes nothing within a bounded 2s
  read (`preflight_failure_waits_the_bounded_read_then_exits_silently`) and
  for a notification (absent/null `id`, JSON-RPC 2.0 §4.1 —
  `preflight_failure_does_not_answer_a_notification`); a non-JSON line, a
  non-object line, and one without `method` return silently too but are read
  off the helper's body and have no test. Failures *after* the bridge starts
  are not answered: the bridge owns stdout and may already have answered that
  id. How a given harness renders the error is harness-specific and not
  measured here. The funnel tripwire
  (`every_serving_surface_goes_through_one_call_site`) counts proxy-entrypoint
  names in `app.rs` source text, so it is a drift detector, not a proof of
  exclusivity: aliasing (`use lagom_proxy::spawn_and_validate as x`) or a
  renamed re-export evades it, and a count of 1 survives the call moving to
  another surface in the same file.
- **`lagom_config::search_paths`** (`7d64e37`) — the candidate config paths
  `discover` inspects, *including* ones that do not exist, highest precedence
  first and de-duplicated. An empty `discover` result is silent about where
  lagom looked, which makes "unconfigured on purpose" and "my config never
  landed on a searched path" indistinguishable; the ungated diagnostic reports
  this list (`search_paths_lists_absent_candidates_in_precedence_order`,
  `search_paths_does_not_repeat_cwd_as_project_root`).
- **Cross-face lock on rename-through-merge, plus a stale-blob CI gate**
  (`e6c2976`). A parity fixture case and `go/lagom_test.go`'s
  `TestMergeAdmitsOverlayRename` pin the `88cb040` semantics on the
  go-via-wasm face; the `wasm-go` job now runs the Go suite against the
  **committed** blob *before* `just wasm` overwrites it, so a blob predating a
  core change fails there. Enforced only for behavior the Go tests and parity
  fixtures cover — byte identity of the committed blob is deliberately not
  gated, because the release build's bytes depend on the building machine's
  `CARGO_HOME` and on the toolchain hash; `err` parity cases compare
  classification only.

## [0.1.0] — 2026-07-06

First release. One transport-less Rust core (`lagom-core`) behind four faces at
byte-identical capability parity (guarded by `parity/` in CI):

- **Engine**: `Policy` model; `project` / `rewrite` / `merge` (narrow-only —
  the sandbox) / `validate` (drift = refuse to serve); deterministic `mint` /
  `refire` with full provenance (`MintRecord`); the brandable one-call `Guard`.
- **CLI** (`lagom`): `serve` (stdio proxy over a spawned upstream, `--audit`
  JSONL trace), `validate`, `refire`, `emit`, `emit-skills`.
- **Bindings**: Python (`hylla-lagom` on PyPI, `import lagom`), Node/TS
  (`@hylla-io/lagom`, napi-rs, 5 platforms), Go
  (`github.com/hylla-io/lagom/go`, wasm+wazero, no cgo), Rust (the crates
  themselves).
- **Security**: `tools/call` rewritten or rejected before the upstream;
  monotonic narrowing enforced at merge; JSON-RPC batches rejected;
  unparseable downstream lines rejected (never forwarded unparsed); README
  documents the full security model and non-goals; `SECURITY.md` +
  `cargo deny` + `govulncheck` + Dependabot 72h-cooldown supply-chain guards.
- **Tests**: unit + property-style tables across the core; process-level CLI
  integration tests; real-foreign-upstream e2e (`just e2e`); cross-binding
  parity suite; Go mcp-go consumption examples; real-measurement bench
  (`bench/`).
