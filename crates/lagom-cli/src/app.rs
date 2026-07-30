//! The clap command surface and dispatch for the `lagom` binary.
//!
//! Each subcommand maps to a method that returns an [`ExitCode`] so drift can
//! exit non-zero (`SPEC.md` §5.3) without panicking. The harness-snippet
//! rendering lives in [`crate::emit`] so it is unit-testable without spawning a
//! process; the shipped skill bytes are sourced from `lagom_proxy::SHIPPED_SKILLS`
//! and written by `cmd_emit_skills`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use lagom_audit::{AuditEvent, AuditLog};
use lagom_proxy::{MintRecord, PolicySources, ResolvedPolicy, UpstreamCommand};

use crate::emit::{harness_snippet, parse_upstream};

/// Top-level error for the CLI: anything a subcommand can fail with before it
/// decides on an exit code.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// A config/proxy operation failed (load, merge, drift, spawn).
    #[error(transparent)]
    Proxy(#[from] lagom_proxy::ProxyError),
    /// Loading a config layer failed.
    #[error(transparent)]
    Config(#[from] lagom_config::ConfigError),
    /// Opening or writing the audit log failed (`SPEC.md` §9.3).
    #[error(transparent)]
    Audit(#[from] lagom_audit::AuditError),
    /// The upstream launch command was missing or malformed.
    #[error("{0}")]
    Upstream(String),
    /// Writing emitted output to disk failed.
    #[error("writing `{path}`: {source}")]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Serializing the harness snippet to JSON failed.
    #[error("rendering snippet: {0}")]
    Json(#[from] serde_json::Error),
    /// A persisted [`MintRecord`] could not be read or parsed (`SPEC.md` §8.2).
    #[error("reading mint record `{path}`: {message}")]
    Record {
        /// The record path involved.
        path: PathBuf,
        /// What went wrong (I/O or JSON parse).
        message: String,
    },
}

/// lagom — serve narrowed projections of an upstream MCP server.
#[derive(Debug, Parser)]
#[command(name = "lagom", version, about)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    command: Command,
}

/// The lagom subcommands (`SPEC.md` §7.1, §12).
#[derive(Debug, Subcommand)]
enum Command {
    /// Run the stdio proxy: spawn the upstream child and serve the projection.
    ///
    /// The upstream launch command is given as trailing args after `--`:
    /// `lagom serve --config lagom.toml -- npx -y @some/server`.
    Serve {
        /// Explicit config path; otherwise discovered by precedence
        /// (`SPEC.md` §6.3).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Append an audit log (JSONL) at this path, recording the original
        /// upstream defs, the resolved policy, and every rewrite/rejection under
        /// `--run-id` (`SPEC.md` §8.2, §9.3). Without it no audit log is written
        /// and behavior is unchanged. The parent directory must already exist.
        #[arg(long, value_name = "PATH")]
        audit: Option<PathBuf>,
        /// The run id every audit record is tagged with, linking a trace to a
        /// single run for refire (`SPEC.md` §8.2). Only meaningful with
        /// `--audit`; defaults to `run`.
        #[arg(long, value_name = "ID", default_value = "run")]
        run_id: String,
        /// The upstream MCP server launch command and its arguments.
        #[arg(last = true, required = true, value_name = "UPSTREAM")]
        upstream: Vec<String>,
    },
    /// Print the harness stdio-server snippet to paste into a harness config.
    ///
    /// lagom never edits `.mcp.json` / `settings.json` (`SPEC.md` §6.5); this
    /// prints the entry for you to paste. The upstream launch command is given
    /// after `--` so the emitted entry is self-contained.
    Emit {
        /// Config path the emitted `lagom serve` entry should reference.
        #[arg(long)]
        config: Option<PathBuf>,
        /// The downstream server key in the snippet (e.g. `hylla`).
        #[arg(long, default_value = "lagom")]
        name: String,
        /// The upstream MCP server launch command and its arguments.
        #[arg(last = true, required = true, value_name = "UPSTREAM")]
        upstream: Vec<String>,
    },
    /// Re-mint an identical server from a persisted mint record (`SPEC.md` §8.2).
    ///
    /// Reads the [`MintRecord`] JSON at `--record` (as written by `lagom serve
    /// --audit`, whose trace's first line is the `mint` event, or by any binding's
    /// `mint`), and re-mints the **recorded resolved policy** — the exact
    /// projection the original run had, even if the on-disk config has since
    /// changed. The upstream launch command is taken from the record; trailing
    /// `--` args may override it (e.g. a relocated binary). Like `serve`, it then
    /// runs the stdio proxy until the session ends, optionally appending its own
    /// audit trace.
    Refire {
        /// Path to the persisted mint record (JSON). When the file is a JSONL
        /// audit trace, its first `mint` event is used.
        #[arg(long, value_name = "PATH")]
        record: PathBuf,
        /// Append an audit log (JSONL) at this path for the refired run, mirroring
        /// `serve --audit` (`SPEC.md` §8.2, §9.3).
        #[arg(long, value_name = "PATH")]
        audit: Option<PathBuf>,
        /// Override the run id the refired run's audit records are tagged with;
        /// defaults to the record's own `run_id`.
        #[arg(long, value_name = "ID")]
        run_id: Option<String>,
        /// Optional upstream launch command override (after `--`). When omitted,
        /// the upstream recorded in the mint record is used.
        #[arg(last = true, value_name = "UPSTREAM")]
        upstream: Vec<String>,
    },
    /// Validate a policy against the live upstream surface; non-zero on drift.
    Validate {
        /// Explicit config path to validate.
        #[arg(long)]
        config: Option<PathBuf>,
        /// The upstream MCP server launch command and its arguments.
        #[arg(last = true, required = true, value_name = "UPSTREAM")]
        upstream: Vec<String>,
    },
    /// Emit the shipped skill documents into a directory.
    EmitSkills {
        /// Destination directory for the skill markdown files. Defaults to the
        /// current directory.
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
}

impl Cli {
    /// Dispatch the parsed command, returning the process exit code.
    ///
    /// The two SERVING arms are composed rather than called: `cmd_serve`/`cmd_refire`
    /// stop at the pre-flight/bridge seam and yield a [`Preflighted`], and
    /// [`bridge_or_correlate`] either bridges it or answers the harness's
    /// `initialize` with the pre-flight cause (LGM-6). The other three arms yield an
    /// [`ExitCode`] and so cannot reach that reply path — `Result<ExitCode, _>` is
    /// not `Result<Preflighted, _>` (lock class: the type system, these arms).
    pub async fn run(self) -> Result<ExitCode, CliError> {
        match self.command {
            Command::Serve {
                config,
                audit,
                run_id,
                upstream,
            } => bridge_or_correlate(cmd_serve(config, audit, run_id, upstream).await).await,
            Command::Refire {
                record,
                audit,
                run_id,
                upstream,
            } => bridge_or_correlate(cmd_refire(record, audit, run_id, upstream).await).await,
            Command::Emit {
                config,
                name,
                upstream,
            } => cmd_emit(config, &name, upstream),
            Command::Validate { config, upstream } => cmd_validate(config, upstream).await,
            Command::EmitSkills { dir } => cmd_emit_skills(&dir),
        }
    }
}

/// Discover the layered config paths (lowest-precedence base first), pairing them
/// with the supplied upstream launch command as [`PolicySources`] — the
/// provenance a mint records for refire (`SPEC.md` §8.2). Returns the discovery
/// `cwd` alongside them.
///
/// `lagom_config::discover` returns paths highest-precedence first, while
/// [`PolicySources::config_paths`] and the proxy's [`lagom_proxy::mint`] expect
/// base-first (lowest precedence first), so the discovered order is reversed and
/// the explicit `--config` (highest precedence) appended last.
///
/// The `cwd` rides along because [`warn_if_ungated`] needs the *same* directory
/// discovery used to name the candidate paths and to absolutize relative ones; a
/// second `current_dir()` call would be a second fallible read of state that can
/// change under the process.
fn sources(
    config: Option<PathBuf>,
    upstream: Vec<String>,
) -> Result<(PolicySources, PathBuf), CliError> {
    let upstream = parse_upstream(upstream).map_err(CliError::Upstream)?;
    let cwd = std::env::current_dir().map_err(|source| CliError::Io {
        path: PathBuf::from("."),
        source,
    })?;

    // Lowest-precedence first: reverse discovery (highest-first), then append the
    // explicit `--config` as the highest-precedence overlay (de-duplicated).
    let mut discovered = lagom_config::discover(&cwd);
    discovered.reverse();
    if let Some(explicit) = config.as_deref() {
        discovered.retain(|p| p != explicit);
        discovered.push(explicit.to_path_buf());
    }
    let config_paths: Vec<String> = discovered
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    Ok((
        PolicySources {
            config_paths,
            upstream,
            dynamic_inputs: serde_json::Value::Null,
        },
        cwd,
    ))
}

/// Where the resolved policy came from. The *remedy* half of the ungated
/// diagnostic depends on it, so the two serving surfaces cannot share one text.
///
/// `serve`/`validate` resolve from discovered config layers, so editing a
/// `lagom.toml` changes the next run. `refire` replays the policy recorded in a
/// mint record verbatim (`SPEC.md` §8.2) and reads no config at all — telling that
/// operator to "create a lagom.toml" would name a file the run never opens.
enum PolicyOrigin<'a> {
    /// Config layers loaded for this run, lowest precedence first; empty when
    /// discovery found nothing.
    Discovered(&'a [String]),
    /// The persisted mint record `refire` replayed.
    Record(&'a Path),
}

/// Warn on stderr when `resolved` restricts nothing, naming the cause and the
/// remedy that fits `origin`.
///
/// The predicate is **semantic** ([`policy_restricts`]), not equality with
/// `Policy::default()`. Structural equality is necessary but not sufficient: a
/// `ToolPolicy` carrying only an empty table, `presence = "keep"`, `rename`, a
/// `description` override, or an arg `default` is structurally non-default and
/// still leaves every tool callable with every argument — `presence: None` falls
/// through to `default_presence = Keep` and an absent transform touches no schema.
/// A structural test is therefore silent on exactly the shapes where an operator
/// sees a `[tools.…]` section and concludes they are gated.
///
/// The condition is also NOT equivalent to "discovery found nothing": a 0-byte
/// `lagom.toml` *is* discovered (`discover` filters on existence, so `loaded` is
/// non-empty) yet lowers to the zero-config passthrough — `lagom-config`'s
/// `empty_document_is_passthrough` asserts `lower("")` equals
/// `Policy::passthrough()`. A comment-only file parses to the same empty table but
/// has no test of its own. `ungated_warning_separates_a_vacuous_config_from_a_missing_one`
/// (below) locks the rendering half: a discovered-but-vacuous config must not be
/// reported as undiscovered.
///
/// stderr ONLY: `serve`/`refire` own stdout as their JSON-RPC channel, so any byte
/// written there corrupts the protocol. Never an error and never a refusal to
/// start — passthrough-when-unconfigured is specified behavior (`SPEC.md` §3); this
/// is about telling the truth, not about changing the semantic.
fn warn_if_ungated(resolved: &ResolvedPolicy, origin: &PolicyOrigin<'_>, cwd: &Path) {
    let serialized = match serde_json::to_value(&resolved.policy) {
        Ok(value) => value,
        Err(error) => {
            // Defensive: `Policy` is a plain struct of string-keyed maps and
            // derived enums, so no shipped path can fail here. If a future shape
            // does, say the check did not run — inferring either verdict from a
            // value we never inspected would be a claim we cannot back.
            eprintln!(
                "lagom: WARNING could not inspect the resolved policy ({error}); the \
                 ungated check did not run"
            );
            return;
        }
    };
    if policy_restricts(&serialized) {
        return;
    }
    // Structural equality still picks the *cause*: "no rule at all" and "rules
    // that restrict nothing" read very differently to an operator.
    let no_rules = resolved.policy == Default::default();
    eprintln!("{}", ungated_warning(origin, no_rules, cwd));
}

/// Top-level keys of a serialized `lagom_core::Policy`. An unrecognised key means
/// this build is reading a policy shape it does not understand.
const POLICY_KEYS: [&str; 2] = ["default_presence", "tools"];

/// Keys of a serialized `lagom_core::ToolPolicy`, same rule as [`POLICY_KEYS`].
const TOOL_KEYS: [&str; 4] = ["presence", "rename", "description", "args"];

/// Does this policy **restrict** anything — i.e. remove authority the agent would
/// otherwise have?
///
/// The definition, per `SPEC.md` §3/§4.1:
/// - `default_presence = "drop"` → unlisted tools vanish. Restricts.
/// - a tool's `presence = "drop"` → that tool vanishes. Restricts.
/// - an arg `pin` → the arg leaves the schema, lagom fixes the value. Restricts.
/// - an arg `constrain` (enum / range / pattern) → narrower domain. Restricts.
/// - `rename` → a different downstream *name* for the same authority; every tool
///   stays callable with every argument. Does NOT restrict.
/// - `description` (Tier-1 override, §4.2) → fewer tokens, identical authority.
///   Does NOT restrict. Deliberate, and the load-bearing judgement here: an
///   operator whose whole config is slim prose has gated nothing, which is exactly
///   what they must be told.
/// - an arg `default` → supplies a value the agent OMITTED; the arg stays visible
///   and any value the agent does send still passes. Does NOT restrict.
/// - `presence = "keep"`, arg `passthrough`, an empty `[tools.<name>]` table →
///   no-ops.
///
/// Reads the SERIALIZED policy because `lagom-core`'s `Presence`/`ArgPolicy` are
/// not nameable from this crate (`lagom-cli` depends on `lagom-proxy`, which
/// re-exports only the mint types) and this unit may not touch a manifest.
/// Consequence: the serde representation is load-bearing. The `serve`-level tests
/// in `tests/cli.rs` drive real TOML through real minting, so renaming a serde tag
/// breaks them loudly instead of degrading this check quietly.
///
/// Any shape this build does not recognise — unknown key, unknown presence string,
/// unknown arg transform — counts as restricting, so the warning stays silent.
/// Silence claims nothing; printing "nothing is gated" about a policy we could not
/// read would be a false claim. Residual, stated plainly: a *future*
/// non-restricting `ArgPolicy`/`ToolPolicy` variant re-opens the silent-ungated
/// hole for policies that use it until these lists are extended.
fn policy_restricts(policy: &serde_json::Value) -> bool {
    let Some(map) = policy.as_object() else {
        return true;
    };
    if map.keys().any(|k| !POLICY_KEYS.contains(&k.as_str())) {
        return true;
    }
    // `default_presence` carries `#[serde(default = …)]` and no
    // `skip_serializing_if` (`lagom-core`'s `policy.rs:104-106`), so it is present in
    // the serialized form and anything other than an explicit "keep" is either the
    // seal or unrecognised. `missing default_presence` is covered anyway, by
    // `unrecognised_policy_shapes_count_as_restricting`.
    if map.get("default_presence").and_then(|v| v.as_str()) != Some("keep") {
        return true;
    }
    match map.get("tools") {
        None => false,
        Some(serde_json::Value::Object(tools)) => tools.values().any(tool_restricts),
        Some(_) => true,
    }
}

/// Per-tool half of [`policy_restricts`]. Reached only when the policy-level
/// `default_presence` is `keep`, so an explicit `presence = "keep"` is a no-op
/// rather than a re-widening.
fn tool_restricts(tool: &serde_json::Value) -> bool {
    let Some(map) = tool.as_object() else {
        return true;
    };
    if map.keys().any(|k| !TOOL_KEYS.contains(&k.as_str())) {
        return true;
    }
    match map.get("presence") {
        // Absent: inherit `default_presence` (`keep` here).
        None => {}
        Some(value) if value.as_str() == Some("keep") => {}
        // "drop", or a presence string this build does not know.
        Some(_) => return true,
    }
    match map.get("args") {
        None => false,
        Some(serde_json::Value::Object(args)) => args.values().any(arg_restricts),
        Some(_) => true,
    }
}

/// Per-argument half of [`policy_restricts`]: `pin` and `constrain` restrict,
/// `default` and `passthrough` do not, anything else is unrecognised.
fn arg_restricts(arg: &serde_json::Value) -> bool {
    match arg {
        // The unit variant `ArgPolicy::Passthrough`.
        serde_json::Value::String(tag) => tag != "passthrough",
        // Externally-tagged newtype variants: exactly one key.
        serde_json::Value::Object(map) if map.len() == 1 => {
            match map.keys().next().map(String::as_str) {
                Some("pin" | "constrain") => true,
                Some("default") => false,
                _ => true,
            }
        }
        _ => true,
    }
}

/// Dispatch the ungated warning to the renderer matching `origin`; `no_rules`
/// distinguishes an empty/default policy from one whose rules restrict nothing.
fn ungated_warning(origin: &PolicyOrigin<'_>, no_rules: bool, cwd: &Path) -> String {
    match origin {
        PolicyOrigin::Record(path) => record_warning(path, no_rules, cwd),
        PolicyOrigin::Discovered(loaded) if !loaded.is_empty() => {
            discovered_warning(loaded, no_rules, cwd)
        }
        // Nothing loaded ⇒ nothing to name; the searched candidates are the only
        // useful information, and they are computed here (not by the caller) so a
        // gated run never pays for the lookup.
        PolicyOrigin::Discovered(_) => {
            missing_config_warning(&lagom_config::search_paths(cwd), cwd)
        }
    }
}

/// The remedy line shared by both discovered-config renderings.
///
/// It names the keys that actually remove authority. The previous wording ("add a
/// `[tools.<name>]` rule") nudged toward the very shape that gates nothing.
const GATING_REMEDY: &str = "lagom:   to gate the upstream add a rule that restricts: presence = \
     \"drop\" in [tools.<name>], `pin` or a constraint (enum / min / max / pattern) in \
     [tools.<name>.args.<arg>], or a top-level default-presence = \"drop\" allowlist.";

/// Render the warning for a discovered config that gates nothing: `loaded` = the
/// files that were read (lowest precedence first), `cwd` = what relative paths are
/// resolved against.
///
/// Splits on `no_rules` because the two causes are not interchangeable to a
/// reader: an empty/all-comments file versus a file full of rules that happen to
/// restrict nothing. Both list the loaded files — the paths are right, so the
/// searched-candidate list would be noise.
///
/// The "nothing above restricts" line states only what the predicate verified
/// (no drop, no seal, no pin, no constraint) and then names the shapes that are
/// non-restricting; it never claims which of them the file actually contains.
fn discovered_warning(loaded: &[String], no_rules: bool, cwd: &Path) -> String {
    let mut out = String::from(if no_rules {
        "lagom: WARNING lagom.toml discovered but it declares no rule — the resolved policy is \
         PASSTHROUGH: every upstream tool is exposed unchanged and nothing is gated.\n"
    } else {
        "lagom: WARNING lagom.toml discovered, and it declares rules, but none of them restricts \
         anything — every upstream tool is still exposed with every argument agent-settable and \
         nothing is gated.\n"
    });
    out.push_str("lagom:   loaded (lowest precedence first):\n");
    for path in loaded {
        out.push_str(&format!(
            "lagom:     - {}\n",
            absolutized(cwd, Path::new(path)).display()
        ));
    }
    out.push_str(if no_rules {
        "lagom:   every file above is empty, all-comments, or sets only defaults.\n"
    } else {
        "lagom:   nothing above drops a tool, seals default-presence, or pins/constrains an \
         argument; a bare [tools.<name>] table, `presence = \"keep\"`, `rename`, `description`, \
         and an arg `default` all leave the agent's authority unchanged.\n"
    });
    out.push_str(GATING_REMEDY);
    out
}

/// Render the warning for a `refire` whose *recorded* policy gates nothing.
///
/// Kept separate from [`discovered_warning`] because the remedy inverts: refire
/// replays the recorded policy verbatim (`SPEC.md` §8.2), so editing any
/// `lagom.toml` — including a gating one sitting in this very cwd — changes
/// nothing about this run. Naming the record is the only actionable pointer.
fn record_warning(record: &Path, no_rules: bool, cwd: &Path) -> String {
    let mut out = String::from(
        "lagom: WARNING the replayed mint record's resolved policy restricts nothing — every \
         upstream tool is exposed unchanged with every argument agent-settable and nothing is \
         gated.\n",
    );
    out.push_str(&format!(
        "lagom:   record: {}\n",
        absolutized(cwd, record).display()
    ));
    out.push_str(if no_rules {
        "lagom:   the recorded policy is an empty passthrough — it carries no rule at all.\n"
    } else {
        "lagom:   the recorded policy declares rules, but none of them drops a tool, seals \
         default-presence, or pins/constrains an argument.\n"
    });
    out.push_str(
        "lagom:   refire replays the RECORDED policy verbatim (`SPEC.md` §8.2) and reads no \
         config, so editing a lagom.toml cannot change this run — re-mint a gating policy with \
         `lagom serve --config <path> --audit <trace>` and refire that trace.",
    );
    out
}

/// Render the warning for a run where discovery found no config at all: name every
/// candidate that was inspected (`searched`, highest precedence first), because the
/// operator's file may sit somewhere lagom never looks — the one thing the
/// discovered-config rendering cannot tell them.
fn missing_config_warning(searched: &[PathBuf], cwd: &Path) -> String {
    let mut out = String::from(
        "lagom: WARNING no lagom.toml discovered — the resolved policy is PASSTHROUGH: \
         every upstream tool is exposed unchanged and nothing is gated.\n",
    );
    if searched.is_empty() {
        // Defensive: `lagom_config::search_paths` pushes the cwd candidate
        // unconditionally before any layer test (`lagom-config`'s
        // `discover.rs:57-58`), so its result is non-empty and this arm is
        // unreachable from that call path as committed. Kept so a future layer-1
        // change cannot silently render an empty list — this arm's explicit text and
        // the omission of "the paths above" (there are none to point at) are locked
        // by `unconfigured_warning_is_explicit_when_no_candidate_exists`, which calls
        // this renderer with `&[]` and asserts both.
        out.push_str("lagom:   searched: (no candidate paths could be formed)\n");
        out.push_str("lagom:   pass --config <path> to gate the upstream.");
        return out;
    }
    out.push_str("lagom:   searched (highest precedence first):\n");
    for path in searched {
        out.push_str(&format!(
            "lagom:     - {}\n",
            absolutized(cwd, path).display()
        ));
    }
    out.push_str(
        "lagom:   pass --config <path>, or create a lagom.toml at one of the paths above, \
         to gate the upstream.",
    );
    out
}

/// Absolutize `path` against `cwd` for display.
///
/// A candidate can be relative: `HOME=""` (set but empty — routine in stripped
/// container/systemd/CI environments) lowers the user-config layer to
/// `.config/lagom/lagom.toml`, and an explicit `--config lagom.toml` is relative
/// as typed. Existence is probed against the process cwd, so the bare relative
/// form names a location the reader cannot resolve — and the remedy line's "create
/// a lagom.toml at one of the paths above" then points nowhere in particular. The
/// absolutized form is the exact file lagom stat()ed.
fn absolutized(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Render `validate`'s success report, telling the truth about an `ungated`
/// (empty passthrough) policy instead of implying it was checked.
///
/// "grounded against the live upstream" is a claim about policy *references*
/// matching the upstream surface (`SPEC.md` §5.3). An empty passthrough policy
/// references nothing, so that claim is vacuously true and reads as a safety
/// endorsement of a projection that gates nothing — dangerous on the exact
/// surface an operator uses to confirm their gating works. Exit stays 0: an
/// unconfigured lagom is legal, just not gated.
fn validate_report(ungated: bool) -> String {
    if ungated {
        "warning: upstream reached, but the policy is an EMPTY passthrough — no tool dropped, \
         no arg pinned or constrained, nothing is gated. It has no references to ground, so \
         nothing was checked against the upstream."
            .to_string()
    } else {
        "ok: policy is grounded against the live upstream".to_string()
    }
}

/// Resolve a [`ResolvedPolicy`] from the discovered sources (the policy projected
/// against the supplied upstream launch command), warning on stderr if it gates
/// nothing.
///
/// The warning lives after the mint because only the *resolved* policy can answer
/// "does this gate anything" — the sources cannot.
fn resolve(config: Option<PathBuf>, upstream: Vec<String>) -> Result<ResolvedPolicy, CliError> {
    let (sources, cwd) = sources(config, upstream)?;
    let resolved = lagom_proxy::mint(&sources)?;
    warn_if_ungated(
        &resolved,
        &PolicyOrigin::Discovered(&sources.config_paths),
        &cwd,
    );
    Ok(resolved)
}

/// Warn if the resolved policy restricts nothing, then spawn/probe/drift-check the
/// session and hand back the un-bridged handle. Both serving surfaces — `serve` and
/// `refire` — route through here; [`bridge_or_correlate`] performs the bridge step.
///
/// `refire` shipped with no ungated diagnostic because it grew its own copy of the
/// "serve or serve_audited" tail, and it fronts agents exactly as `serve` does.
/// The two steps are split here rather than delegated to a spawn+run wrapper so
/// there is a pre-flight/bridge seam: [`lagom_proxy::spawn_and_validate`] returns
/// once the child is spawned, probed and drift-checked but before any traffic
/// moves, and `Server::run` — the step that bridges — happens at the seam's far
/// side in [`bridge_or_correlate`]. LGM-6 needs that split: a reply on stdout is
/// legitimate only while nothing else owns stdout.
///
/// **What is mechanically enforced** — lock class: a named test,
/// `every_serving_surface_goes_through_one_call_site` (below). It COUNTS proxy
/// entrypoint names in this file's source: `lagom_proxy::serve` and
/// `lagom_proxy::serve_audited` at 0, `lagom_proxy::spawn_and_validate` and
/// `lagom_proxy::validate_only` at 1 each. So adding a call IN THIS FILE to either
/// spawn+run wrapper — the shorthand `refire` skipped the warning with — turns that
/// test red, as does adding a second `spawn_and_validate` call site here.
///
/// **What it does NOT enforce**, enumerated rather than implied:
/// - It is a COUNT over this file's text, not a LOCATION and not an exclusivity
///   lock. Moving the `spawn_and_validate` call out of this function into another
///   surface *in this file* keeps the count at 1 and stays green; nothing here makes
///   that surface call [`warn_if_ungated`]. (Moving it to another file would instead
///   read 0 and fail — the tripwire sees only `app.rs`.)
/// - It is textual, so aliasing (`use lagom_proxy::spawn_and_validate as x`) or a
///   renamed re-export evades it — see the test's own residual note.
/// - `origin` being mandatory is the type system, but that binds only calls already
///   routed through this function.
async fn warn_then_spawn(
    resolved: ResolvedPolicy,
    log: Option<AuditLog>,
    run_id: String,
    origin: PolicyOrigin<'_>,
    cwd: &Path,
) -> Result<Preflighted, CliError> {
    // Before the JSON-RPC session starts, so an operator sees the diagnostic at the
    // top of the session's stderr rather than interleaved with traffic.
    warn_if_ungated(&resolved, &origin, cwd);
    // Together with `bridge_or_correlate` this is what `lagom_proxy::serve_audited`
    // does (`bridge.rs`, `serve_audited`: spawn_and_validate, then
    // `server.run(audit, run_id)`), split at the seam. One difference from the
    // previous un-audited arm: `lagom_proxy::serve` hardcodes `run_id = "run"`, and
    // this passes the caller's instead. Inert with `log == None` — `run_id` reaches
    // only `bridge.rs`'s `record()` calls, and `record` no-ops when the log is
    // `None`, so no wire bytes and no stderr depend on it. With `Some(log)` the
    // run_id was already the caller's.
    let server = lagom_proxy::spawn_and_validate(resolved).await?;
    Ok(Preflighted {
        server,
        log,
        run_id,
    })
}

/// JSON-RPC error code lagom answers a pre-flight failure with.
///
/// Distinct from `bridge.rs`'s `REJECT_ERROR_CODE` (`-32001`) on purpose: a harness
/// log must be able to tell "lagom refused this `tools/call`" from "lagom never
/// started". Both live in `-32000..=-32099`, the band the JSON-RPC 2.0 spec's error
/// object table (§5.1) reserves for implementation-defined server errors.
const PREFLIGHT_ERROR_CODE: i64 = -32002;

/// Budget for the correlation read on stdin after a pre-flight failure — one
/// `read_line` on a detached thread, no loop, as [`reply_preflight_error`]'s body is
/// written. No test pins that read count.
///
/// 2s: slack for a harness that has not yet written `initialize` when lagom fails,
/// not a poll interval — one read, then done. Short because the wait is pure cost
/// when nothing arrives, e.g. for a human running `lagom serve` in a terminal, who
/// would otherwise watch a failed process sit there. Real harness write latency is
/// not measured here, so treat the value as a budget, not a fitted number.
/// CLI-owned rather than reusing a `bridge.rs` timeout: those bound the UPSTREAM's
/// latency, a different quantity with different tuning pressure.
///
/// That the wait is really taken (rather than short-circuited by an EOF the test
/// harness accidentally supplied) is locked by
/// `preflight_failure_waits_the_bounded_read_then_exits_silently` (`tests/cli.rs`),
/// which holds stdin open, writes nothing, and asserts elapsed >= this value.
const PREFLIGHT_STDIN_WAIT: Duration = Duration::from_secs(2);

/// A session that is spawned, probed and drift-checked but has NOT yet taken over
/// stdout, plus the audit wiring its bridge step needs.
///
/// The pre-flight/bridge seam as a value: on the path that builds one, the
/// pre-flight stages listed on [`bridge_or_correlate`] have all returned `Ok`, so
/// there the stdout reply belongs to the `Err` arm and the bridge to the `Ok` arm.
///
/// Scope of that: [`warn_then_spawn`] is the sole constructor as written, and the
/// struct plus every field is private, so no module OUTSIDE this one and its
/// descendants can build one (lock class: the type system, at that visibility
/// boundary). Inside this module and its children — `mod tests` included, since Rust
/// privacy reaches descendants — it is a convention: nothing stops a future function
/// here from assembling one.
struct Preflighted {
    /// The validated upstream session, not yet bridged.
    server: lagom_proxy::Server,
    /// The audit log to record into, when `--audit` was given.
    log: Option<AuditLog>,
    /// The run id every audit record is tagged with (`SPEC.md` §8.2).
    run_id: String,
}

/// Bridge a completed pre-flight, or — on a pre-bridge failure — answer the
/// harness's `initialize` with a JSON-RPC error naming the cause, then fail.
///
/// ## Why (LGM-6)
///
/// A pre-flight failure wrote stderr and exited, closing stdout without a single
/// JSON-RPC byte. Every distinct cause therefore reached the harness as the same
/// opaque `connection closed: initialize response`, and correlating that to lagom's
/// stderr cost a full debugging session. Answering the `initialize` id makes the
/// cause arrive on the channel the harness is already reading.
///
/// ## Which causes are answered — enumerated, not "every failure"
///
/// The MECHANISM is "whatever `Err` reaches this function", so the list below is a
/// reading of the current call graph rather than a filter. As `cmd_serve` and
/// `cmd_refire` are written, that is: upstream-command parse and `current_dir`
/// ([`sources`]); config discovery/IO/parse/lower and overlay merge
/// ([`lagom_proxy::mint`], `lagom_proxy::mint_record`); audit-log open and the
/// mint-record write; [`read_mint_record`]; and, inside [`warn_then_spawn`], the
/// three failing arms of [`lagom_proxy::spawn_and_validate`] as committed — spawn
/// (`spawn_child`), the probe (write failure, EOF, its own timeout, unusable
/// `tools/list`), and drift.
///
/// Consequence, in both directions: a pre-flight cause added to either surface is
/// answered without editing this list, while a cause MOVED to after the bridge stops
/// being answered and would need this doc corrected. The reply's message is
/// `CliError`'s `Display` — the same rendering `main.rs` gives stderr — so the two
/// say the same thing.
///
/// ## What is NOT answered, enumerated rather than implied
///
/// - clap usage/parse errors and `--help`: clap exits from `Cli::parse()` in `main`
///   before any of this runs.
/// - `validate`, `emit`, `emit-skills`: not serving surfaces, so a JSON-RPC frame
///   would be noise. They also cannot reach this function — their arms in
///   [`Cli::run`] yield `Result<ExitCode, _>`, not `Result<Preflighted, _>` (lock
///   class: the type system, as those arms are written).
/// - failures after the bridge starts: `Server::run`'s `Err` returns below WITHOUT a
///   reply, because the bridge owns stdout from its first byte and may already have
///   answered that id — a second frame could duplicate it. As written, the only
///   `run` call site is the `Ok` arm here and it does not call the reply helper;
///   `preflight_reply_is_confined_to_the_pre_bridge_arm` (below) counts that helper
///   at one call site, this function at two, and the bridge step in its
///   `log`-argument form at one. Counts only: the test cannot see WHICH arm holds
///   each.
/// - a harness that writes nothing within [`PREFLIGHT_STDIN_WAIT`], a non-JSON or
///   non-object line, a line without `method`, and a notification (absent or null
///   `id`): stdout stays byte-empty. Two of those four are locked, in `tests/cli.rs`
///   and each asserting `out.stdout.is_empty()`: the silent budget expiry by
///   `preflight_failure_waits_the_bounded_read_then_exits_silently`, and both
///   notification shapes by `preflight_failure_does_not_answer_a_notification`. The
///   non-JSON, non-object and missing-`method` returns have no test of their own —
///   they are read off the reply helper's body.
///
/// ## Scope of the harness-visibility claim
///
/// What is shown is that lagom answers `initialize` with the cause instead of
/// closing stdout silently (the tests above assert the frame's id, code and
/// message). How a given harness RENDERS that error — whether it surfaces the
/// message or reports its own transport wording — is harness-specific and not
/// measured here.
async fn bridge_or_correlate(
    preflighted: Result<Preflighted, CliError>,
) -> Result<ExitCode, CliError> {
    let Preflighted {
        server,
        log,
        run_id,
    } = match preflighted {
        Ok(ready) => ready,
        Err(error) => {
            reply_preflight_error(&error);
            return Err(error);
        }
    };
    server.run(log, run_id).await?;
    Ok(ExitCode::SUCCESS)
}

/// Read at most one line from stdin, bounded by [`PREFLIGHT_STDIN_WAIT`], and if it
/// is a JSON-RPC request — a JSON object carrying a `method` and a non-null `id` —
/// write ONE JSON-RPC error object echoing that id to stdout.
///
/// Returns without writing a byte at FOUR guarded early returns below, covering five
/// causes: the budget expires or the reader thread ended without sending (the
/// `recv_timeout` guard); the line is not valid JSON, or is JSON but not an object
/// (one `let Ok(Value::Object(_)) = … else` guard — also where EOF lands, since
/// `read_line` at EOF yields `Ok(0)` and the thread sends the empty string); it has
/// no `method`; or its `id` is absent/null. Of those, the budget case is locked by
/// `preflight_failure_waits_the_bounded_read_then_exits_silently` and the
/// absent/null-`id` case by `preflight_failure_does_not_answer_a_notification`
/// (`tests/cli.rs`); the non-JSON, non-object and missing-`method` returns are read
/// off the body and have no test of their own. Skipping a notification is JSON-RPC
/// 2.0 §4.1 (a Notification gets no reply) — a stray frame is worse than no frame on
/// a channel the harness is parsing.
///
/// The serialized value is the `error` literal below, so no `result` frame is built
/// here; `preflight_failure_answers_initialize_with_one_jsonrpc_error` asserts both
/// (the frame carries no `result` key, and stdout holds exactly one line).
///
/// A write failure goes to stderr and is otherwise dropped: the pre-flight cause is
/// what the operator needs, and returning/reporting a broken-pipe error in its place
/// would hide it. The caller returns the original `Err` either way.
///
/// Blocks the calling task for up to [`PREFLIGHT_STDIN_WAIT`], deliberately: no other
/// lagom task is in flight here — the bridge's pumps are spawned inside
/// `Server::run`, which has not been called, and the upstream child, if one was
/// spawned, was killed inside `spawn_and_validate` before it returned `Err`
/// (`bridge.rs`, the probe and drift arms).
///
/// The read runs on a DETACHED `std::thread`, not `tokio::task::spawn_blocking`:
/// tokio 1.52.3's `io::stdin` docs (`src/io/stdin.rs:15-22`) state stdin "is
/// implemented by using an ordinary blocking read on a separate thread, and it is
/// impossible to cancel that read. This can make shutdown of the runtime hang until
/// the user presses enter", and recommend exactly this — "spawn a thread dedicated to
/// user input and use blocking IO directly in that thread". An OS thread is ended by
/// process exit instead of joined at runtime shutdown. Cost, stated: that thread can
/// still hold the read when the process exits, so a byte the harness wrote late is
/// consumed and dropped — acceptable, this process is exiting non-zero and serves no
/// traffic.
fn reply_preflight_error(error: &CliError) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok() {
            // Send failure means the receiver already timed out and went away: there
            // is nobody to hand the line to and nothing to repair.
            let _ = tx.send(line);
        }
    });
    let Ok(line) = rx.recv_timeout(PREFLIGHT_STDIN_WAIT) else {
        return;
    };
    let Ok(serde_json::Value::Object(request)) = serde_json::from_str(line.trim()) else {
        return;
    };
    if !request.contains_key("method") {
        return;
    }
    let Some(id) = request.get("id").filter(|id| !id.is_null()) else {
        return;
    };

    let reply = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": PREFLIGHT_ERROR_CODE,
            "message": format!("lagom pre-flight failed, no session started: {error}"),
        }
    });
    // A local one-line writer: `lagom_proxy`'s `write_value`/`write_line` are private
    // to that crate, and `Value`'s `Display` is already the compact one-line form the
    // newline-delimited stdio framing wants (MCP `2025-11-25` transports, the same
    // framing `bridge.rs`'s `write_line` produces).
    let mut out = std::io::stdout().lock();
    if let Err(io) = writeln!(out, "{reply}").and_then(|()| out.flush()) {
        eprintln!(
            "lagom: could not write the pre-flight error reply to stdout ({io}); the cause follows"
        );
    }
}

