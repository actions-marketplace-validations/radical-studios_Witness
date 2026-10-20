//! Classification rules. Kept in one module so they can be swapped.
//!
//! For a segment at head (see [`crate::provenance`]), look at the agent
//! edits that wrote it, the assumptions about it, and the test and build
//! runs after its last edit that cover its file:
//!
//! - `failing`: the last covering test run failed, or the last build did
//!   with no test since;
//! - `guessed`: otherwise, an assumption was recorded against it;
//! - `tested`: a covering test passed after the last edit;
//! - `compiled`: only a covering build passed;
//! - `unverified`: written by the agent, none of the above;
//! - `human`: no agent edit wrote it (excluded from the count).
//!
//! `unrecorded` is decided by the report from commit metadata: human-looking
//! lines from a commit attributed to an agent.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::Config;
use crate::provenance::Segment;
use crate::record::{CommandClass, EventKind, MAX_COMMAND_CHARS, truncate_chars};
use crate::scope::{self, Coverage, Packages};
use crate::testsum::{RunOutcome, TestCounts};
use crate::timeline::{Placed, Timeline};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A test or build failed after the last edit.
    Failing,
    /// The agent recorded an assumption about these lines.
    Guessed,
    /// From a commit attributed to an agent, with no Witness record.
    Unrecorded,
    /// Written by the agent; nothing passed afterwards.
    Unverified,
    /// Only a build passed after the last edit.
    Compiled,
    /// A test covering the file passed after the last edit.
    Tested,
    Human,
}

impl Status {
    pub const ALL: [Status; 7] = [
        Status::Failing,
        Status::Guessed,
        Status::Unrecorded,
        Status::Unverified,
        Status::Compiled,
        Status::Tested,
        Status::Human,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Failing => "failing",
            Status::Guessed => "guessed",
            Status::Unrecorded => "unrecorded",
            Status::Unverified => "unverified",
            Status::Compiled => "compiled",
            Status::Tested => "tested",
            Status::Human => "human",
        }
    }

    /// Base priority for ordering sections to review.
    pub fn weight(&self) -> u32 {
        match self {
            Status::Failing => 100,
            Status::Guessed => 80,
            Status::Unrecorded => 70,
            Status::Unverified => 60,
            Status::Compiled => 30,
            Status::Tested => 10,
            Status::Human => 0,
        }
    }
}

/// The verdict for one segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub status: Status,
    /// One line, for reviewers.
    pub reason: String,
    /// Agent of the last edit (or assumption), if any.
    pub agent: Option<String>,
    pub evidence: Evidence,
}

/// What the verdict rests on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// When the agent last edited these lines.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::record::timestamp::option"
    )]
    pub last_edit: Option<OffsetDateTime>,
    /// Agent edits that wrote these lines.
    pub edits: usize,
    /// The run that decided the status, if one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRef {
    pub command: String,
    pub class: CommandClass,
    pub outcome: RunOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestCounts>,
    /// Only some tests ran.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub filtered: bool,
    #[serde(with = "crate::record::timestamp")]
    pub at: OffsetDateTime,
}

/// What classification looks at besides the segment.
pub struct ClassifyContext<'a> {
    pub timeline: &'a Timeline,
    pub config: &'a Config,
    pub packages: &'a dyn Packages,
}

