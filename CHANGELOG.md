# Changelog

All notable changes to lagom. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions follow SemVer
(pre-1.0: breaking changes may land in minor versions).

## [Unreleased]

## [0.1.2] — 2026-09-09

Security release: lagom's own JSON shapes — the programmatic policy face and the
persisted mint record — accepted keys their models do not define and silently
discarded them, so a policy could parse `Ok` while meaning less than its author
wrote, and a record could refire the right executable with none of its
arguments. No API changes; one behavioral tightening (unknown keys are now a
deserialization error) that can turn a previously-`Ok` mint or refire into a
typed failure — see *Upgrade impact* below.

### Security

- **An unknown key in a policy deserialized `Ok` and was dropped, silently
  widening the projection** (`crates/lagom-core/src/policy.rs`). `Policy`,
  `ToolPolicy` and `Constraint` now carry `#[serde(deny_unknown_fields)]`, so a
  key outside the model is a deserialization error instead of a discard. Three
  concrete losses this closes, each of which produced a policy that reads
  correct under audit:
  - `{"default-presence":"drop"}` — lagom's **own canonical spelling on the
    `lagom.toml` face** — deserialized into an empty `Policy`, whose
    `default_presence` then defaulted back to `Keep` (`SPEC.md` §3
    passthrough). A **sealed sandbox was downgraded to passthrough** by a
    copy-paste between two of lagom's own faces, with no diagnostic.
  - A typo'd `arg` (singular) inside a tool rule deserialized with `args: {}`,
    so the author's pin vanished and the agent kept setting that argument
    itself. A misspelled `presence` fell through to `default_presence` the same
    way — dropping a tool the author meant to keep, or keeping one they meant
    to drop.
  - `{"range":{"min":1,"maximum":5}}` was a **partial** parse:
    `Range { min: Some(1.0), max: None }`. Half the author's bound was
    enforced and the resulting call looked legitimate.

  **The asymmetry is what made this dangerous.** The `lagom.toml` face was
  already guarded — `lagom-config`'s `toml_model.rs:42,56,76` has carried
  `deny_unknown_fields` since before this release — so a typo in a config file
  failed loud, while the identical typo on the JSON/programmatic mint face
  (bindings, `dynamic_inputs`, a persisted policy) passed. Operators
  reasonably generalized the guarded face's behavior to the unguarded one.

  Locked by ten tests in `policy.rs`: five refusals
  (`default_presence_kebab_key_is_rejected`, `unknown_policy_key_is_rejected`,
  `tool_policy_typoed_args_key_is_rejected`,
  `tool_policy_typoed_presence_key_is_rejected`,
  `constraint_range_typoed_maximum_is_rejected`), the pre-existing serde
  behavior they lean on (`constraint_unknown_variant_tag_is_rejected` — an
  unknown *variant tag* is refused by external tagging, not by this attribute),
  and four locks that the legitimate surface still parses
  (`snake_case_default_presence_still_parses`,
  `empty_document_is_still_passthrough`,
  `tool_policy_full_legitimate_surface_still_parses`,
  `constraint_range_with_both_bounds_still_parses`). One further test in
  `crates/lagom-proxy/src/lib.rs`
  (`mint_typoed_dynamic_overlay_key_is_rejected_not_silently_widened`) proves
  the refusal reaches the mint path: a typo'd key in `dynamic_inputs` now fails
  the mint as `ConfigError::Lower` sourced at `<dynamic_inputs>`, rather than
  producing a non-narrowing overlay.

  **Scope, stated rather than implied.** The guard covers the `Policy` /
  `ToolPolicy` / `Constraint` shapes. `ToolDef` and `ToolCall` (`tooldef.rs`)
  stay lenient **by design** — real MCP `tools/list` entries carry extra keys
  (`annotations`, `title`, `outputSchema`) and refusing them would break
  conformant upstreams. The boundary is **our own persisted shapes** versus **an
  upstream wire shape we do not control**; `mint.rs` is on the first side and is
  covered by the next entry.

