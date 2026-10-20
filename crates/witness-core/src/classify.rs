//! v0 hunk classification rules. Kept in one module so they can be swapped.
//!
//! For a hunk at head, look at timeline events for the same file whose spans
//! overlap it:
//!
//! - `guessed`: an assumption was recorded against it;
//! - `human`: otherwise, no agent edit touched it (excluded from the count);
//! - `checked`: a `test` or `build` command exited 0 after the hunk's last
//!   edit, and no `test` or `build` failed after that pass;
//! - `unverified`: edited by the agent, none of the above.

use serde::{Deserialize, Serialize};

use crate::hunk::{Hunk, spans_overlap};
use crate::record::{EventKind, MAX_COMMAND_CHARS, truncate_chars};
use crate::timeline::{Placed, Timeline};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Guessed,
    Unverified,
    Checked,
    Human,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Guessed => "guessed",
            Status::Unverified => "unverified",
            Status::Checked => "checked",
            Status::Human => "human",
        }
    }

    /// Whether reviewers should look closely.
    pub fn is_flagged(&self) -> bool {
        matches!(self, Status::Guessed | Status::Unverified)
    }
}

/// The verdict for one hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub status: Status,
    /// One line, for reviewers.
    pub reason: String,
    /// Agent of the last overlapping edit, if any.
    pub agent: Option<String>,
}

/// A swappable set of classification rules.
pub trait Classifier {
    fn classify(&self, hunk: &Hunk, timeline: &Timeline) -> Verdict;
}

/// The v0 rules described in the module docs.
#[derive(Debug, Clone, Copy, Default)]
pub struct V0Rules;

const MAX_REASON_TEXT: usize = 120;

impl Classifier for V0Rules {
    fn classify(&self, hunk: &Hunk, timeline: &Timeline) -> Verdict {
        let span = hunk.span();
        let touches =
            |p: &&Placed| p.file() == Some(hunk.file.as_str()) && spans_overlap(p.span, span);

        let assumptions: Vec<&Placed> = timeline
            .events
            .iter()
            .filter(|p| matches!(p.event.kind, EventKind::Assumption { .. }))
            .filter(touches)
            .collect();
        let edits: Vec<&Placed> = timeline
            .events
            .iter()
            .filter(|p| matches!(p.event.kind, EventKind::Edit { .. }))
            .filter(touches)
            .collect();
        let agent = edits
            .last()
            .or(assumptions.last())
            .map(|p| p.event.agent.clone());

        if let Some(first) = assumptions.first() {
            let text = match &first.event.kind {
                EventKind::Assumption { text, .. } => one_line(text, MAX_REASON_TEXT),
                _ => String::new(),
            };
            let more = match assumptions.len() {
                1 => String::new(),
                n => format!(" (+{} more)", n - 1),
            };
            return Verdict {
                status: Status::Guessed,
                reason: format!("Assumed: {text}{more}"),
                agent,
            };
        }

        let Some(last_edit) = edits.last() else {
            return Verdict {
                status: Status::Human,
                reason: "No agent edit recorded for these lines".to_owned(),
                agent: None,
            };
        };
        let who = last_edit.event.agent.as_str();

        // Verifying runs after the last edit, in order.
        let runs: Vec<(&Placed, &str, Option<i32>)> = timeline
            .events
            .iter()
            .filter(|p| p.order > last_edit.order)
            .filter_map(|p| match &p.event.kind {
                EventKind::Command {
                    classification,
                    exit_code,
                    command,
                } if classification.verifies() => Some((p, command.as_str(), *exit_code)),
                _ => None,
            })
            .collect();

        let last_pass = runs.iter().rev().find(|(_, _, code)| *code == Some(0));
        let last_fail = runs
            .iter()
            .rev()
            .find(|(_, _, code)| code.is_some_and(|c| c != 0));

        match (last_pass, last_fail) {
            (Some((pass, cmd, _)), fail) if fail.is_none_or(|(f, _, _)| f.order < pass.order) => {
                Verdict {
                    status: Status::Checked,
                    reason: format!("`{}` passed after the last edit", code_text(cmd)),
                    agent: Some(who.to_owned()),
                }
            }
            (_, Some((_, cmd, code))) => Verdict {
                status: Status::Unverified,
                reason: format!(
                    "Edited by {who}; `{}` failed (exit {}) after the last edit",
                    code_text(cmd),
                    code.unwrap_or_default()
                ),
                agent: Some(who.to_owned()),
            },
            _ => {
                let reason = match runs.last() {
                    Some((_, cmd, _)) => format!(
                        "Edited by {who}; `{}` ran after the last edit but its exit code is unknown",
                        code_text(cmd)
                    ),
                    None => format!("Edited by {who}; no test or build ran after the last edit"),
                };
                Verdict {
                    status: Status::Unverified,
                    reason,
                    agent: Some(who.to_owned()),
                }
            }
        }
    }
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", truncate_chars(&flat, max - 1))
    } else {
        flat
    }
}

