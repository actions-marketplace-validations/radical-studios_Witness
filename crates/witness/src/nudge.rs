//! What the hooks tell the user and the agent when a turn ends with edits
//! that failed or were never checked (`[nudge]` in `witness.toml`).
//!
//! - `remind`: the user sees a one-line note when the turn ends, and the
//!   agent gets the list with the next prompt;
//! - `block`: the agent is asked to keep going, once per turn, until it
//!   checks its edits or explains why it can't;
//! - `off`: nothing.
//!
//! Claims in the agent's final message ("all tests pass") are compared with
//! the record and stored as `claim` events.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::json;
use time::OffsetDateTime;
use witness_core::claims::{self, Claim};
use witness_core::classify::Status;
use witness_core::config::NudgeMode;
use witness_core::record::{Event, EventKind, session_file_stem};
use witness_core::report::Section;

use crate::pipeline::Repo;
use crate::review;
use crate::workspace::{self, STATE_DIR};

/// What to tell the agent and the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// A note for the user; the agent doesn't see it.
    Remind(String),
    /// Ask the agent to continue, with this reason.
    Block(String),
    /// Context for the agent at the start of a turn.
    Context(String),
}

/// Sections listed in a nudge.
const MAX_LISTED: usize = 8;

/// At the end of a turn.
pub fn on_stop(
    root: &Path,
    agent: &str,
    session: &str,
    hook_active: bool,
    last_message: Option<&str>,
) -> Result<Option<Reply>> {
    let repo = Repo::at(root)?;
    let mode = repo.config.raw.nudge.mode;
    let claims = last_message.map(claims::detect).unwrap_or_default();
    if mode == NudgeMode::Off && claims.is_empty() {
        return Ok(None);
    }
    let base = review::resolve_base(&repo.git, None)?;
    let (report, _) = review::worktree_report(&repo, &base)?;
    let mine: Vec<&Section> = report
        .sections()
        .filter(|s| s.evidence.sessions.iter().any(|x| x == session))
        .collect();
    // A session that changed nothing on this branch has nothing to check,
    // and its claims nothing to be compared with.
    if mine.is_empty() {
        return Ok(None);
    }
    let open: Vec<&Section> = mine
        .iter()
        .copied()
        .filter(|s| matches!(s.status, Status::Failing | Status::Unverified))
        .collect();

    let mut contradicted: Vec<Claim> = Vec::new();
    let mut events = Vec::new();
    for claim in &claims {
        let supported = match claim {
            Claim::TestsPass => !mine.iter().any(|s| {
                matches!(
                    s.status,
                    Status::Failing | Status::Unverified | Status::Compiled
                )
            }),
            Claim::Builds => !mine.iter().any(|s| s.status == Status::Failing),
        };
        if !supported {
            contradicted.push(*claim);
        }
        events.push(Event::new(
            session,
            agent,
            OffsetDateTime::now_utc(),
            EventKind::Claim {
                claim: *claim,
                supported,
            },
        ));
    }
    workspace::append_events(root, &events)?;

    let reminder = reminder_path(root, session, "txt");
    let seen = reminder_path(root, session, "seen");
    if open.is_empty() && contradicted.is_empty() {
        let _ = fs::remove_file(&reminder);
        let _ = fs::remove_file(&seen);
        return Ok(None);
    }
    let list = listing(&open);
    let claim_note = contradicted
        .iter()
        .map(|c| {
            format!(
                "the last message says {}, but the record doesn't show it",
                c.describe()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    if mode == NudgeMode::Block && !hook_active {
        let mut reason = String::from("Witness: ");
        if !claim_note.is_empty() {
            reason.push_str(&claim_note);
            reason.push_str(". ");
        }
        if !open.is_empty() {
            reason.push_str("These sections from this session have no passing check yet:\n");
            reason.push_str(&list);
            reason.push('\n');
        }
        reason.push_str(
            "Run the tests or build that cover them (without piping their output), or say in your \
             reply why they can't be checked.",
        );
        return Ok(Some(Reply::Block(reason)));
    }
    if mode == NudgeMode::Off {
        return Ok(None);
    }
    // Remind: the agent hears about it with the next prompt.
    let mut context = String::from("Witness recorded that when the previous turn ended");
    if !open.is_empty() {
        context.push_str(", these sections had no passing check:\n");
        context.push_str(&list);
    }
    if !claim_note.is_empty() {
        context.push_str(&format!("\n({claim_note}.)"));
    }
    // The agent already has this exact list; don't repeat it every turn.
    if fs::read_to_string(&seen).ok().as_deref() != Some(context.as_str()) {
        if let Some(parent) = reminder.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&reminder, &context)?;
    }
    let mut note = format!(
        "Witness: {} {} from this session {} no passing check",
        open.len(),
        if open.len() == 1 {
            "section"
        } else {
            "sections"
        },
        if open.len() == 1 { "has" } else { "have" }
    );
    if !claim_note.is_empty() {
        note = format!("Witness: {claim_note}");
        if !open.is_empty() {
            note.push_str(&format!("; {} unchecked", open.len()));
        }
    }
    note.push_str(". Run `witness status` for details.");
    Ok(Some(Reply::Remind(note)))
}

/// At the start of a turn, or when a session resumes: the reminder left by
/// the previous turn, delivered once.
pub fn pending(root: &Path, session: &str) -> Option<Reply> {
    let path = reminder_path(root, session, "txt");
    let text = fs::read_to_string(&path).ok()?;
    let _ = fs::rename(&path, reminder_path(root, session, "seen"));
    Some(Reply::Context(text))
}

fn reminder_path(root: &Path, session: &str, ext: &str) -> PathBuf {
    root.join(STATE_DIR)
        .join("reminders")
        .join(format!("{}.{ext}", session_file_stem(session)))
}

fn listing(sections: &[&Section]) -> String {
    let mut lines: Vec<String> = sections
        .iter()
        .take(MAX_LISTED)
        .map(|s| {
            format!(
                "- {}:{} ({}): {}",
                s.file,
                s.lines,
                s.status.as_str(),
                s.reason
            )
        })
        .collect();
    if sections.len() > MAX_LISTED {
        lines.push(format!("- and {} more", sections.len() - MAX_LISTED));
    }
    lines.join("\n")
}

/// The reply in the agent's hook output format; `None` when the agent
/// can't take it. `event` is the agent's name for the hook event.
pub fn render(agent: &str, event: &str, reply: &Reply) -> Option<String> {
    let text = match (agent, reply) {
        ("claude-code" | "codex" | "gemini-cli" | "generic", Reply::Remind(note)) => {
            json!({ "systemMessage": note }).to_string()
        }
        ("claude-code" | "codex" | "copilot" | "gemini-cli" | "generic", Reply::Block(reason)) => {
            json!({ "decision": "block", "reason": reason }).to_string()
        }
        ("cursor", Reply::Block(reason)) => json!({ "followup_message": reason }).to_string(),
        // Claude Code adds a prompt or session-start hook's plain output to
        // the agent's context.
        ("claude-code", Reply::Context(text)) => text.clone(),
        ("codex" | "gemini-cli", Reply::Context(text)) => json!({
            "hookSpecificOutput": { "hookEventName": event, "additionalContext": text }
        })
        .to_string(),
        _ => return None,
    };
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_per_agent() {
        let block = Reply::Block("check it".into());
        assert_eq!(
            render("claude-code", "Stop", &block).unwrap(),
            r#"{"decision":"block","reason":"check it"}"#
        );
        assert_eq!(
            render("cursor", "stop", &block).unwrap(),
            r#"{"followup_message":"check it"}"#
        );
        let remind = Reply::Remind("note".into());
        assert_eq!(
            render("codex", "Stop", &remind).unwrap(),
            r#"{"systemMessage":"note"}"#
        );
        assert_eq!(render("copilot", "agentStop", &remind), None);
        let context = Reply::Context("ctx".into());
        assert_eq!(
            render("claude-code", "UserPromptSubmit", &context).unwrap(),
            "ctx"
        );
        assert!(
            render("codex", "UserPromptSubmit", &context)
                .unwrap()
                .contains(r#""hookEventName":"UserPromptSubmit""#)
        );
    }
}
