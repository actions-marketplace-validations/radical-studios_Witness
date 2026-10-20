//! Builds the review plan and renders it as markdown or JSON.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::classify::{Classifier, ClassifyContext, Evidence, Status, Verdict};
use crate::config::Config;
use crate::history::{HistoryStore, HunkFingerprint};
use crate::hunk::Hunk;
use crate::provenance::{self, Segment};
use crate::record::Event;
use crate::scope::Packages;
use crate::signals::{self, Signal};
use crate::timeline::Timeline;

/// Hidden marker on the first line of the markdown report. Platform adapters
/// look for it to update their previous comment instead of adding one.
pub const REPORT_MARKER: &str = "<!-- witness:report -->";

/// Version of the JSON report shape.
pub const REPORT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub base: String,
    pub head: String,
    pub summary: Summary,
    /// Sections to look at closely, highest priority first, up to the cap.
    pub flagged: Vec<Section>,
    /// Sections for close review beyond `report.max_flagged`.
    pub over_cap: Vec<Section>,
    /// Sections to skim, by path.
    pub skim: Vec<Section>,
    pub track_record: TrackRecordNote,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// Agent-written sections (everything except `human`).
    pub sections: usize,
    /// Sections listed for close review (`flagged.len()`).
    pub review_closely: usize,
    pub failing: usize,
    pub guessed: usize,
    /// From commits attributed to an agent, with no Witness record.
    pub unrecorded: usize,
    pub unverified: usize,
    pub compiled: usize,
    pub tested: usize,
    /// Sections with no agent edit, not counted in `sections`.
    pub human: usize,
    pub sessions: usize,
    /// Session log lines that could not be read.
    pub skipped_lines: usize,
    pub lines: LineCounts,
}

/// Agent-written lines added, by how they were checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineCounts {
    pub agent: usize,
    pub tested: usize,
    pub compiled: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub file: String,
    /// Human-readable location at head, e.g. `12-18`.
    pub lines: String,
    /// First line at head; `None` for whole-file (binary) changes.
    pub start: Option<u32>,
    /// Last line at head; `None` for whole-file (binary) changes.
    pub end: Option<u32>,
    pub status: Status,
    pub reason: String,
    pub agent: Option<String>,
    pub area: String,
    #[serde(default)]
    pub signals: Vec<Signal>,
    /// Priority for review; higher first.
    pub score: u32,
    #[serde(default)]
    pub evidence: Evidence,
    /// Lines this section adds.
    pub lines_added: usize,
    pub fingerprint: HunkFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackRecordNote {
    pub available: bool,
    pub message: String,
}

/// Everything the report is computed from.
pub struct ReportInput<'a> {
    pub base: &'a str,
    pub head: &'a str,
    pub hunks: &'a [Hunk],
    /// One event list per session, each in file order.
    pub sessions: Vec<Vec<Event>>,
    pub skipped_lines: usize,
    pub config: &'a Config,
    pub classifier: &'a dyn Classifier,
    pub history: &'a dyn HistoryStore,
    /// Package lookups at head, to tell what a scoped test run covers.
    pub packages: &'a dyn Packages,
    /// For lines no recorded agent edit wrote: the agent their commit is
    /// attributed to (e.g. by a `Co-authored-by` trailer), if any.
    pub commit_agent: &'a dyn Fn(&Segment) -> Option<String>,
}

/// A [`ReportInput::commit_agent`] that attributes nothing.
pub fn no_commit_agent(_: &Segment) -> Option<String> {
    None
}

/// Score added by a signal that escalates to close review, and by others.
const ESCALATING_SIGNAL_SCORE: u32 = 20;
const SIGNAL_SCORE: u32 = 5;

