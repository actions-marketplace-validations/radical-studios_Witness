//! `witness hook [--agent <agent>] <event>`: record a coding agent's hook
//! call in the session log.
//!
//! Runs on every tool call of the agent, so it must be fast, must never
//! block and must always exit 0 (some agents deny a tool call when a
//! pre-tool hook fails). Errors go to stderr, which agents don't show the
//! model on exit 0, and to the local hook log.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use clap::Parser;
use time::OffsetDateTime;
use witness_core::capture::{
    CaptureContext, CapturePlan, HookEvent, HookInput, Phase, ToolAction, call_key, capture_plan,
    events_for, parse_hook,
};
use witness_core::record::{Event, EventKind, session_file_stem};

use crate::cli::{Cli, Command};
use crate::nudge;
use crate::snapshot::Snapshots;
use crate::workspace;

/// Agents that have only after-hooks: their shell commands are compared
/// with the session's latest snapshot instead of a pre-command one.
const POST_ONLY_AGENTS: &[&str] = &["cursor"];

/// Entry point used by `main` for `witness hook ...`: parses arguments and
/// runs the hook, swallowing every error and panic.
pub fn main_guarded() {
    let outcome = std::panic::catch_unwind(|| match Cli::try_parse() {
        Ok(Cli {
            command: Command::Hook { agent, event },
        }) => run(agent.as_deref(), &event),
        Ok(_) => {}
        Err(err) => eprintln!("witness hook: {err}"),
    });
    if outcome.is_err() {
        eprintln!("witness hook: internal error (panic); event not recorded");
    }
}

/// Runs the hook. Errors go to stderr and, once the repo is known, to the
/// local hook log; with `WITNESS_DEBUG` set, timings are logged too.
pub fn run(agent: Option<&str>, event: &str) {
    let started = Instant::now();
    let (root, input) = match read_input(agent, event) {
        Ok(Some(found)) => found,
        // Repositories that never ran `witness init` are left alone.
        Ok(None) => return,
        Err(err) => {
            eprintln!("witness hook {event}: {err:#}");
            return;
        }
    };
    if !first_delivery(&root, &input) {
        return;
    }
    let recorded = record(&root, &input);
    if recorded.is_ok() {
        reply(&root, &input, event);
    }
    match recorded {
        Err(err) => {
            eprintln!("witness hook {event}: {err:#}");
            workspace::log_hook(&root, &format!("{} {event}: error: {err:#}", input.agent));
        }
        Ok(()) if debug_enabled() => {
            let tool = match &input.event {
                HookEvent::Tool(call) => call.tool.as_str(),
                _ => "-",
            };
            let ms = started.elapsed().as_millis();
            workspace::log_hook(
                &root,
                &format!("{} {event} {tool}: ok in {ms} ms", input.agent),
            );
        }
        Ok(()) => {}
    }
}

/// Prints the hook's reply to the agent, if any (see [`nudge`]). Stdout
/// carries nothing else, so agents can parse it.
fn reply(root: &Path, input: &HookInput, event: &str) {
    let (reply, name) = match &input.event {
        HookEvent::Stop {
            hook_active,
            last_message,
        } => {
            match nudge::on_stop(
                root,
                &input.agent,
                &input.session_id,
                *hook_active,
                last_message.as_deref(),
            ) {
                Ok(reply) => (reply, "Stop"),
                Err(err) => {
                    workspace::log_hook(
                        root,
                        &format!("{} {event}: no nudge: {err:#}", input.agent),
                    );
                    return;
                }
            }
        }
        HookEvent::Prompt => (
            nudge::pending(root, &input.session_id),
            if input.agent == "gemini-cli" {
                "BeforeAgent"
            } else {
                "UserPromptSubmit"
            },
        ),
        HookEvent::SessionStart => {
            let plugin = std::env::var_os("CLAUDE_PLUGIN_ROOT").map(PathBuf::from);
            let version = nudge::version_check(plugin.as_deref());
            let pending = nudge::pending(root, &input.session_id);
            let reply = match (version, pending) {
                (Some(v), Some(p)) => Some(v.or(p)),
                (v, p) => v.or(p),
            };
            (reply, "SessionStart")
        }
        _ => return,
    };
    if let Some(text) = reply
        .filter(|r| !r.is_empty())
        .and_then(|r| nudge::render(&input.agent, name, &r))
    {
        println!("{text}");
    }
}

