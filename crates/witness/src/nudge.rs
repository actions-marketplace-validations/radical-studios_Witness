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

/// What a hook tells the user and the agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    /// A note for the user; the agent doesn't see it.
    pub message: Option<String>,
    /// Context for the agent at the start of a turn.
    pub context: Option<String>,
    /// Ask the agent to continue, with this reason.
    pub block: Option<String>,
}

impl Reply {
    pub fn message(text: String) -> Self {
        Self {
            message: Some(text),
            ..Self::default()
        }
    }

    pub fn context(text: String) -> Self {
        Self {
            context: Some(text),
            ..Self::default()
        }
    }

    pub fn block(reason: String) -> Self {
        Self {
            block: Some(reason),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.message.is_none() && self.context.is_none() && self.block.is_none()
    }

    /// Both replies' parts, `other`'s taking the place of missing ones.
    pub fn or(self, other: Reply) -> Reply {
        Reply {
            message: self.message.or(other.message),
            context: self.context.or(other.context),
            block: self.block.or(other.block),
        }
    }
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
        return Ok(Some(Reply::block(reason)));
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
    Ok(Some(Reply::message(note)))
}

/// At the start of a turn, or when a session resumes: the reminder left by
/// the previous turn, delivered once.
pub fn pending(root: &Path, session: &str) -> Option<Reply> {
    let path = reminder_path(root, session, "txt");
    let text = fs::read_to_string(&path).ok()?;
    let _ = fs::rename(&path, reminder_path(root, session, "seen"));
    Some(Reply::context(text))
}

/// When a session starts from the Claude Code plugin: a note if the plugin
/// and the binary are different releases (`plugin_root` is
/// `$CLAUDE_PLUGIN_ROOT`).
pub fn version_check(plugin_root: Option<&Path>) -> Option<Reply> {
    let manifest = plugin_root?.join(".claude-plugin").join("plugin.json");
    let text = fs::read_to_string(manifest).ok()?;
    let plugin: serde_json::Value = serde_json::from_str(&text).ok()?;
    let plugin = plugin.get("version")?.as_str()?;
    let binary = env!("CARGO_PKG_VERSION");
    let release = |v: &str| v.split('.').take(2).collect::<Vec<_>>().join(".");
    (release(plugin) != release(binary)).then(|| {
        Reply::message(format!(
            "Witness: the plugin ({plugin}) and the witness binary ({binary}) are different \
             releases; update both (`/plugin update witness`, and install.sh for the binary)."
        ))
    })
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

/// The reply in the agent's hook output format; `None` when there is
/// nothing the agent can take. `event` is the agent's name for the hook
/// event.
pub fn render(agent: &str, event: &str, reply: &Reply) -> Option<String> {
    if let Some(reason) = &reply.block {
        return Some(
            match agent {
                "cursor" => json!({ "followup_message": reason }),
                _ => json!({ "decision": "block", "reason": reason }),
            }
            .to_string(),
        );
    }
    // Copilot and Cursor ignore the output of these hooks.
    if matches!(agent, "copilot" | "cursor") {
        return None;
    }
    let mut out = serde_json::Map::new();
    if let Some(message) = &reply.message {
        out.insert("systemMessage".into(), json!(message));
    }
    if let Some(context) = &reply.context {
        out.insert(
            "hookSpecificOutput".into(),
            json!({ "hookEventName": event, "additionalContext": context }),
        );
    }
    (!out.is_empty()).then(|| serde_json::Value::Object(out).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_per_agent() {
        let block = Reply::block("check it".into());
        assert_eq!(
            render("claude-code", "Stop", &block).unwrap(),
            r#"{"decision":"block","reason":"check it"}"#
        );
        assert_eq!(
            render("cursor", "stop", &block).unwrap(),
            r#"{"followup_message":"check it"}"#
        );
        let remind = Reply::message("note".into());
        assert_eq!(
            render("codex", "Stop", &remind).unwrap(),
            r#"{"systemMessage":"note"}"#
        );
        assert_eq!(render("copilot", "agentStop", &remind), None);
        let context = Reply::context("ctx".into());
        assert_eq!(
            render("claude-code", "UserPromptSubmit", &context).unwrap(),
            r#"{"hookSpecificOutput":{"additionalContext":"ctx","hookEventName":"UserPromptSubmit"}}"#
        );
        let both = Reply::message("note".into()).or(context);
        let out = render("gemini-cli", "SessionStart", &both).unwrap();
        assert!(out.contains(r#""systemMessage":"note""#), "{out}");
        assert!(out.contains(r#""hookEventName":"SessionStart""#), "{out}");
        assert_eq!(render("claude-code", "Stop", &Reply::default()), None);
    }

    #[test]
    fn checks_the_plugin_release() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join(".claude-plugin");
        fs::create_dir_all(&manifest).unwrap();
        let release: Vec<&str> = env!("CARGO_PKG_VERSION").split('.').collect();
        let same = format!("{}.{}.99", release[0], release[1]);
        fs::write(
            manifest.join("plugin.json"),
            format!(r#"{{"version":"{same}"}}"#),
        )
        .unwrap();
        assert_eq!(version_check(Some(dir.path())), None);
        fs::write(manifest.join("plugin.json"), r#"{"version":"0.0.1"}"#).unwrap();
        let note = version_check(Some(dir.path())).unwrap().message.unwrap();
        assert!(note.contains("(0.0.1)"), "{note}");
        assert_eq!(version_check(None), None);
    }
}
