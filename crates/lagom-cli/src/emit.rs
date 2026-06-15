//! Rendering the harness stdio-server snippet (`SPEC.md` §6.5) and parsing the
//! upstream launch command from CLI trailing args.
//!
//! lagom **never edits** a harness config; `lagom emit` prints the exact entry a
//! user pastes into `.mcp.json` / `settings.json`. The printed entry runs
//! `lagom serve`, embedding the same `--config` and trailing upstream command so
//! the pasted block is self-contained and reproduces the projection.

use std::path::Path;

use lagom_proxy::UpstreamCommand;
use serde_json::{Value, json};

/// Turn the trailing CLI args (everything after `--`) into an [`UpstreamCommand`].
///
/// The first token is the executable; the rest are its arguments. Returns an
/// error string (never swallows, `SPEC.md` §9.1) if no command was supplied —
/// `serve`/`validate`/`emit` all need a real upstream.
pub fn parse_upstream(mut tokens: Vec<String>) -> Result<UpstreamCommand, String> {
    if tokens.is_empty() {
        return Err("no upstream command given; pass it after `--`, e.g. \
             `lagom serve -- npx -y @some/mcp-server`"
            .to_string());
    }
    let command = tokens.remove(0);
    Ok(UpstreamCommand {
        command,
        args: tokens,
        env: Vec::new(),
    })
}

/// Render the harness snippet as pretty JSON: a single-entry `mcpServers` block
/// keyed by `name`, running `lagom serve` with the given config and upstream.
///
/// The shape matches the `.mcp.json` / Claude Code `mcpServers` convention
/// (`{ "command": ..., "args": [...] }`); harnesses that key it differently
/// (e.g. `settings.json`) reuse the same `command`/`args` body. lagom does not
/// write this anywhere — the caller prints it for the user to paste.
pub fn harness_snippet(
    name: &str,
    config: Option<&Path>,
    upstream: &UpstreamCommand,
) -> Result<String, serde_json::Error> {
    let mut args: Vec<Value> = vec![json!("serve")];
    if let Some(cfg) = config {
        args.push(json!("--config"));
        args.push(json!(cfg.to_string_lossy()));
    }
    // Separator before the upstream launch command, then the command + its args.
    args.push(json!("--"));
    args.push(json!(upstream.command));
    for a in &upstream.args {
        args.push(json!(a));
    }

    let snippet = json!({
        "mcpServers": {
            name: {
                "command": "lagom",
                "args": args
            }
        }
    });
    serde_json::to_string_pretty(&snippet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_upstream_splits_command_and_args() {
        let up = parse_upstream(vec!["npx".into(), "-y".into(), "@x/srv".into()]).unwrap();
        assert_eq!(up.command, "npx");
        assert_eq!(up.args, vec!["-y".to_string(), "@x/srv".to_string()]);
        assert!(up.env.is_empty());
    }

    #[test]
    fn parse_upstream_empty_is_error() {
        let err = parse_upstream(vec![]).unwrap_err();
        assert!(err.contains("no upstream command"), "{err}");
    }

    #[test]
    fn snippet_embeds_serve_config_and_upstream() {
        let up = UpstreamCommand {
            command: "npx".into(),
            args: vec!["-y".into(), "@x/srv".into()],
            env: vec![],
        };
        let out = harness_snippet("hylla", Some(&PathBuf::from("lagom.toml")), &up).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let entry = &v["mcpServers"]["hylla"];
        assert_eq!(entry["command"], json!("lagom"));
        assert_eq!(
            entry["args"],
            json!([
                "serve",
                "--config",
                "lagom.toml",
                "--",
                "npx",
                "-y",
                "@x/srv"
            ])
        );
    }

    #[test]
    fn snippet_omits_config_when_absent() {
        let up = UpstreamCommand {
            command: "srv".into(),
            args: vec![],
            env: vec![],
        };
        let out = harness_snippet("lagom", None, &up).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["mcpServers"]["lagom"]["args"],
            json!(["serve", "--", "srv"])
        );
    }
}