/// Whether this process is the first to handle this hook call. Claude Code
/// can run the hook twice (the plugin and the project settings both install
/// it), so each call is claimed with an exclusively created marker file,
/// keyed by tool call id and phase, or by turn. Markers live per session
/// under `.witness/state/seen/` and go when the session ends or a day
/// later.
fn first_delivery(root: &Path, input: &HookInput) -> bool {
    // Only Claude Code can have Witness installed twice; other agents'
    // call ids may not be unique (e.g. a wrapper script's).
    if input.agent != "claude-code" {
        return true;
    }
    let seen = root.join(workspace::STATE_DIR).join("seen");
    let dir = seen.join(session_file_stem(&input.session_id));
    match &input.event {
        HookEvent::SessionStart => workspace::remove_stale_dirs(&seen, 24 * 60 * 60),
        HookEvent::SessionEnd { .. } => {
            let _ = std::fs::remove_dir_all(&dir);
        }
        _ => {}
    }
    let key = match &input.event {
        HookEvent::Tool(call) => match &call.call_id {
            Some(id) => format!("{:?}-{id}", call.phase),
            None => return true,
        },
        // Turn events only with a turn id; a repeat of them is harmless
        // anyway (a reminder is handed over once).
        HookEvent::Prompt | HookEvent::Stop { .. } => match &input.turn {
            Some(turn) => format!("{:?}-{turn}", std::mem::discriminant(&input.event)),
            None => return true,
        },
        _ => return true,
    };
    let claimed = std::fs::create_dir_all(&dir).and_then(|()| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(witness_core::record::line_hash(&key)))
    });
    match claimed {
        Ok(_) => true,
        Err(err) => err.kind() != std::io::ErrorKind::AlreadyExists,
    }
}

fn debug_enabled() -> bool {
    std::env::var("WITNESS_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Reads the payload from stdin and finds the repo it belongs to.
fn read_input(agent: Option<&str>, event: &str) -> Result<Option<(PathBuf, HookInput)>> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!("expected a hook payload on stdin");
    }
    let mut json = String::new();
    stdin
        .lock()
        .read_to_string(&mut json)
        .context("reading stdin")?;
    let input = parse_hook(&json, agent, event)?;
    let cwd = input
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from))
        .map_or_else(std::env::current_dir, Ok)
        .context("finding the working directory")?;
    Ok(workspace::find_root(&cwd).map(|root| (root, input)))
}

fn record(root: &Path, input: &HookInput) -> Result<()> {
    let config = workspace::load_config(root).unwrap_or_else(|err| {
        eprintln!("witness hook: {err:#}; using default config");
        witness_core::Config::default()
    });
    let canonical = std::fs::canonicalize(root).ok().filter(|c| c != root);
    let now = OffsetDateTime::now_utc();
    let ctx = CaptureContext {
        config: &config,
        root,
        canonical_root: canonical.as_deref(),
        now,
    };

    let mut observed = None;
    let mut note = None;
    if let HookEvent::Tool(call) = &input.event
        && config.raw.capture.snapshots
    {
        let plan = capture_plan(input, &ctx);
        let key = call_key(input).unwrap_or_default();
        let snaps = Snapshots::new(root, &config);
        let post_only = POST_ONLY_AGENTS.contains(&input.agent.as_str());
        let outcome = match call.phase {
            Phase::Pre => snaps
                .before(&plan, &key, &input.session_id)
                .map(|n| (None, n)),
            Phase::Post | Phase::PostFailure => snaps
                .after(&plan, &key, &input.session_id, post_only)
                .map(|a| (a.observed, a.note)),
        };
        match outcome {
            Ok((o, n)) => (observed, note) = (o, n),
            // A failed snapshot loses this call's file changes, not the call.
            Err(err) => {
                workspace::log_hook(root, &format!("{}: snapshot failed: {err:#}", input.agent))
            }
        }
        // For post-only agents, move the baseline past changes that are not
        // a shell command's edits (edits the agent reported itself, a
        // branch switch) so the next command isn't credited with them.
        let not_an_edit = match &call.action {
            ToolAction::Edit(_) => true,
            ToolAction::Shell(shell) => witness_core::command::moves_worktree(&shell.command),
            _ => false,
        };
        if post_only
            && call.phase != Phase::Pre
            && not_an_edit
            && plan == CapturePlan::None
            && let Err(err) = snaps.refresh_baseline(&input.session_id)
        {
            workspace::log_hook(root, &format!("{}: snapshot failed: {err:#}", input.agent));
        }
    }

    let read_file = |path: &Path| std::fs::read_to_string(path).ok();
    let mut events = events_for(input, &ctx, &read_file, observed.as_ref());
    if let Some(text) = note {
        let mut event = Event::new(
            &input.session_id,
            &input.agent,
            now,
            EventKind::Note { text },
        );
        event.turn.clone_from(&input.turn);
        events.push(event);
    }
    workspace::append_events(root, &events)?;
    if !matches!(input.event, HookEvent::SessionEnd { .. }) {
        workspace::write_current_session(root, &input.session_id, &input.agent)?;
    }
    Ok(())
}
