//! `witness hook <event>`: append hook payloads to the session log.
//!
//! Runs on every tool call of the agent, so it must be fast, must never
//! block and must always exit 0. Errors go to stderr, which Claude Code
//! does not show the model on exit 0.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

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

/// Runs the hook. Errors go to stderr and, once the repo is known, to the
/// local hook log; with `WITNESS_DEBUG` set, timings are logged too.
pub fn run(event: &str) {
    let started = Instant::now();
    let (root, payload) = match read_payload(event) {
        Ok(Some(found)) => found,
        // Repositories that never ran `witness init` are left alone.
        Ok(None) => return,
        Err(err) => {
            eprintln!("witness hook {event}: {err:#}");
            return;
        }
    };
    match record(&root, &payload) {
        Err(err) => {
            eprintln!("witness hook {event}: {err:#}");
            workspace::log_hook(&root, &format!("{event}: error: {err:#}"));
        }
        Ok(()) if debug_enabled() => {
            let tool = payload.tool_name.as_deref().unwrap_or("-");
            let ms = started.elapsed().as_millis();
            workspace::log_hook(&root, &format!("{event} {tool}: ok in {ms} ms"));
        }
        Ok(()) => {}
    }
}

fn debug_enabled() -> bool {
    std::env::var("WITNESS_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Reads the payload from stdin and finds the repo it belongs to.
fn read_payload(event: &str) -> Result<Option<(PathBuf, HookPayload)>> {
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
    Ok(workspace::find_root(&cwd).map(|root| (root, payload)))
}

fn record(root: &Path, payload: &HookPayload) -> Result<()> {
    let config = workspace::load_config(root).unwrap_or_else(|err| {
        eprintln!("witness hook: {err:#}; using default config");
        witness_core::Config::default()
    });
    let canonical = std::fs::canonicalize(root).ok().filter(|c| c != root);
    let ctx = CaptureContext {
        config: &config,
        root,
        canonical_root: canonical.as_deref(),
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