/// Command text safe to put inside a markdown code span.
fn code_text(cmd: &str) -> String {
    one_line(&cmd.replace('`', "'"), MAX_COMMAND_CHARS.min(80))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Change, CommandClass, Event, LineRange};
    use time::Duration;
    use time::macros::datetime;

    fn hunk(file: &str, new_start: u32, new_count: u32) -> Hunk {
        Hunk {
            file: file.into(),
            old_start: new_start,
            old_count: 0,
            new_start,
            new_count,
            added: Vec::new(),
            binary: false,
        }
    }

    struct Log(Vec<Event>);

    impl Log {
        fn new() -> Self {
            Log(Vec::new())
        }
        fn push(mut self, kind: EventKind) -> Self {
            let secs = i64::try_from(self.0.len()).unwrap();
            self.0.push(Event::new(
                "s",
                "claude-code",
                datetime!(2026-10-20 00:00:00 UTC) + Duration::seconds(secs),
                kind,
            ));
            self
        }
        fn edit(self, file: &str, start: u32, added: u32) -> Self {
            let change = Change {
                start,
                removed: 0,
                added,
            };
            self.push(EventKind::Edit {
                file: file.into(),
                lines: Some(change.new_range()),
                change: Some(change),
                tool: "Edit".into(),
            })
        }
        fn whole_file_edit(self, file: &str) -> Self {
            self.push(EventKind::Edit {
                file: file.into(),
                lines: None,
                change: None,
                tool: "NotebookEdit".into(),
            })
        }
        fn run(self, class: CommandClass, cmd: &str, exit: Option<i32>) -> Self {
            self.push(EventKind::Command {
                classification: class,
                exit_code: exit,
                command: cmd.into(),
            })
        }
        fn assume(self, file: &str, lines: Option<(u32, u32)>, text: &str) -> Self {
            self.push(EventKind::Assumption {
                file: file.into(),
                lines: lines.map(|(a, b)| LineRange::new(a, b)),
                text: text.into(),
            })
        }
        fn verdict(self, hunk: &Hunk) -> Verdict {
            V0Rules.classify(hunk, &Timeline::build(vec![self.0]))
        }
    }

    #[test]
    fn guessed_when_assumption_overlaps() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .assume("a.rs", Some((12, 12)), "API returns UTC\ntimestamps")
            .run(CommandClass::Test, "cargo test", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Guessed);
        assert_eq!(v.reason, "Assumed: API returns UTC timestamps");
    }

    #[test]
    fn guessed_counts_extra_assumptions_and_file_wide_ones() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .assume("a.rs", None, "default timeout is 30s")
            .assume("a.rs", Some((11, 11)), "ids are unique")
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Guessed);
        assert_eq!(v.reason, "Assumed: default timeout is 30s (+1 more)");
    }

    #[test]
    fn assumption_elsewhere_does_not_flag() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .assume("a.rs", Some((40, 41)), "x")
            .assume("b.rs", Some((10, 12)), "y")
            .run(CommandClass::Test, "cargo test", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Checked);
    }

    #[test]
    fn human_when_no_agent_edit_overlaps() {
        let v = Log::new()
            .edit("a.rs", 100, 2)
            .edit("b.rs", 10, 2)
            .run(CommandClass::Test, "cargo test", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Human);
        assert_eq!(v.agent, None);
    }

    #[test]
    fn checked_when_test_passes_after_last_edit() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Test, "cargo test", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Checked);
        assert_eq!(v.reason, "`cargo test` passed after the last edit");
        assert_eq!(v.agent.as_deref(), Some("claude-code"));
    }

    #[test]
    fn checked_by_build_too() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Build, "cargo build", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Checked);
    }

    #[test]
    fn unverified_when_test_passed_only_before_last_edit() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Test, "cargo test", Some(0))
            .edit("a.rs", 12, 1)
            .verdict(&hunk("a.rs", 10, 6));
        assert_eq!(v.status, Status::Unverified);
        assert_eq!(
            v.reason,
            "Edited by claude-code; no test or build ran after the last edit"
        );
    }

    #[test]
    fn unverified_when_only_lint_or_other_ran() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Lint, "cargo clippy", Some(0))
            .run(CommandClass::Other, "ls", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Unverified);
    }

    #[test]
    fn unverified_when_test_failed_after_pass() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Build, "cargo build", Some(0))
            .run(CommandClass::Test, "cargo test", Some(101))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Unverified);
        assert_eq!(
            v.reason,
            "Edited by claude-code; `cargo test` failed (exit 101) after the last edit"
        );
    }

    #[test]
    fn checked_when_fail_then_pass() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Test, "cargo test", Some(1))
            .run(CommandClass::Test, "cargo test", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Checked);
    }

    #[test]
    fn unknown_exit_code_does_not_verify() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Test, "cargo test | tail", None)
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Unverified);
        assert!(v.reason.contains("exit code is unknown"), "{}", v.reason);
    }

    #[test]
    fn whole_file_edit_matches_any_hunk_in_file() {
        let v = Log::new()
            .whole_file_edit("a.ipynb")
            .verdict(&hunk("a.ipynb", 300, 2));
        assert_eq!(v.status, Status::Unverified);
    }

    #[test]
    fn edit_shifted_by_later_edit_still_matches() {
        // Edit at 50, then 5 lines inserted at 10: the first edit is now at 55.
        let log = Log::new()
            .edit("a.rs", 50, 1)
            .run(CommandClass::Test, "cargo test", Some(0))
            .edit("a.rs", 10, 5);
        let v = log.verdict(&hunk("a.rs", 55, 1));
        assert_eq!(v.status, Status::Checked);
    }

    #[test]
    fn deletion_hunk_matches_deletion_edit() {
        let change = Change {
            start: 8,
            removed: 3,
            added: 0,
        };
        let v = Log::new()
            .push(EventKind::Edit {
                file: "a.rs".into(),
                lines: Some(change.new_range()),
                change: Some(change),
                tool: "Edit".into(),
            })
            .verdict(&hunk("a.rs", 7, 0));
        assert_eq!(v.status, Status::Unverified);
    }

    #[test]
    fn long_reasons_are_one_line_and_bounded() {
        let long = "word ".repeat(100);
        let v = Log::new()
            .assume("a.rs", None, &long)
            .verdict(&hunk("a.rs", 1, 1));
        assert!(!v.reason.contains('\n'));
        assert!(v.reason.chars().count() <= "Assumed: ".len() + MAX_REASON_TEXT);
    }
}