pub fn build(input: ReportInput<'_>) -> Report {
    let config = input.config;
    let session_count = input.sessions.len();
    let timeline = Timeline::build(input.sessions);
    let segments = provenance::attribute(input.hunks, &timeline);
    let ctx = ClassifyContext {
        timeline: &timeline,
        config,
        packages: input.packages,
    };
    let human_hunks: HashSet<usize> = segments
        .iter()
        .filter(|s| !s.by_agent() && s.assumptions.is_empty())
        .map(|s| s.hunk)
        .collect();

    let mut flagged = Vec::new();
    let mut skim = Vec::new();
    let mut summary = Summary {
        sessions: session_count,
        skipped_lines: input.skipped_lines,
        ..Summary::default()
    };
    for segment in &segments {
        let mut verdict = input.classifier.classify(segment, &ctx);
        if verdict.status == Status::Human
            && let Some(agent) = (input.commit_agent)(segment)
        {
            verdict = Verdict {
                status: Status::Unrecorded,
                reason: format!(
                    "Committed as written by {agent}, but no Witness record covers these lines"
                ),
                agent: Some(agent),
                evidence: Evidence::default(),
            };
        }
        let lines_added = segment.added.len();
        match verdict.status {
            Status::Human => {
                summary.human += 1;
                continue;
            }
            Status::Failing => summary.failing += 1,
            Status::Guessed => summary.guessed += 1,
            Status::Unrecorded => summary.unrecorded += 1,
            Status::Unverified => summary.unverified += 1,
            Status::Compiled => {
                summary.compiled += 1;
                summary.lines.compiled += lines_added;
            }
            Status::Tested => {
                summary.tested += 1;
                summary.lines.tested += lines_added;
            }
        }
        summary.sections += 1;
        summary.lines.agent += lines_added;

        let human_nearby = segment.by_agent() && human_hunks.contains(&segment.hunk);
        let signals = signals::for_segment(segment, &verdict, human_nearby, &timeline, config);
        let close =
            config.closes(verdict.status) || signals.iter().any(|s| config.escalates(s.kind));
        let score = verdict.status.weight()
            + signals
                .iter()
                .map(|s| {
                    if config.escalates(s.kind) {
                        ESCALATING_SIGNAL_SCORE
                    } else {
                        SIGNAL_SCORE
                    }
                })
                .sum::<u32>()
            + u32::try_from((lines_added / 5).min(10)).unwrap_or(10);
        let (start, end) = segment.bounds();
        let section = Section {
            file: segment.file.clone(),
            lines: segment.display_lines(),
            start,
            end,
            status: verdict.status,
            reason: verdict.reason,
            agent: verdict.agent,
            area: config.area_for(&segment.file).to_owned(),
            signals,
            score,
            evidence: verdict.evidence,
            lines_added,
            fingerprint: HunkFingerprint::new(&segment.file, &segment.added),
        };
        if close {
            flagged.push(section);
        } else {
            skim.push(section);
        }
    }

    let position = |s: &Section| (s.file.clone(), s.start.unwrap_or(0));
    flagged.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| position(a).cmp(&position(b)))
    });
    skim.sort_by_key(position);
    let over_cap = if flagged.len() > config.max_flagged() {
        flagged.split_off(config.max_flagged())
    } else {
        Vec::new()
    };
    summary.review_closely = flagged.len();

    let any_record = flagged.iter().chain(&skim).any(|s| {
        s.agent
            .as_deref()
            .is_some_and(|agent| matches!(input.history.track_record(agent, &s.area), Ok(Some(_))))
    });
    let track_record = TrackRecordNote {
        available: any_record,
        message: if any_record {
            "Track record available but not yet used in this version.".to_owned()
        } else {
            "No track record yet.".to_owned()
        },
    };

    Report {
        schema_version: REPORT_SCHEMA_VERSION,
        base: input.base.to_owned(),
        head: input.head.to_owned(),
        summary,
        flagged,
        over_cap,
        skim,
        track_record,
    }
}

impl Report {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let s = &self.summary;
        // Writing to a String cannot fail; ignore the fmt::Result.
        let _ = writeln!(out, "{REPORT_MARKER}");
        let _ = writeln!(out, "### Witness review plan");
        let _ = writeln!(out);
        let sections = plural(s.sections, "section", "sections");
        if s.sections == 0 {
            let _ = writeln!(
                out,
                "**No agent-written sections in `{}...{}`.**",
                code(&self.base),
                code(&self.head)
            );
        } else if s.review_closely == 0 {
            let _ = writeln!(
                out,
                "**Review 0 of {} {sections} closely: nothing failed, was guessed or went unchecked.**",
                s.sections
            );
        } else if s.review_closely == s.sections {
            let _ = writeln!(
                out,
                "**Review {} of {} {sections} closely.**",
                s.review_closely, s.sections
            );
        } else {
            let _ = writeln!(
                out,
                "**Review {} of {} {sections} closely, skim the rest.**",
                s.review_closely, s.sections
            );
        }