/// `lagom serve`'s PRE-FLIGHT: discover/merge the policy, open the audit log, spawn
/// the upstream and drift-validate — stopping at the seam, before any traffic moves.
/// [`Cli::run`] hands the result to [`bridge_or_correlate`], which runs the session.
///
/// Returning `Preflighted` instead of an `ExitCode` is what makes the `?`s in this
/// body correlatable: each returns a pre-bridge cause to [`Cli::run`]'s composed
/// serving arm, which answers the harness's `initialize` with it before exiting
/// non-zero (LGM-6).
///
/// With `--audit <path>` this opens an append-only JSONL log and passes it along to
/// `Server::run`; the bridge then records the original upstream defs + resolved
/// policy at session start and every rewrite/rejection thereafter, tagged with
/// `run_id` (`SPEC.md` §8.2, §9.3). Without the flag the same path runs with no log
/// wired in, so nothing is recorded.
async fn cmd_serve(
    config: Option<PathBuf>,
    audit: Option<PathBuf>,
    run_id: String,
    upstream: Vec<String>,
) -> Result<Preflighted, CliError> {
    let (sources, cwd) = sources(config, upstream)?;
    // Mint (and, with `--audit`, open the trace) first, then spawn through
    // `warn_then_spawn`: the ungated diagnostic tests the RESOLVED policy, so it
    // cannot run any earlier.
    let (resolved, log) = match audit {
        None => (lagom_proxy::mint(&sources)?, None),
        Some(path) => {
            // Build the full mint record (resolved policy + provenance) and persist
            // it as the trace head (`SPEC.md` §8.2): the on-disk log then carries
            // both *what the run saw* (OriginalDefs + ResolvedPolicy, recorded by
            // the bridge) and the *provenance* that produced it (config paths +
            // dynamic inputs + upstream), so the run is refirable from its own
            // trace.
            let record = lagom_proxy::mint_record(run_id.clone(), &sources)?;
            let resolved = record.resolved.clone();
            let mut log = AuditLog::open(&path)?;
            log.record(&AuditEvent::Mint { record })?;
            (resolved, Some(log))
        }
    };
    warn_then_spawn(
        resolved,
        log,
        run_id,
        PolicyOrigin::Discovered(&sources.config_paths),
        &cwd,
    )
    .await
}