- **A typo'd key in a persisted `MintRecord` launched the upstream stripped of
  its arguments and environment** (`crates/lagom-core/src/mint.rs`).
  `UpstreamCommand`, `PolicySources`, `ResolvedPolicy` and `MintRecord` now carry
  `#[serde(deny_unknown_fields)]`.

  **This one changes what actually runs, not merely what is recorded.**
  `UpstreamCommand.args` and `.env` carry `#[serde(default)]`, so a record
  containing `"argz":["MUST_SURVIVE"]` deserialized `Ok` as
  `{"command":"printf","args":[],"env":[]}`. The path is a real launch path, not
  a hypothetical: `lagom refire` parses the persisted record
  (`crates/lagom-cli/src/app.rs:980`), `refire()` clones `record.resolved`
  (`mint.rs:147`), and the proxy spawns it verbatim —
  `cmd.args(&resolved.upstream.args)` and `cmd.env(k, v)`
  (`crates/lagom-proxy/src/bridge.rs:1096-1103`). So a single misspelling ran the
  **correct executable with none of the arguments or environment it requires**,
  and the refire reported success.

  `PolicySources` had the same shape on `config_paths`: the singular
  `config_path` parsed `Ok` with the list empty, recording a mint composed from
  no config files at all. `PolicySources.dynamic_inputs` stays a free-form
  `Value` by design — the guard bounds the record's keys, not the captured
  overlay.

  Locked by eight tests in `mint.rs`: six refusals
  (`upstream_command_typoed_args_key_is_rejected`,
  `upstream_command_typoed_env_key_is_rejected`,
  `policy_sources_typoed_config_paths_key_is_rejected`,
  `resolved_policy_stray_key_is_rejected`, `mint_record_stray_key_is_rejected`,
  and the end-to-end `mint_record_with_typoed_args_is_refused_not_refired_empty`,
  which asserts a whole record with a typo'd `args` is refused at
  deserialization rather than refired with empty args), plus two locks that the
  legitimate surface still parses (`full_legitimate_record_still_parses`,
  `upstream_command_omitted_defaults_still_parse` — the guard refuses unknown
  keys, it does not make `args`/`env` mandatory).

  **No record written by 0.1.0 or 0.1.1 becomes unreadable.** These four structs
  are byte-identical across `v0.1.0`, `v0.1.1` and this release, and none of
  their fields uses `skip_serializing_if`, so a persisted record carries exactly
  the keys they still declare. The guard can only reject a key no released
  version ever wrote.

### Testing / CI

- **Six unknown-key parity fixtures** (`parity/fixtures/cases.json`, the
  `UNKNOWN-KEY BLOCK`), bringing the shared matrix to 24 cases:
  `project_kebab_default_presence_rejects`, `project_typoed_args_key_rejects`,
  `project_typoed_presence_key_rejects`,
  `rewrite_range_typoed_maximum_rejects`, plus the mint-shape pair
  `refire_record_typoed_args_key_rejects` and its discrimination control
  `refire_record_inline_ok`.

  **Why they matter beyond covering the fix.** The matrix previously reported
  `PARITY OK … zero drift` **across a real Rust-vs-Go divergence**, because no
  fixture exercised an unknown key: the Rust reference denied them while the
  committed wasm blob, built before the change, ignored them — and nothing
  asked. These make the fixture set double as the **wasm-blob staleness
  detector** the matrix lacked. Against a stale
  `go/internal/wasmbin/lagom.wasm` exactly these drift (`rust=err`, `go=ok`)
  while the rest stay clean.

  The first four bound `Policy` only. `refire_record_typoed_args_key_rejects`
  bounds the **persisted mint shapes** (`MintRecord` → `PolicySources` →
  `UpstreamCommand`), which no other case reaches, so that guard is no longer
  invisible to the matrix. A `refire` case may now carry an inline `record` —
  the on-disk shape `refire --record` reads back — instead of `mint_of`;
  `upstream_command` cannot serve here, because the Python and Node runners
  destructure it into `command`/`args`/`env` and never hand the object whole to
  the engine, so a typo'd key there would be invisible to two faces and report
  a false drift. `refire` itself takes a whole record JSON string on every
  face, so the guard is compared rather than bypassed.

  **Measured discrimination.** Removing the single `deny_unknown_fields` on
  `UpstreamCommand` and rebuilding the Rust reference flips exactly one case —
  `refire_record_typoed_args_key_rejects` — from `err` to `ok`, with payload
  `…"upstream":{"command":"srv","args":[],"env":[]}`: the correct executable
  launched stripped of `--sealed`. Every other case, including the
  byte-identical control `refire_record_inline_ok`, is unchanged. So the case
  detects that guard's absence and nothing else.

  Each case is shaped so the *stale* engine's operation actually **succeeds**.
  That shaping is load-bearing: `parity/runners/compare/main.rs` compares only
  classification for `err`, so an unknown key that merely produced a
  *different* error would be invisible.

  `parity/runners/rust/main.rs` gained a fallible `policy_from` for
  policy-shaped fixture inputs. Every other face hands its policy across a JSON
  boundary (wasm bytes, PyO3 string, napi string), so a refusal reaches its
  runner as an ordinary error return and classifies `err`; the reference runner
  previously panicked on a deserialization failure, which would have aborted
  the run instead of letting the comparator see the drift it exists to catch.