/// A swappable set of classification rules.
pub trait Classifier {
    fn classify(&self, segment: &Segment, ctx: &ClassifyContext<'_>) -> Verdict;
}

/// The rules described in the module docs.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultRules;

const MAX_REASON_TEXT: usize = 120;

impl Classifier for DefaultRules {
    fn classify(&self, segment: &Segment, ctx: &ClassifyContext<'_>) -> Verdict {
        let timeline = ctx.timeline;
        let placed = |idx: &usize| timeline.events.get(*idx);
        let assumptions: Vec<&Placed> = segment.assumptions.iter().filter_map(placed).collect();
        let mut edits: Vec<&Placed> = segment.edits.iter().filter_map(placed).collect();
        edits.sort_by_key(|p| p.order);
        let agent = edits
            .last()
            .or(assumptions.last())
            .map(|p| p.event.agent.clone());
        let assumed = assumptions.first().map(|first| {
            let text = match &first.event.kind {
                EventKind::Assumption { text, .. } => one_line(text, MAX_REASON_TEXT),
                _ => String::new(),
            };
            let more = match assumptions.len() {
                1 => String::new(),
                n => format!(" (+{} more)", n - 1),
            };
            format!("Assumed: {text}{more}")
        });

        let Some(last_edit) = edits.last() else {
            return match assumed {
                Some(reason) => Verdict {
                    status: Status::Guessed,
                    reason,
                    agent,
                    evidence: Evidence::default(),
                },
                None => Verdict {
                    status: Status::Human,
                    reason: "No agent edit recorded for these lines".to_owned(),
                    agent: None,
                    evidence: Evidence::default(),
                },
            };
        };
        let who = last_edit.event.agent.as_str();

        // Test and build runs after the last edit that cover this file.
        let mut last_test: Option<(&Placed, RunOutcome)> = None;
        let mut last_build: Option<(&Placed, RunOutcome)> = None;
        let mut unknown: Option<&Placed> = None;
        let mut elsewhere: Option<(&Placed, Vec<String>)> = None;
        for p in timeline.events.iter().filter(|p| p.order > last_edit.order) {
            let EventKind::Command {
                classification,
                scope,
                ..
            } = &p.event.kind
            else {
                continue;
            };
            if !classification.verifies() {
                continue;
            }
            let outcome = p.event.run_outcome();
            if let Coverage::Outside(dirs) =
                scope::covers(scope.as_ref(), &segment.file, ctx.packages)
            {
                if outcome == Some(RunOutcome::Passed) {
                    elsewhere = Some((p, dirs));
                }
                continue;
            }
            match (outcome, classification) {
                (Some(o), CommandClass::Test) => last_test = Some((p, o)),
                (Some(o), _) => last_build = Some((p, o)),
                (None, _) => unknown = Some(p),
            }
        }

        use RunOutcome::{Failed, Passed};
        let (status, reason, run) = match (last_test, last_build) {
            (Some((t, Failed)), _) => (Status::Failing, failed_reason(t), Some(t)),
            (Some((t, Passed)), Some((b, Failed))) if b.order > t.order => {
                (Status::Failing, failed_reason(b), Some(b))
            }
            (Some((t, Passed)), _) => (
                Status::Tested,
                format!(
                    "`{}` passed after the last edit{}",
                    code_text(command_of(t)),
                    counts_suffix(t)
                ),
                Some(t),
            ),
            (None, Some((b, Failed))) => (Status::Failing, failed_reason(b), Some(b)),
            (None, Some((b, Passed))) => (
                Status::Compiled,
                format!(
                    "Only built after the last edit: `{}` passed; no test ran",
                    code_text(command_of(b))
                ),
                Some(b),
            ),
            (None, None) => {
                let reason = match (unknown, &elsewhere) {
                    (Some(p), _) => format!(
                        "Edited by {who}; `{}` ran after the last edit but its result is unknown",
                        code_text(command_of(p))
                    ),
                    (None, Some((p, dirs))) => format!(
                        "Edited by {who}; `{}` passed after the last edit but covers only {}",
                        code_text(command_of(p)),
                        dirs.iter()
                            .map(|d| format!("`{d}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    (None, None) => {
                        format!("Edited by {who}; no test or build ran after the last edit")
                    }
                };
                (Status::Unverified, reason, None)
            }
        };
        let (status, reason) = match assumed {
            Some(assumed) if status != Status::Failing => (Status::Guessed, assumed),
            _ => (status, reason),
        };
        Verdict {
            status,
            reason,
            agent,
            evidence: Evidence {
                last_edit: Some(last_edit.event.timestamp),
                edits: edits.len(),
                run: run.and_then(run_ref),
            },
        }
    }
}

fn command_of(p: &Placed) -> &str {
    match &p.event.kind {
        EventKind::Command { command, .. } => command,
        _ => "",
    }
}

fn run_ref(p: &Placed) -> Option<RunRef> {
    let EventKind::Command {
        classification,
        command,
        tests,
        scope,
        ..
    } = &p.event.kind
    else {
        return None;
    };
    Some(RunRef {
        command: command.clone(),
        class: *classification,
        outcome: p.event.run_outcome()?,
        tests: *tests,
        filtered: scope.as_ref().is_some_and(|s| s.filtered),
        at: p.event.timestamp,
    })
}

fn failed_reason(p: &Placed) -> String {
    let mut detail: Vec<String> = Vec::new();
    if let EventKind::Command {
        exit_code, tests, ..
    } = &p.event.kind
    {
        if let Some(code) = exit_code.filter(|c| *c != 0) {
            detail.push(format!("exit {code}"));
        }
        if let Some(failed) = tests.and_then(|t| t.failed).filter(|n| *n > 0) {
            detail.push(format!("{failed} failed"));
        }
    }
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({})", detail.join(", "))
    };
    format!(
        "`{}` failed{detail} after the last edit",
        code_text(command_of(p))
    )
}

fn counts_suffix(p: &Placed) -> String {
    match &p.event.kind {
        EventKind::Command {
            tests: Some(tests), ..
        } => tests
            .describe()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default(),
        _ => String::new(),
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
    use crate::hunk::Hunk;
    use crate::record::{Change, Event, LineRange};
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
            removed: Vec::new(),
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
                via: None,
                line_hashes: None,
                removed_hashes: None,
            })
        }
        fn whole_file_edit(self, file: &str) -> Self {
            self.push(EventKind::Edit {
                file: file.into(),
                lines: None,
                change: None,
                tool: "NotebookEdit".into(),
                via: None,
                line_hashes: None,
                removed_hashes: None,
            })
        }
        fn run(self, class: CommandClass, cmd: &str, exit: Option<i32>) -> Self {
            self.push(EventKind::Command {
                classification: class,
                exit_code: exit,
                command: cmd.into(),
                outcome: None,
                tests: None,
                scope: None,
            })
        }
        fn assume(self, file: &str, lines: Option<(u32, u32)>, text: &str) -> Self {
            self.push(EventKind::Assumption {
                file: file.into(),
                lines: lines.map(|(a, b)| LineRange::new(a, b)),
                text: text.into(),
                line_hashes: None,
            })
        }
        fn run_scoped(self, cmd: &str, exit: i32, scope: crate::scope::Scope) -> Self {
            self.push(EventKind::Command {
                classification: CommandClass::Test,
                exit_code: Some(exit),
                command: cmd.into(),
                outcome: None,
                tests: None,
                scope: Some(scope),
            })
        }
        fn run_summary(self, cmd: &str, outcome: RunOutcome, failed: u32) -> Self {
            self.push(EventKind::Command {
                classification: CommandClass::Test,
                exit_code: None,
                command: cmd.into(),
                outcome: Some(outcome),
                tests: Some(TestCounts {
                    passed: Some(10),
                    failed: Some(failed),
                    skipped: None,
                }),
                scope: None,
            })
        }
        fn verdict(self, hunk: &Hunk) -> Verdict {
            self.verdict_with(hunk, &crate::scope::NoPackages)
        }
        fn verdict_with(self, hunk: &Hunk, packages: &dyn Packages) -> Verdict {
            let timeline = Timeline::build(vec![self.0]);
            let segments = crate::provenance::attribute(std::slice::from_ref(hunk), &timeline);
            let config = Config::default();
            let ctx = ClassifyContext {
                timeline: &timeline,
                config: &config,
                packages,
            };
            DefaultRules.classify(&segments[0], &ctx)
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
        assert_eq!(v.status, Status::Tested);
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
        assert_eq!(v.status, Status::Tested);
        assert_eq!(v.reason, "`cargo test` passed after the last edit");
        assert_eq!(v.agent.as_deref(), Some("claude-code"));
    }

