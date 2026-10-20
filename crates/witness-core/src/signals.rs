//! Signals: observations about how a section was written that reviewers
//! should know, beyond its status. Some (configurable with
//! `[report] escalate`) put a section up for close review.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::classify::{Status, Verdict};
use crate::config::{Config, Risk};
use crate::hunk::spans_overlap;
use crate::provenance::Segment;
use crate::record::{CommandClass, EditVia, EventKind};
use crate::testsum::RunOutcome;
use crate::timeline::{Placed, Timeline};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignalKind {
    /// A test file was changed after a test failed, and the tests then
    /// passed: the test may have been bent to fit the code.
    TestChangedAfterFailure,
    /// Assertions were deleted from a test file.
    AssertionsRemoved,
    /// The section is in an area marked `risk = "high"`.
    HighRiskArea,
    /// The agent edited a file it had not read in that session.
    BlindEdit,
    /// The same lines were edited many times, or tests failed repeatedly
    /// along the way.
    Thrash,
    /// The passing run was limited to some of the tests.
    FilteredTestRun,
    /// A human changed lines next to this section in the same hunk.
    HumanModified,
    /// This agent's earlier changes in this area were often rewritten or
    /// reverted after they landed (see `witness stats`).
    RewrittenOften,
}

impl SignalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SignalKind::TestChangedAfterFailure => "test-changed-after-failure",
            SignalKind::AssertionsRemoved => "assertions-removed",
            SignalKind::HighRiskArea => "high-risk-area",
            SignalKind::BlindEdit => "blind-edit",
            SignalKind::Thrash => "thrash",
            SignalKind::FilteredTestRun => "filtered-test-run",
            SignalKind::HumanModified => "human-modified",
            SignalKind::RewrittenOften => "rewritten-often",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signal {
    pub kind: SignalKind,
    /// One short phrase, e.g. `changed after "cargo test" failed`.
    pub detail: String,
}

/// Edits to a section this many times count as thrashing.
const THRASH_EDITS: usize = 4;
/// Failing runs while a section was being written that count as thrashing.
const THRASH_FAILURES: usize = 3;

static ASSERTION: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"\b(assert\w*|expect|should|require\.\w+|t\.(Fatal|Error)\w*|XCTAssert\w*|check|verify)\b|\.to(Be|Equal|Have|Throw|Match|Contain)",
    )
    .ok()
});

/// Signals for one segment. `human_nearby`: the same hunk has
/// human-written segments.
pub fn for_segment(
    seg: &Segment,
    verdict: &Verdict,
    human_nearby: bool,
    timeline: &Timeline,
    config: &Config,
) -> Vec<Signal> {
    let mut out = Vec::new();
    let mut edits: Vec<&Placed> = seg
        .edits
        .iter()
        .filter_map(|i| timeline.events.get(*i))
        .collect();
    edits.sort_by_key(|p| p.order);
    let by_agent = !edits.is_empty();
    let is_test = config.is_test_file(&seg.file);

    if by_agent
        && is_test
        && let Some(detail) = test_changed_after_failure(&edits, verdict, timeline)
    {
        out.push(Signal {
            kind: SignalKind::TestChangedAfterFailure,
            detail,
        });
    }
    if by_agent && is_test {
        let count = |lines: &[String]| {
            lines
                .iter()
                .filter(|l| ASSERTION.as_ref().is_some_and(|re| re.is_match(l)))
                .count()
        };
        let (removed, added) = (count(&seg.removed), count(&seg.added));
        if removed > added {
            let n = removed - added;
            out.push(Signal {
                kind: SignalKind::AssertionsRemoved,
                detail: format!(
                    "removes {n} {}",
                    if n == 1 { "assertion" } else { "assertions" }
                ),
            });
        }
    }
    if (by_agent || verdict.status == Status::Unrecorded)
        && config.risk_for(&seg.file) == Risk::High
    {
        out.push(Signal {
            kind: SignalKind::HighRiskArea,
            detail: format!("in high-risk area `{}`", config.area_for(&seg.file)),
        });
    }
    if let Some(last) = edits.last()
        && blind(&seg.file, last, timeline)
    {
        out.push(Signal {
            kind: SignalKind::BlindEdit,
            detail: "edited without reading the file first".to_owned(),
        });
    }
    if by_agent && let Some(detail) = thrash(seg, &edits, timeline) {
        out.push(Signal {
            kind: SignalKind::Thrash,
            detail,
        });
    }
    if verdict.status == Status::Tested && verdict.evidence.run.as_ref().is_some_and(|r| r.filtered)
    {
        out.push(Signal {
            kind: SignalKind::FilteredTestRun,
            detail: "only some tests ran".to_owned(),
        });
    }
    if by_agent && human_nearby {
        out.push(Signal {
            kind: SignalKind::HumanModified,
            detail: "a human changed lines next to it".to_owned(),
        });
    }
    out
}