        if !self.flagged.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "| File | Lines | Status | Why |");
            let _ = writeln!(out, "| --- | --- | --- | --- |");
            for sec in &self.flagged {
                let _ = writeln!(
                    out,
                    "| `{}` | {} | {} | {} |",
                    code(&sec.file),
                    sec.lines,
                    sec.status.as_str(),
                    cell(&why(sec))
                );
            }
        }

        if !self.over_cap.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "<details>");
            let _ = writeln!(
                out,
                "<summary>{} more {} to review closely (over the cap of {})</summary>",
                self.over_cap.len(),
                plural(self.over_cap.len(), "section", "sections"),
                self.flagged.len()
            );
            let _ = writeln!(out);
            write_list(&mut out, &self.over_cap);
            let _ = writeln!(out, "</details>");
        }

        if !self.skim.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "<details>");
            let _ = writeln!(
                out,
                "<summary>{} {} to skim</summary>",
                self.skim.len(),
                plural(self.skim.len(), "section", "sections")
            );
            let _ = writeln!(out);
            write_list(&mut out, &self.skim);
            let _ = writeln!(out, "</details>");
        }

        let _ = writeln!(out);
        let mut notes = vec![format!("Track record: {}", self.track_record.message)];
        if s.human > 0 {
            notes.push(format!(
                "{} human-written {} not counted",
                s.human,
                plural(s.human, "section", "sections")
            ));
        }
        notes.push(format!(
            "{} agent {}",
            s.sessions,
            plural(s.sessions, "session", "sessions")
        ));
        if s.skipped_lines > 0 {
            notes.push(format!(
                "{} unreadable log {} skipped",
                s.skipped_lines,
                plural(s.skipped_lines, "line", "lines")
            ));
        }
        let _ = writeln!(out, "<sub>{}.</sub>", notes.join(" · "));
        out
    }
}

/// The reason, with signal details appended.
fn why(sec: &Section) -> String {
    if sec.signals.is_empty() {
        return sec.reason.clone();
    }
    let details: Vec<&str> = sec.signals.iter().map(|s| s.detail.as_str()).collect();
    format!("{} ({})", sec.reason, details.join("; "))
}