/// `lagom refire`'s PRE-FLIGHT: re-mint the recorded resolved policy from a
/// persisted [`MintRecord`] and take it up to the seam (`SPEC.md` §8.2);
/// [`Cli::run`] hands the result to [`bridge_or_correlate`], which serves it.
///
/// Reads the record (a bare `MintRecord` JSON object, or a JSONL audit trace
/// whose first `mint` event carries one), refires it (bypassing re-resolution so
/// the exact recorded projection is reproduced even after the config drifted),
/// then spawns/validates the upstream. Trailing `--` args override the recorded
/// upstream launch command; `--audit`/`--run-id` append a fresh trace for the
/// refired run.
///
/// Spawns through [`warn_then_spawn`], so a recorded policy that restricts nothing
/// gets the same stderr diagnostic `serve` gives — this surface fronts agents the
/// same way. Same reason as `cmd_serve` for returning `Preflighted`: an unreadable
/// record or a relocated-but-missing upstream is a pre-bridge cause the harness can
/// be told about.
async fn cmd_refire(
    record_path: PathBuf,
    audit: Option<PathBuf>,
    run_id: Option<String>,
    upstream_override: Vec<String>,
) -> Result<Preflighted, CliError> {
    let record = read_mint_record(&record_path)?;
    let run_id = run_id.unwrap_or_else(|| record.run_id.clone());

    // Refire reproduces the recorded resolved policy byte-for-byte; an explicit
    // trailing upstream command relocates where the same projection's child runs.
    let mut resolved = lagom_proxy::refire(&record);
    if !upstream_override.is_empty() {
        resolved.upstream = parse_upstream(upstream_override).map_err(CliError::Upstream)?;
    }

    let log = match audit {
        None => None,
        Some(path) => {
            // Re-persist the (refired) mint record at the new trace head so the
            // refired run is itself refirable.
            let mut refired_record = record;
            refired_record.run_id = run_id.clone();
            refired_record.resolved = resolved.clone();
            let mut log = AuditLog::open(&path)?;
            log.record(&AuditEvent::Mint {
                record: refired_record,
            })?;
            Some(log)
        }
    };

    // The ungated diagnostic absolutizes a relative `--record` for display, so it
    // needs the directory the path was typed against. Fail loud exactly as
    // [`sources`] does: a process whose cwd cannot be read is already broken for
    // either serving surface.
    let cwd = std::env::current_dir().map_err(|source| CliError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    warn_then_spawn(
        resolved,
        log,
        run_id,
        PolicyOrigin::Record(&record_path),
        &cwd,
    )
    .await
}

/// Read a [`MintRecord`] from disk, accepting either a bare record JSON object or
/// a JSONL audit trace whose first `mint` event carries one.
fn read_mint_record(path: &std::path::Path) -> Result<MintRecord, CliError> {
    let text = std::fs::read_to_string(path).map_err(|source| CliError::Record {
        path: path.to_path_buf(),
        message: source.to_string(),
    })?;
    // Fast path: the whole file is a single MintRecord JSON object.
    if let Ok(record) = serde_json::from_str::<MintRecord>(text.trim()) {
        return Ok(record);
    }
    // Otherwise treat it as a JSONL audit trace and find the first `mint` event.
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(AuditEvent::Mint { record }) = serde_json::from_str::<AuditEvent>(line) {
            return Ok(record);
        }
    }
    Err(CliError::Record {
        path: path.to_path_buf(),
        message: "no mint record found (expected a MintRecord JSON object or a JSONL \
                  audit trace containing a `mint` event)"
            .to_string(),
    })
}