    #[test]
    fn a_build_alone_is_compiled() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Build, "cargo build", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Compiled);
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
    fn failing_when_test_failed_after_last_edit() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Build, "cargo build", Some(0))
            .run(CommandClass::Test, "cargo test", Some(101))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Failing);
        assert_eq!(
            v.reason,
            "`cargo test` failed (exit 101) after the last edit"
        );
    }

    #[test]
    fn failing_outranks_assumptions() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .assume("a.rs", None, "x")
            .run(CommandClass::Test, "cargo test", Some(1))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Failing);
    }

    #[test]
    fn only_a_build_is_compiled() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Build, "cargo check", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Compiled);
        assert_eq!(
            v.reason,
            "Only built after the last edit: `cargo check` passed; no test ran"
        );
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Build, "cargo check", Some(101))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Failing);
    }

    #[test]
    fn summaries_decide_piped_runs() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run_summary("cargo test 2>&1 | tail", RunOutcome::Failed, 2)
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Failing);
        assert_eq!(
            v.reason,
            "`cargo test 2>&1 \\| tail` failed (2 failed) after the last edit".replace("\\|", "|")
        );
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run_summary("cargo test | tail", RunOutcome::Passed, 0)
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Tested);
        assert_eq!(
            v.reason,
            "`cargo test | tail` passed after the last edit (10 passed)"
        );
        assert_eq!(v.evidence.run.unwrap().tests.unwrap().passed, Some(10));
    }

    struct Pkgs;
    impl Packages for Pkgs {
        fn dir_of(&self, name: &str) -> Option<String> {
            Some(format!("crates/{name}"))
        }
        fn is_package_root(&self, _dir: &str) -> bool {
            false
        }
    }

    #[test]
    fn runs_scoped_elsewhere_do_not_count() {
        let other = crate::scope::Scope {
            packages: vec!["other".into()],
            ..crate::scope::Scope::default()
        };
        let v = Log::new()
            .edit("crates/core/a.rs", 10, 5)
            .run_scoped("cargo test -p other", 0, other.clone())
            .verdict_with(&hunk("crates/core/a.rs", 10, 5), &Pkgs);
        assert_eq!(v.status, Status::Unverified);
        assert_eq!(
            v.reason,
            "Edited by claude-code; `cargo test -p other` passed after the last edit but covers only `crates/other`"
        );
        let mine = crate::scope::Scope {
            packages: vec!["core".into()],
            filtered: true,
            ..crate::scope::Scope::default()
        };
        let v = Log::new()
            .edit("crates/core/a.rs", 10, 5)
            .run_scoped("cargo test -p core parse", 0, mine)
            .verdict_with(&hunk("crates/core/a.rs", 10, 5), &Pkgs);
        assert_eq!(v.status, Status::Tested);
        assert!(v.evidence.run.unwrap().filtered);
    }

    #[test]
    fn checked_when_fail_then_pass() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Test, "cargo test", Some(1))
            .run(CommandClass::Test, "cargo test", Some(0))
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Tested);
    }

    #[test]
    fn unknown_exit_code_does_not_verify() {
        let v = Log::new()
            .edit("a.rs", 10, 5)
            .run(CommandClass::Test, "cargo test | tail", None)
            .verdict(&hunk("a.rs", 10, 5));
        assert_eq!(v.status, Status::Unverified);
        assert!(v.reason.contains("result is unknown"), "{}", v.reason);
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
        assert_eq!(v.status, Status::Tested);
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
                via: None,
                line_hashes: None,
                removed_hashes: None,
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