fn write_list(out: &mut String, sections: &[Section]) {
    for sec in sections {
        let _ = writeln!(
            out,
            "- `{}` {}: {}, {}",
            code(&sec.file),
            sec.lines,
            sec.status.as_str(),
            lowercase_first(&why(sec))
        );
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// Text safe inside a markdown code span.
fn code(text: &str) -> String {
    text.replace('`', "'").replace('|', "\\|")
}

/// Text safe inside a markdown table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::DefaultRules;
    use crate::history::NullHistoryStore;
    use crate::record::{Change, CommandClass, EventKind, LineRange};
    use crate::scope::NoPackages;
    use time::Duration;
    use time::macros::datetime;

    fn hunk(file: &str, new_start: u32, lines: &[&str]) -> Hunk {
        Hunk {
            file: file.into(),
            old_start: new_start,
            old_count: 0,
            new_start,
            new_count: u32::try_from(lines.len()).unwrap(),
            added: lines.iter().map(|s| (*s).to_owned()).collect(),
            removed: Vec::new(),
            binary: false,
        }
    }

    fn event(secs: i64, kind: EventKind) -> Event {
        Event::new(
            "s1",
            "claude-code",
            datetime!(2026-10-20 00:00:00 UTC) + Duration::seconds(secs),
            kind,
        )
    }

    fn edit(file: &str, start: u32, lines: &[&str]) -> EventKind {
        let change = Change {
            start,
            removed: 0,
            added: u32::try_from(lines.len()).unwrap(),
        };
        EventKind::Edit {
            file: file.into(),
            lines: Some(change.new_range()),
            change: Some(change),
            tool: "Edit".into(),
            via: None,
            line_hashes: Some(crate::record::line_hashes(lines)),
            removed_hashes: None,
        }
    }

    fn run(class: CommandClass, cmd: &str, exit: i32) -> EventKind {
        EventKind::Command {
            classification: class,
            exit_code: Some(exit),
            command: cmd.into(),
            outcome: None,
            tests: None,
            scope: None,
        }
    }

    const A1: [&str; 3] = ["fn alpha_one() {", "    compute_alpha()", "}"];
    const A2: [&str; 2] = ["fn alpha_two() -> u8 {", "    second_value() }"];
    const B: [&str; 1] = ["let separator = config.separator();"];
    const C: [&str; 1] = ["pub const HUMAN_WRITTEN: u32 = 1;"];
    const D: [&str; 1] = ["validate_input(&request)?;"];

    fn sample(max_flagged: usize) -> Report {
        let hunks = vec![
            hunk("src/a.rs", 1, &A1),
            hunk("src/a.rs", 20, &A2),
            hunk("src/b.rs", 5, &B),
            hunk("src/c.rs", 1, &C),
            hunk("src/d.rs", 3, &D),
        ];
        let sessions = vec![vec![
            event(0, edit("src/a.rs", 1, &A1)),
            event(1, edit("src/b.rs", 5, &B)),
            event(2, edit("src/d.rs", 3, &D)),
            event(3, run(CommandClass::Test, "cargo test", 0)),
            event(4, edit("src/d.rs", 3, &D)),
            event(5, run(CommandClass::Build, "cargo build", 0)),
            event(6, edit("src/a.rs", 20, &A2)),
            event(
                7,
                EventKind::Assumption {
                    file: "src/b.rs".into(),
                    lines: Some(LineRange::new(5, 5)),
                    text: "the | separator is never escaped".into(),
                    line_hashes: None,
                },
            ),
        ]];
        let config = Config::parse(&format!("[report]\nmax_flagged = {max_flagged}")).unwrap();
        build(ReportInput {
            base: "main",
            head: "HEAD",
            hunks: &hunks,
            sessions,
            skipped_lines: 0,
            config: &config,
            classifier: &DefaultRules,
            history: &NullHistoryStore,
            packages: &NoPackages,
            commit_agent: &no_commit_agent,
        })
    }

    #[test]
    fn counts_and_orders_sections() {
        let report = sample(10);
        let s = &report.summary;
        assert_eq!(s.sections, 4);
        assert_eq!(s.human, 1);
        assert_eq!(
            (s.guessed, s.unverified, s.compiled, s.tested),
            (1, 1, 1, 1)
        );
        assert_eq!(s.review_closely, 2);
        assert_eq!(s.lines.agent, 7);
        assert_eq!(s.lines.tested, 3);
        let flagged: Vec<_> = report
            .flagged
            .iter()
            .map(|s| (s.file.as_str(), s.status))
            .collect();
        assert_eq!(
            flagged,
            vec![
                ("src/b.rs", Status::Guessed),
                ("src/a.rs", Status::Unverified)
            ]
        );
        let skim: Vec<_> = report
            .skim
            .iter()
            .map(|s| (s.file.as_str(), s.status))
            .collect();
        assert_eq!(
            skim,
            vec![("src/a.rs", Status::Tested), ("src/d.rs", Status::Compiled)]
        );
        assert_eq!(report.track_record.message, "No track record yet.");
    }

    #[test]
    fn markdown_starts_with_marker_and_escapes_cells() {
        let md = sample(10).to_markdown();
        assert!(md.starts_with(REPORT_MARKER));
        assert!(md.contains("**Review 2 of 4 sections closely, skim the rest.**"));
        assert!(md.contains("Assumed: the \\| separator is never escaped"));
        assert!(md.contains("<summary>2 sections to skim</summary>"));
        assert!(md.contains("compiled, only built after the last edit: `cargo build` passed"));
    }

    #[test]
    fn cap_moves_extra_flags_out_of_the_table() {
        let report = sample(1);
        assert_eq!(report.summary.review_closely, 1);
        assert_eq!(report.over_cap.len(), 1);
        assert_eq!(report.flagged[0].status, Status::Guessed);
        let md = report.to_markdown();
        assert!(md.contains("**Review 1 of 4 sections closely"));
        assert!(md.contains("1 more section to review closely"));
    }

    #[test]
    fn json_has_same_data() {
        let report = sample(10);
        let json = report.to_json().unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema_version"], 2);
        assert_eq!(v["flagged"][0]["status"], "guessed");
        assert_eq!(v["summary"]["sections"], 4);
        assert_eq!(v["skim"][0]["evidence"]["run"]["command"], "cargo test");
    }

    #[test]
    fn empty_change() {
        let config = Config::default();
        let report = build(ReportInput {
            base: "main",
            head: "HEAD",
            hunks: &[],
            sessions: Vec::new(),
            skipped_lines: 0,
            config: &config,
            classifier: &DefaultRules,
            history: &NullHistoryStore,
            packages: &NoPackages,
            commit_agent: &no_commit_agent,
        });
        assert!(report.to_markdown().contains("No agent-written sections"));
    }
}