/// `lagom emit`: print the harness stdio-server snippet (`SPEC.md` §6.5).
fn cmd_emit(
    config: Option<PathBuf>,
    name: &str,
    upstream: Vec<String>,
) -> Result<ExitCode, CliError> {
    let upstream: UpstreamCommand = parse_upstream(upstream).map_err(CliError::Upstream)?;
    let snippet = harness_snippet(name, config.as_deref(), &upstream)?;
    println!("{snippet}");
    Ok(ExitCode::SUCCESS)
}

/// `lagom validate`: spawn the upstream, fetch its live `tools/list`, and check
/// the policy's references against it (`SPEC.md` §5.3). Exits non-zero on drift.
///
/// Goes through [`lagom_proxy::validate_only`], whose success type is
/// [`lagom_proxy::Teardown`] — lock class: the type system. So no `Server` reaches
/// this function *from that call*, and `Server`'s fields are private to `bridge`, so
/// one cannot be rebuilt here from parts either. Scope: those two facts cover this
/// call and this type, not the function's future shape — the funnel tripwire counts
/// `spawn_and_validate` at 1 without pinning where it lives, so an edit that moved
/// that call in here would stay green. As committed, the body below calls only
/// `validate_only`.
///
/// `validate_only` also owns the teardown and *reports* it in that `Ok` payload. This
/// function consumes the payload by branching on it (never `let _`) and reports the
/// two unclean arms on stderr, but keeps the exit code the drift verdict. Its rustdoc
/// states what the reap does and does not cover (grandchildren, uninterruptible `D`
/// state, and the drift and probe-failure arms, which return from inside
/// `spawn_and_validate` reporting no `Teardown` at all).
async fn cmd_validate(
    config: Option<PathBuf>,
    upstream: Vec<String>,
) -> Result<ExitCode, CliError> {
    let resolved = resolve(config, upstream)?;
    // Deliberately STRUCTURAL, and a different claim from the stderr diagnostic:
    // this report is about whether there was anything to *ground* against the
    // upstream. `Policy::default()` references no tool and no argument, so drift
    // checking is vacuous. A policy that names tools but restricts nothing (only
    // `rename`/`description`) does have references, and they really are checked —
    // its ungatedness is [`warn_if_ungated`]'s semantic claim, reported separately
    // on stderr.
    let ungated = resolved.policy == Default::default();
    // `validate_only` spawns the child, probes `tools/list`, validates, and tears the
    // child down itself before returning; a drifted policy returns
    // `ProxyError::Drift`. Nothing runnable comes back, so there is no `Server` to
    // remember to discard here.
    match lagom_proxy::validate_only(resolved).await {
        Ok(teardown) => {
            println!("{}", validate_report(ungated));
            // Teardown is a DIFFERENT fact from the drift verdict (`validate_only`'s
            // "two independent outcomes, two channels"), so it lands on stderr and
            // does not move the exit code: this command answers "does the policy
            // still match the upstream", and exiting non-zero for a child that
            // refused to die would report a policy problem validation did not find.
            // stderr, not stdout — stdout is the JSON-RPC channel. `Exited` is the
            // clean arm and says nothing; `reap` already logged each unclean arm's
            // *cause*, these name the caller-visible consequence. Residual, stated
            // rather than implied: a caller reading only the exit status or only
            // stdout sees "validated" while an `Unreaped` pid may still be running —
            // stderr is the only place that distinction surfaces here.
            match teardown {
                lagom_proxy::Teardown::Exited => {}
                lagom_proxy::Teardown::Killed => eprintln!(
                    "lagom: validated, but the upstream ignored MCP's stdin-close \
                     shutdown and had to be SIGKILLed"
                ),
                lagom_proxy::Teardown::Unreaped(source) => eprintln!(
                    "lagom: validated, but the upstream child was NOT reaped \
                     ({source}); its pid may still be running or zombied"
                ),
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(lagom_proxy::ProxyError::Drift(messages)) => {
            eprintln!("drift: policy references no longer match the upstream:");
            for m in &messages {
                eprintln!("  - {m}");
            }
            Ok(ExitCode::FAILURE)
        }
        Err(other) => Err(other.into()),
    }
}

/// `lagom emit-skills`: write the shipped skill markdown into `dir` (`SPEC.md`
/// §12). Does not manage harness discovery — it just drops the files.
fn cmd_emit_skills(dir: &std::path::Path) -> Result<ExitCode, CliError> {
    std::fs::create_dir_all(dir).map_err(|source| CliError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for (filename, body) in lagom_proxy::SHIPPED_SKILLS {
        let path = dir.join(filename);
        std::fs::write(&path, body).map_err(|source| CliError::Io {
            path: path.clone(),
            source,
        })?;
        println!("wrote {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_warning_names_every_searched_path_and_the_consequence() {
        let searched = vec![
            PathBuf::from("/work/lagom.toml"),
            PathBuf::from("/home/u/.config/lagom/lagom.toml"),
        ];
        let msg = missing_config_warning(&searched, Path::new("/work"));
        assert!(msg.contains("no lagom.toml discovered"), "{msg}");
        assert!(msg.contains("PASSTHROUGH"), "{msg}");
        assert!(msg.contains("nothing is gated"), "{msg}");
        for path in &searched {
            assert!(
                msg.contains(&path.display().to_string()),
                "searched path {} must appear: {msg}",
                path.display()
            );
        }
        assert!(msg.contains("--config"), "must state the remedy: {msg}");
    }

    #[test]
    fn unconfigured_warning_is_explicit_when_no_candidate_exists() {
        // No cwd candidate and no HOME/XDG: still say so rather than print an
        // empty list that reads like a rendering bug.
        let msg = missing_config_warning(&[], Path::new("/work"));
        assert!(msg.contains("no candidate paths could be formed"), "{msg}");
        assert!(msg.contains("nothing is gated"), "{msg}");
        // With no candidates there is nothing "above" to create a file at, so the
        // remedy must not point at a list it did not print.
        assert!(!msg.contains("paths above"), "{msg}");
    }

    #[test]
    fn ungated_warning_separates_a_vacuous_config_from_a_missing_one() {
        // A discovered-but-vacuous config is the case a discovery-emptiness gate
        // misses. It must name the files that WERE read and must not tell the
        // operator their config was never found — the remedies are opposites.
        let msg = discovered_warning(&["/work/lagom.toml".to_string()], true, Path::new("/work"));
        assert!(msg.contains("declares no rule"), "{msg}");
        assert!(msg.contains("PASSTHROUGH"), "{msg}");
        assert!(msg.contains("nothing is gated"), "{msg}");
        assert!(msg.contains("/work/lagom.toml"), "{msg}");
        assert!(
            msg.contains("[tools.<name>]"),
            "must state the remedy: {msg}"
        );
        assert!(
            !msg.contains("no lagom.toml discovered"),
            "a discovered file must not be reported as undiscovered: {msg}"
        );
    }

    #[test]
    fn ungated_warning_absolutizes_relative_candidates() {
        // `HOME=""` (set but empty) lowers the user-config layer to the relative
        // `.config/lagom/lagom.toml`; existence is probed against the cwd, so the
        // bare relative form names a place the reader cannot resolve.
        let msg = missing_config_warning(
            &[PathBuf::from(".config/lagom/lagom.toml")],
            Path::new("/work"),
        );
        assert!(
            msg.contains("/work/.config/lagom/lagom.toml"),
            "relative candidate must render as the path actually probed: {msg}"
        );
        assert!(
            !msg.contains("- .config/lagom/lagom.toml"),
            "the unresolvable relative form must not be printed: {msg}"
        );
    }

    #[test]
    fn ungated_warning_absolutizes_a_relative_loaded_path() {
        // `--config lagom.toml` is relative as typed; the diagnostic must still say
        // which file on disk gated nothing.
        let msg = discovered_warning(&["lagom.toml".to_string()], true, Path::new("/work"));
        assert!(msg.contains("/work/lagom.toml"), "{msg}");
    }

    /// The serialized form of a policy whose single tool carries `body`'s keys.
    fn tool_policy(body: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "default_presence": "keep", "tools": { "search": body } })
    }

    #[test]
    fn structurally_nondefault_but_non_restricting_policies_do_not_restrict() {
        // The core defect this unit closes: each of these is `!= Policy::default()`
        // yet leaves every tool callable with every argument, so the structural test
        // that shipped in round 1/2 was silent on all of them.
        let cases: &[(&str, serde_json::Value)] = &[
            ("empty tool table", tool_policy(serde_json::json!({}))),
            (
                "presence keep",
                tool_policy(serde_json::json!({ "presence": "keep" })),
            ),
            (
                "rename only",
                tool_policy(serde_json::json!({ "rename": "s" })),
            ),
            (
                "description override",
                tool_policy(serde_json::json!({ "description": { "override": "slim" } })),
            ),
            (
                "arg default",
                tool_policy(serde_json::json!({ "args": { "limit": { "default": 10 } } })),
            ),
            (
                "arg passthrough",
                tool_policy(serde_json::json!({ "args": { "limit": "passthrough" } })),
            ),
            (
                "all of them at once",
                tool_policy(serde_json::json!({
                    "presence": "keep",
                    "rename": "s",
                    "description": { "override": "slim" },
                    "args": { "limit": { "default": 10 } }
                })),
            ),
            (
                "empty passthrough",
                serde_json::json!({ "default_presence": "keep", "tools": {} }),
            ),
        ];
        for (label, policy) in cases {
            assert!(
                !policy_restricts(policy),
                "{label} restricts nothing: {policy}"
            );
        }
    }

    #[test]
    fn authority_removing_policies_restrict() {
        let cases: &[(&str, serde_json::Value)] = &[
            (
                "sealed default",
                serde_json::json!({ "default_presence": "drop", "tools": {} }),
            ),
            (
                "tool drop",
                tool_policy(serde_json::json!({ "presence": "drop" })),
            ),
            (
                "arg pin",
                tool_policy(serde_json::json!({ "args": { "artifact": { "pin": "hylla" } } })),
            ),
            (
                "arg constrain",
                tool_policy(
                    serde_json::json!({ "args": { "n": { "constrain": { "range": { "max": 5.0 } } } } }),
                ),
            ),
            (
                "one gating arg among non-gating ones",
                tool_policy(serde_json::json!({
                    "rename": "s",
                    "args": { "limit": { "default": 10 }, "artifact": { "pin": "hylla" } }
                })),
            ),
        ];
        for (label, policy) in cases {
            assert!(policy_restricts(policy), "{label} restricts: {policy}");
        }
    }

    #[test]
    fn unrecognised_policy_shapes_count_as_restricting() {
        // A shape this build cannot read must NOT be reported as "nothing is gated";
        // silence claims nothing, a false claim is worse. Locks the conservative
        // direction of the unknown-shape arms.
        let cases: &[(&str, serde_json::Value)] = &[
            (
                "unknown policy key",
                serde_json::json!({ "default_presence": "keep", "tools": {}, "future": 1 }),
            ),
            (
                "missing default_presence",
                serde_json::json!({ "tools": {} }),
            ),
            (
                "unknown presence string",
                tool_policy(serde_json::json!({ "presence": "maybe" })),
            ),
            (
                "unknown tool key",
                tool_policy(serde_json::json!({ "future": true })),
            ),
            (
                "unknown arg transform",
                tool_policy(serde_json::json!({ "args": { "x": { "future": 1 } } })),
            ),
            (
                "unknown arg unit variant",
                tool_policy(serde_json::json!({ "args": { "x": "future" } })),
            ),
            ("not an object", serde_json::json!("passthrough")),
        ];
        for (label, policy) in cases {
            assert!(
                policy_restricts(policy),
                "{label} must be treated as gating: {policy}"
            );
        }
    }

    #[test]
    fn discovered_warning_separates_no_rules_from_non_restricting_rules() {
        let loaded = ["/work/lagom.toml".to_string()];
        let non_restricting = discovered_warning(&loaded, false, Path::new("/work"));
        // The operator DID write rules, so claiming the file declares none would be
        // false; it must say the rules restrict nothing instead.
        assert!(
            non_restricting.contains("restricts anything"),
            "{non_restricting}"
        );
        assert!(
            !non_restricting.contains("declares no rule"),
            "a file with rules must not be reported as ruleless: {non_restricting}"
        );
        assert!(
            non_restricting.contains("nothing is gated"),
            "{non_restricting}"
        );
        assert!(
            non_restricting.contains("/work/lagom.toml"),
            "{non_restricting}"
        );
        // The remedy must name keys that actually restrict, not just "add a rule".
        for key in ["presence = \"drop\"", "pin", "default-presence = \"drop\""] {
            assert!(
                non_restricting.contains(key),
                "remedy must name {key}: {non_restricting}"
            );
        }

        let no_rules = discovered_warning(&loaded, true, Path::new("/work"));
        assert!(no_rules.contains("declares no rule"), "{no_rules}");
        assert!(
            !no_rules.contains("restricts anything"),
            "an empty file must not be described as having rules: {no_rules}"
        );
    }

    #[test]
    fn record_warning_points_at_the_record_not_at_config_discovery() {
        let msg = record_warning(Path::new("mint.json"), true, Path::new("/work"));
        assert!(msg.contains("nothing is gated"), "{msg}");
        // Relative as typed -> absolutize, same reason as the config paths.
        assert!(msg.contains("/work/mint.json"), "{msg}");
        // refire reads no config, so the discovery remedies would be actively
        // misleading here.
        assert!(!msg.contains("no lagom.toml discovered"), "{msg}");
        assert!(!msg.contains("create a lagom.toml"), "{msg}");
        assert!(msg.contains("replays the RECORDED policy"), "{msg}");

        let with_rules = record_warning(Path::new("/r/mint.json"), false, Path::new("/work"));
        assert!(
            with_rules.contains("declares rules"),
            "a recorded policy with rules must not be called empty: {with_rules}"
        );
    }

    #[test]
    fn every_serving_surface_goes_through_one_call_site() {
        // `refire` shipped without the ungated diagnostic because it grew its own
        // copy of the "serve or serve_audited" tail. This tripwire pins how many
        // times each proxy entrypoint is named in this file, so a third surface —
        // or a re-inlined second one — fails here instead of silently serving
        // undiagnosed. The two composed spawn+run wrappers are pinned at 0, so a new
        // caller taking that shorthand — the one `refire` skipped the diagnostic
        // with — turns this red.
        //
        // What this is NOT: it is a COUNT, so it does not pin *which* function
        // holds the call, and a count of 1 survives a call moving between call
        // sites. Textual on purpose: the needles are assembled at runtime so this
        // test's own source does not match them. Residual: it counts source text,
        // so an aliasing `use lagom_proxy::serve as x` or a renamed re-export would
        // evade it. It is a drift tripwire, not a proof of exclusivity.
        let src = include_str!("app.rs");
        // Every mismatch is collected so a red run reports all four counts at once
        // rather than stopping at the first.
        let mut wrong = Vec::new();
        for (name, want) in [
            ("serve", 0usize),
            ("serve_audited", 0),
            ("spawn_and_validate", 1),
            ("validate_only", 1),
        ] {
            let needle = format!("lagom_proxy::{name}(");
            let got = src.matches(&needle).count();
            if got != want {
                wrong.push(format!("`{needle}` expected {want}, found {got}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "proxy call-site counts drifted: {}",
            wrong.join("; ")
        );
    }

    #[test]
    fn preflight_reply_is_confined_to_the_pre_bridge_arm() {
        // LGM-6's stdout reply is legitimate only while nothing else owns stdout.
        // This pins the shape that makes that true in the source as written: ONE
        // reply call site (the `Err` arm of `bridge_or_correlate`), ONE bridge call
        // site (its `Ok` arm), and exactly TWO composed serving arms in `Cli::run`.
        // A reply added after a bridge failure, or a third composed surface, turns
        // this red.
        //
        // What this is NOT: a COUNT, not a location and not a reachability proof —
        // it cannot see that the one reply call site sits on the `Err` arm, only
        // that there is one of it. Textual, so the needles are assembled at runtime
        // to keep this test's own source from matching them; an alias, or a helper
        // wrapping either call, would evade it. Drift tripwire, not exclusivity.
        let src = include_str!("app.rs");
        let mut wrong = Vec::new();
        for (needle, want, what) in [
            (
                format!("{}_preflight_error(", "reply"),
                2usize,
                "definition + one call site",
            ),
            (
                format!("bridge_{}(", "or_correlate"),
                3,
                "definition + the two Cli::run serving arms",
            ),
            // The `log`-argument form, because the bare `server.run(` also occurs
            // in prose quoting `bridge.rs`'s own tail.
            (format!("server.{}(log", "run"), 1, "the single bridge step"),
        ] {
            let got = src.matches(&needle).count();
            if got != want {
                wrong.push(format!("`{needle}` expected {want} ({what}), found {got}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "pre-flight reply/bridge call sites drifted: {}",
            wrong.join("; ")
        );
    }

    #[test]
    fn validate_report_distinguishes_empty_passthrough_from_grounded() {
        let ungated = validate_report(true);
        assert!(
            !ungated.contains("policy is grounded"),
            "an empty policy must not be reported as grounded: {ungated}"
        );
        assert!(ungated.contains("passthrough"), "{ungated}");
        assert!(ungated.contains("nothing is gated"), "{ungated}");

        let grounded = validate_report(false);
        assert_eq!(grounded, "ok: policy is grounded against the live upstream");
    }
}