/// A test file changed between a failing test run and a passing one.
fn test_changed_after_failure(
    edits: &[&Placed],
    verdict: &Verdict,
    timeline: &Timeline,
) -> Option<String> {
    if verdict.status != Status::Tested {
        return None;
    }
    let last = edits.last()?;
    // The latest test run before the last edit, if it failed.
    let before = timeline.events[..last.order]
        .iter()
        .rev()
        .find(|p| is_test_run(p))?;
    (before.event.run_outcome() == Some(RunOutcome::Failed)).then(|| {
        format!(
            "changed after `{}` failed",
            command_text(before).unwrap_or("a test run")
        )
    })
}

fn is_test_run(p: &Placed) -> bool {
    matches!(
        p.event.kind,
        EventKind::Command {
            classification: CommandClass::Test,
            ..
        }
    ) && p.event.run_outcome().is_some()
}

fn command_text(p: &Placed) -> Option<&str> {
    match &p.event.kind {
        EventKind::Command { command, .. } => Some(command.split('|').next()?.trim()),
        _ => None,
    }
}

/// Whether the agent edited `file` in `last`'s session without first
/// reading it (a `Read`, or a command naming the file), creating it, or
/// having edited it before. Only edits made with edit tools count: files
/// written by commands and scripts aren't expected to be read first.
fn blind(file: &str, last: &Placed, timeline: &Timeline) -> bool {
    let session = &last.event.session_id;
    let mut first_edit: Option<&Placed> = None;
    for p in &timeline.events {
        if p.event.session_id != *session || p.file() != Some(file) {
            continue;
        }
        if let EventKind::Edit { via, .. } = &p.event.kind {
            first_edit = Some(p);
            if matches!(
                via,
                Some(EditVia::Snapshot | EditVia::Content | EditVia::Reported)
            ) {
                return false;
            }
            break;
        }
    }
    let Some(first) = first_edit else {
        return false;
    };
    let base = file.rsplit('/').next().unwrap_or(file);
    !timeline.events[..first.order].iter().any(|p| {
        p.event.session_id == *session
            && match &p.event.kind {
                EventKind::Read { file: f } => f == file,
                EventKind::Command { command, .. } => {
                    command.contains(file) || (base.len() >= 4 && command.contains(base))
                }
                _ => false,
            }
    })
}

