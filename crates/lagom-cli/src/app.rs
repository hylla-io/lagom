//! The clap command surface and dispatch for the `lagom` binary.
//!
//! Each subcommand maps to a method that returns an [`ExitCode`] so drift can
//! exit non-zero (`SPEC.md` §5.3) without panicking. Pure rendering logic
//! (the harness snippet, the shipped skills) lives in [`crate::emit`] /
//! [`crate::skills`] so it is unit-testable without spawning a process.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lagom_proxy::{ResolvedPolicy, UpstreamCommand};

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

/// Discover + merge the policy layers, then resolve a [`ResolvedPolicy`] pairing
/// the merged policy with the supplied upstream launch command.
fn resolve(config: Option<PathBuf>, upstream: Vec<String>) -> Result<ResolvedPolicy, CliError> {
    let upstream = parse_upstream(upstream).map_err(CliError::Upstream)?;
    let cwd = std::env::current_dir().map_err(|source| CliError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let policy = lagom_config::load_discovered(&cwd, config.as_deref())?;
    Ok(ResolvedPolicy { policy, upstream })
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
    let resolved = resolve(config, upstream)?;
    match audit {
        None => lagom_proxy::serve(resolved).await?,
        Some(path) => {
            let log = lagom_audit::AuditLog::open(&path)?;
            lagom_proxy::serve_audited(resolved, Some(log), run_id).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
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
    match lagom_proxy::test_support::spawn_and_validate(resolved).await {
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
