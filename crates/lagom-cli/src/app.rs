//! The clap command surface and dispatch for the `lagom` binary.
//!
//! Each subcommand maps to a method that returns an [`ExitCode`] so drift can
//! exit non-zero (`SPEC.md` §5.3) without panicking. The harness-snippet
//! rendering lives in [`crate::emit`] so it is unit-testable without spawning a
//! process; the shipped skill bytes are sourced from `lagom_proxy::SHIPPED_SKILLS`
//! and written by `cmd_emit_skills`.

use std::path::PathBuf;
use std::process::ExitCode;

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
    pub async fn run(self) -> Result<ExitCode, CliError> {
        match self.command {
            Command::Serve {
                config,
                audit,
                run_id,
                upstream,
            } => cmd_serve(config, audit, run_id, upstream).await,
            Command::Refire {
                record,
                audit,
                run_id,
                upstream,
            } => cmd_refire(record, audit, run_id, upstream).await,
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
/// provenance a mint records for refire (`SPEC.md` §8.2).
///
/// `lagom_config::discover` returns paths highest-precedence first, while
/// [`PolicySources::config_paths`] and the proxy's [`lagom_proxy::mint`] expect
/// base-first (lowest precedence first), so the discovered order is reversed and
/// the explicit `--config` (highest precedence) appended last.
fn sources(config: Option<PathBuf>, upstream: Vec<String>) -> Result<PolicySources, CliError> {
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
    let config_paths = discovered
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    Ok(PolicySources {
        config_paths,
        upstream,
        dynamic_inputs: serde_json::Value::Null,
    })
}

/// Resolve a [`ResolvedPolicy`] from the discovered sources (the policy projected
/// against the supplied upstream launch command).
fn resolve(config: Option<PathBuf>, upstream: Vec<String>) -> Result<ResolvedPolicy, CliError> {
    let sources = sources(config, upstream)?;
    Ok(lagom_proxy::mint(&sources)?)
}

/// `lagom serve`: discover/merge the policy, spawn the upstream, drift-validate,
/// then run the stdio proxy until the session ends.
///
/// With `--audit <path>` the session is routed through
/// [`lagom_proxy::serve_audited`], which opens an append-only JSONL log and
/// records the original upstream defs + resolved policy at session start and
/// every rewrite/rejection thereafter, tagged with `run_id` (`SPEC.md` §8.2,
/// §9.3). Without the flag this is byte-for-byte the prior behavior
/// (`serve`, no audit log).
async fn cmd_serve(
    config: Option<PathBuf>,
    audit: Option<PathBuf>,
    run_id: String,
    upstream: Vec<String>,
) -> Result<ExitCode, CliError> {
    let sources = sources(config, upstream)?;
    match audit {
        None => {
            let resolved = lagom_proxy::mint(&sources)?;
            lagom_proxy::serve(resolved).await?;
        }
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
            lagom_proxy::serve_audited(resolved, Some(log), run_id).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `lagom refire`: re-mint the recorded resolved policy from a persisted
/// [`MintRecord`] and serve it (`SPEC.md` §8.2).
///
/// Reads the record (a bare `MintRecord` JSON object, or a JSONL audit trace
/// whose first `mint` event carries one), refires it (bypassing re-resolution so
/// the exact recorded projection is reproduced even after the config drifted),
/// then runs the stdio proxy. Trailing `--` args override the recorded upstream
/// launch command; `--audit`/`--run-id` append a fresh trace for the refired run.
async fn cmd_refire(
    record_path: PathBuf,
    audit: Option<PathBuf>,
    run_id: Option<String>,
    upstream_override: Vec<String>,
) -> Result<ExitCode, CliError> {
    let record = read_mint_record(&record_path)?;
    let run_id = run_id.unwrap_or_else(|| record.run_id.clone());

    // Refire reproduces the recorded resolved policy byte-for-byte; an explicit
    // trailing upstream command relocates where the same projection's child runs.
    let mut resolved = lagom_proxy::refire(&record);
    if !upstream_override.is_empty() {
        resolved.upstream = parse_upstream(upstream_override).map_err(CliError::Upstream)?;
    }

    match audit {
        None => lagom_proxy::serve(resolved).await?,
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
            lagom_proxy::serve_audited(resolved, Some(log), run_id).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
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
/// every policy reference against it (`SPEC.md` §5.3). Exits non-zero on drift.
async fn cmd_validate(
    config: Option<PathBuf>,
    upstream: Vec<String>,
) -> Result<ExitCode, CliError> {
    let resolved = resolve(config, upstream)?;
    // `spawn_and_validate` spawns the child, probes `tools/list`, and validates.
    // A drifted policy returns `ProxyError::Drift`; on success the returned
    // server is dropped here, which tears the child down (`kill_on_drop`).
    match lagom_proxy::spawn_and_validate(resolved).await {
        Ok(_server) => {
            println!("ok: policy is grounded against the live upstream");
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
