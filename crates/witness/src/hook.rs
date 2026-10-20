//! `witness hook <event>`: append hook payloads to the session log.
//!
//! Runs on every tool call of the agent, so it must be fast, must never
//! block and must always exit 0. Errors go to stderr, which Claude Code
//! does not show the model on exit 0.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Parser;
use time::OffsetDateTime;
use witness_core::capture::{CaptureContext, HookPayload, events_from_payload};

use crate::cli::{Cli, Command};
use crate::workspace;

/// Entry point used by `main` for `witness hook ...`: parses arguments and
/// runs the hook, swallowing every error and panic.
pub fn main_guarded() {
    let outcome = std::panic::catch_unwind(|| match Cli::try_parse() {
        Ok(Cli {
            command: Command::Hook { event },
        }) => run(&event),
        Ok(_) => {}
        Err(err) => eprintln!("witness hook: {err}"),
    });
    if outcome.is_err() {
        eprintln!("witness hook: internal error (panic); event not recorded");
    }
}

/// Runs the hook, logging any error to stderr.
pub fn run(event: &str) {
    if let Err(err) = try_run(event) {
        eprintln!("witness hook {event}: {err:#}");
    }
}

fn try_run(event: &str) -> Result<()> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!("expected a hook payload on stdin");
    }
    let mut input = String::new();
    stdin
        .lock()
        .read_to_string(&mut input)
        .context("reading stdin")?;
    let payload = HookPayload::parse(&input)?;
    if !event_matches(event, &payload.hook_event_name) {
        eprintln!(
            "witness hook {event}: payload is for {}; recording it as that",
            payload.hook_event_name
        );
    }
    let cwd = payload
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from))
        .map_or_else(std::env::current_dir, Ok)
        .context("finding the working directory")?;
    // Repositories that never ran `witness init` are left alone.
    let Some(root) = workspace::find_root(&cwd) else {
        return Ok(());
    };
    record(&root, &payload)
}

fn record(root: &Path, payload: &HookPayload) -> Result<()> {
    let config = workspace::load_config(root).unwrap_or_else(|err| {
        eprintln!("witness hook: {err:#}; using default config");
        witness_core::Config::default()
    });
    let ctx = CaptureContext {
        config: &config,
        root,
        now: OffsetDateTime::now_utc(),
    };
    let read_file = |path: &Path| std::fs::read_to_string(path).ok();
    let events = events_from_payload(payload, &ctx, &read_file);
    workspace::append_events(root, &events)?;
    if payload.hook_event_name != "SessionEnd" {
        workspace::write_current_session(
            root,
            &payload.session_id,
            witness_core::record::AGENT_CLAUDE_CODE,
        )?;
    }
    Ok(())
}

/// `post-tool-use` matches `PostToolUse`, and so on.
fn event_matches(arg: &str, name: &str) -> bool {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase()
    };
    norm(arg) == norm(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_case_event_names_match() {
        assert!(event_matches("post-tool-use", "PostToolUse"));
        assert!(event_matches("session-end", "SessionEnd"));
        assert!(!event_matches("post-tool-use", "PostToolUseFailure"));
    }
}
