//! # lagom (CLI)
//!
//! The standalone binary face (`SPEC.md` §7.1): a harness-agnostic stdio proxy
//! drop-in plus authoring aids. Subcommands:
//!
//! - `serve`       — run the stdio proxy (spawn upstream child, serve the
//!   projected surface). With `--audit <path> [--run-id <id>]` it persists a full
//!   `MintRecord` (resolved policy + provenance) plus the original defs and every
//!   rewrite/rejection as an append-only JSONL trace for traceability + refire
//!   (`SPEC.md` §8.2, §9.3).
//! - `refire`      — re-mint an identical server from a persisted `MintRecord`
//!   (`--record <path>`), reproducing the exact recorded projection even after
//!   the source config has drifted (`SPEC.md` §8.2).
//! - `emit`        — print the harness stdio-server snippet to paste into
//!   `.mcp.json` / `settings.json` (lagom never edits them, `SPEC.md` §6.5).
//! - `validate`    — check a policy against the live upstream (`SPEC.md` §5.3),
//!   non-zero exit on drift.
//! - `emit-skills` — write the shipped skill documents (`lagom-slim-docs`,
//!   `lagom-dynamic-mint`) into the consumer's repo (`SPEC.md` §12).
//!
//! ## Sourcing the upstream command
//!
//! `lagom.toml` carries only the *narrowing policy* (`SPEC.md` §6.2); it does
//! **not** carry how to launch the upstream. The launch command is supplied on
//! the CLI as trailing arguments after `--`:
//!
//! ```text
//! lagom serve --config lagom.toml -- npx -y @some/mcp-server --flag
//! ```
//!
//! `serve` and `validate` both need a live upstream and so require it; `emit`
//! embeds it into the snippet it prints so the pasted entry is self-contained.

mod app;
mod emit;

use std::process::ExitCode;

use clap::Parser;

use app::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.run().await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("lagom: {err}");
            ExitCode::FAILURE
        }
    }
}