### Fixed

- **`go-tag` in the release workflow is now idempotent**
  (`.github/workflows/release.yml`). The step was a bare `git tag` + `git push`,
  which fails the whole release when the `go/vX.Y.Z` tag already exists on the
  remote. That is not hypothetical: the **`v0.1.1` release run failed there**
  ([run 30519411394](https://github.com/hylla-io/lagom/actions/runs/30519411394),
  `! [rejected] go/v0.1.1 -> go/v0.1.1 (already exists)`) after every other job
  in that run — including `python-publish` and `npm-publish` — had succeeded,
  because `go/v0.1.1` had already been published at the same release commit.
  The step now queries remote state first: an existing tag on **this** commit
  is the desired end state and is skipped; an existing tag on a **different**
  commit fails loud and is never force-pushed, because the Go module proxy
  caches a published version immutably. Both `refs/tags/<tag>` and its `^{}`
  peel are queried so an **annotated** tag compares correctly against the
  commit SHA — which is what `go/v0.1.1` is.

- **`go/internal/wasmbin/lagom.wasm` regenerated** so the Go face carries the
  `deny_unknown_fields` engine. Required for the parity matrix to pass; see the
  staleness note above for why the previous blob's divergence went unreported.
  The blob is built from `lagom-core`, so **any** change to that crate needs
  `just wasm` before the matrix says anything about the Go face — the fixture
  set detects staleness only where a case exercises the changed behavior. The
  `mint.rs` shapes are now covered by
  `refire_record_typoed_args_key_rejects`; other behavior may still be
  uncovered, so the rule stands.

### Upgrade impact

A payload that previously parsed `Ok` while carrying an unrecognised key now
fails with a typed deserialization error. That is the point of the release: any
such payload was already not doing what its author wrote. Callers that mint
from a hand-written JSON policy, or that persist and re-read a policy, should
expect a refusal where they previously got a silently-widened projection.
Callers of `lagom refire --record` should expect a refusal on a hand-edited
record where they previously got a child process launched without its
arguments. Records written by 0.1.0 or 0.1.1 are unaffected — those shapes never
carried a key the current models do not declare.

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

### Dependencies

Eight Dependabot bumps that landed on `main` before this release and are
included in it: `wazero` 1.9.0 → 1.12.0 (`/go`, the wasm runtime the Go face
executes on), `mcp-go` (`/go/examples`), `napi` 3.9.2 → 3.10.2 and
`napi-derive` 3.5.6 → 3.5.9 and `@napi-rs/cli` (`/crates/lagom-node`), and
`toml` 0.8.23 → 1.1.2+spec-1.1.0 in the workspace plus `/crates/lagom-py` and
`/crates/lagom-node`. All eight were authored 2026-07-07, so each satisfies the
72-hour supply-chain cooldown in `CLAUDE.md` by a wide margin — the basis for
that statement is the commit dates, not a per-crate publish-date lookup. The
`toml` change crosses a major version; it is a config/manifest-parsing
dependency and the full gate (`just ci`, `go-test`, `parity`, `node-test`) is
green on the integrated tree. `crates/lagom-node/index.js` was regenerated
under the bumped napi CLI so its binding-version checks match this release.

**Consumer impact — the Go module's language floor rises in a patch release.**
`go/go.mod` requires `go 1.25.0`, where `go/v0.1.0` required `go 1.24`; the
wazero 1.12.0 bump carries it, and `golang.org/x/sys v0.44.0` enters as a new
indirect dependency through the same bump. A consumer pinned to Go 1.24 can
therefore build `go/v0.1.0` but not `go/v0.1.1`. This is unusual for a patch
version and is called out rather than buried: the floor came from the
Dependabot commits already on `main`, not from this release's own changes.
The 72-hour basis above is the Dependabot commit dates; `golang.org/x/sys`
arrived transitively and its own publish date was not independently checked.

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