/// Many edits to the same lines, or many failing runs while writing them.
fn thrash(seg: &Segment, edits: &[&Placed], timeline: &Timeline) -> Option<String> {
    let last = edits.last()?;
    let touching: Vec<&Placed> = timeline.events[..=last.order]
        .iter()
        .filter(|p| {
            matches!(p.event.kind, EventKind::Edit { .. })
                && p.file() == Some(seg.file.as_str())
                && spans_overlap(p.span, seg.span)
        })
        .collect();
    if touching.len() >= THRASH_EDITS {
        return Some(format!("edited {} times", touching.len()));
    }
    let first = touching.first().map_or(last.order, |p| p.order);
    let failures = timeline.events[first..=last.order]
        .iter()
        .filter(|p| {
            matches!(
                p.event.kind,
                EventKind::Command { classification, .. } if classification.verifies()
            ) && p.event.run_outcome() == Some(RunOutcome::Failed)
        })
        .count();
    (failures >= THRASH_FAILURES)
        .then(|| format!("checks failed {failures} times while it was written"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{Classifier, ClassifyContext, DefaultRules};
    use crate::hunk::Hunk;
    use crate::record::{Change, Event, line_hashes};
    use crate::scope::NoPackages;
    use time::Duration;
    use time::macros::datetime;

    struct Log(Vec<Event>);

    impl Log {
        fn new() -> Self {
            Log(Vec::new())
        }
        fn push(mut self, session: &str, kind: EventKind) -> Self {
            let secs = i64::try_from(self.0.len()).unwrap();
            self.0.push(Event::new(
                session,
                "claude-code",
                datetime!(2026-10-20 00:00:00 UTC) + Duration::seconds(secs),
                kind,
            ));
            self
        }
        fn read(self, file: &str) -> Self {
            self.push("s", EventKind::Read { file: file.into() })
        }
        fn edit(self, file: &str, start: u32, lines: &[&str], via: EditVia) -> Self {
            let change = Change {
                start,
                removed: 0,
                added: u32::try_from(lines.len()).unwrap(),
            };
            self.push(
                "s",
                EventKind::Edit {
                    file: file.into(),
                    lines: Some(change.new_range()),
                    change: Some(change),
                    tool: "Edit".into(),
                    via: Some(via),
                    line_hashes: Some(line_hashes(lines)),
                    removed_hashes: None,
                },
            )
        }
        /// Replaces line `start` with `line`.
        fn rewrite(self, file: &str, start: u32, line: &str) -> Self {
            let change = Change {
                start,
                removed: 1,
                added: 1,
            };
            self.push(
                "s",
                EventKind::Edit {
                    file: file.into(),
                    lines: Some(change.new_range()),
                    change: Some(change),
                    tool: "Edit".into(),
                    via: Some(EditVia::Patch),
                    line_hashes: Some(line_hashes(&[line])),
                    removed_hashes: None,
                },
            )
        }
        fn test(self, cmd: &str, exit: i32) -> Self {
            self.push(
                "s",
                EventKind::Command {
                    classification: CommandClass::Test,
                    exit_code: Some(exit),
                    command: cmd.into(),
                    outcome: None,
                    tests: None,
                    scope: None,
                },
            )
        }
        fn signals(self, hunk: &Hunk, config: &Config) -> Vec<SignalKind> {
            let timeline = Timeline::build(vec![self.0]);
            let segs = crate::provenance::attribute(std::slice::from_ref(hunk), &timeline);
            let ctx = ClassifyContext {
                timeline: &timeline,
                config,
                packages: &NoPackages,
            };
            let verdict = DefaultRules.classify(&segs[0], &ctx);
            for_segment(&segs[0], &verdict, false, &timeline, config)
                .into_iter()
                .map(|s| s.kind)
                .collect()
        }
    }

    fn hunk(file: &str, start: u32, added: &[&str], removed: &[&str]) -> Hunk {
        Hunk {
            file: file.into(),
            old_start: start,
            old_count: u32::try_from(removed.len()).unwrap(),
            new_start: start,
            new_count: u32::try_from(added.len()).unwrap(),
            added: added.iter().map(|s| (*s).to_owned()).collect(),
            removed: removed.iter().map(|s| (*s).to_owned()).collect(),
            binary: false,
        }
    }

    const TEST_LINES: [&str; 2] = [
        "let parsed = parse(\"2026-10-20\");",
        "assert!(parsed.is_ok());",
    ];

    #[test]
    fn test_changed_after_a_failure() {
        let config = Config::default();
        let signals = Log::new()
            .read("tests/parse.rs")
            .test("cargo test", 101)
            .edit("tests/parse.rs", 3, &TEST_LINES, EditVia::Patch)
            .test("cargo test", 0)
            .signals(&hunk("tests/parse.rs", 3, &TEST_LINES, &[]), &config);
        assert_eq!(signals, vec![SignalKind::TestChangedAfterFailure]);
        // A test written before any failure is fine.
        let signals = Log::new()
            .read("tests/parse.rs")
            .edit("tests/parse.rs", 3, &TEST_LINES, EditVia::Patch)
            .test("cargo test", 0)
            .signals(&hunk("tests/parse.rs", 3, &TEST_LINES, &[]), &config);
        assert!(signals.is_empty(), "{signals:?}");
    }

    #[test]
    fn assertions_removed_from_tests() {
        let config = Config::default();
        let signals = Log::new()
            .read("tests/parse.rs")
            .edit("tests/parse.rs", 3, &TEST_LINES[..1], EditVia::Patch)
            .signals(
                &hunk(
                    "tests/parse.rs",
                    3,
                    &TEST_LINES[..1],
                    &[
                        "assert_eq!(parsed.unwrap().year(), 2026);",
                        "assert!(parsed.is_ok());",
                    ],
                ),
                &config,
            );
        assert_eq!(signals, vec![SignalKind::AssertionsRemoved]);
    }

    #[test]
    fn blind_edits_and_reads_by_command() {
        let config = Config::default();
        let line = ["pub fn handler(request: Request) -> Response {"];
        let blind = Log::new()
            .edit("src/api.rs", 10, &line, EditVia::Preimage)
            .signals(&hunk("src/api.rs", 10, &line, &[]), &config);
        assert_eq!(blind, vec![SignalKind::BlindEdit]);
        // `sed -n` on the file counts as reading it.
        let read = Log::new()
            .push(
                "s",
                EventKind::Command {
                    classification: CommandClass::Other,
                    exit_code: Some(0),
                    command: "sed -n 1,40p src/api.rs".into(),
                    outcome: None,
                    tests: None,
                    scope: None,
                },
            )
            .edit("src/api.rs", 10, &line, EditVia::Preimage)
            .signals(&hunk("src/api.rs", 10, &line, &[]), &config);
        assert!(read.is_empty(), "{read:?}");
        // Files written by commands aren't expected to be read first.
        let generated = Log::new()
            .edit("src/api.rs", 10, &line, EditVia::Snapshot)
            .signals(&hunk("src/api.rs", 10, &line, &[]), &config);
        assert!(generated.is_empty(), "{generated:?}");
    }

    #[test]
    fn thrash_and_risk() {
        let config = Config::parse(
            "[[areas]]\nname = \"auth\"\npaths = [\"src/auth/**\"]\nrisk = \"high\"\n",
        )
        .unwrap();
        let line = ["let token = issue_token(&claims)?;"];
        let mut log = Log::new().read("src/auth/token.rs");
        for _ in 0..4 {
            log = log.rewrite("src/auth/token.rs", 7, line[0]);
        }
        let signals = log.signals(&hunk("src/auth/token.rs", 7, &line, &[]), &config);
        assert_eq!(signals, vec![SignalKind::HighRiskArea, SignalKind::Thrash]);
    }
}
