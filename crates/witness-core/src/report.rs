//! Builds the review plan and renders it as markdown or JSON.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::classify::{Classifier, Status, Verdict};
use crate::config::Config;
use crate::history::{HistoryStore, HunkFingerprint};
use crate::hunk::Hunk;
use crate::provenance::{self, Segment};
use crate::record::Event;
use crate::timeline::Timeline;

/// Hidden marker on the first line of the markdown report. Platform adapters
/// look for it to update their previous comment instead of adding one.
pub const REPORT_MARKER: &str = "<!-- witness:report -->";

/// Version of the JSON report shape.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub base: String,
    pub head: String,
    pub summary: Summary,
    /// Sections to look at closely, highest priority first, up to the cap.
    pub flagged: Vec<Section>,
    /// Flagged sections beyond `report.max_flagged`.
    pub over_cap: Vec<Section>,
    /// Sections that were checked.
    pub skim: Vec<Section>,
    pub track_record: TrackRecordNote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// Agent-written sections (everything except `human`).
    pub sections: usize,
    /// Sections listed for close review (`flagged.len()`).
    pub review_closely: usize,
    pub guessed: usize,
    pub unverified: usize,
    /// From commits attributed to an agent, with no Witness record.
    #[serde(default)]
    pub unrecorded: usize,
    pub checked: usize,
    /// Sections with no agent edit, not counted in `sections`.
    pub human: usize,
    pub sessions: usize,
    /// Session log lines that could not be read.
    pub skipped_lines: usize,
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
    /// For lines no recorded agent edit wrote: the agent their commit is
    /// attributed to (e.g. by a `Co-authored-by` trailer), if any.
    pub commit_agent: &'a dyn Fn(&Segment) -> Option<String>,
}

/// A [`ReportInput::commit_agent`] that attributes nothing.
pub fn no_commit_agent(_: &Segment) -> Option<String> {
    None
}

pub fn build(input: ReportInput<'_>) -> Report {
    let session_count = input.sessions.len();
    let timeline = Timeline::build(input.sessions);
    let segments = provenance::attribute(input.hunks, &timeline);

    let mut flagged = Vec::new();
    let mut skim = Vec::new();
    let mut summary = Summary {
        sections: 0,
        review_closely: 0,
        guessed: 0,
        unverified: 0,
        unrecorded: 0,
        checked: 0,
        human: 0,
        sessions: session_count,
        skipped_lines: input.skipped_lines,
    };
    for segment in &segments {
        let mut verdict = input.classifier.classify(segment, &timeline);
        if verdict.status == Status::Human
            && let Some(agent) = (input.commit_agent)(segment)
        {
            verdict = Verdict {
                status: Status::Unrecorded,
                reason: format!(
                    "Committed as written by {agent}, but no Witness record covers these lines"
                ),
                agent: Some(agent),
            };
        }
        match verdict.status {
            Status::Human => {
                summary.human += 1;
                continue;
            }
            Status::Guessed => summary.guessed += 1,
            Status::Unverified => summary.unverified += 1,
            Status::Unrecorded => summary.unrecorded += 1,
            Status::Checked => summary.checked += 1,
        }
        summary.sections += 1;
        let (start, end) = segment.bounds();
        let section = Section {
            file: segment.file.clone(),
            lines: segment.display_lines(),
            start,
            end,
            status: verdict.status,
            reason: verdict.reason,
            agent: verdict.agent,
            area: input.config.area_for(&segment.file).to_owned(),
            fingerprint: HunkFingerprint::new(&segment.file, &segment.added),
        };
        if section.status.is_flagged() {
            flagged.push(section);
        } else {
            skim.push(section);
        }
    }

    let position = |s: &Section| (s.file.clone(), s.start.unwrap_or(0));
    flagged.sort_by_key(|s| (s.status, position(s)));
    skim.sort_by_key(position);
    let over_cap = if flagged.len() > input.config.max_flagged() {
        flagged.split_off(input.config.max_flagged())
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
                "**Review 0 of {} {} closely: every agent-written section was checked.**",
                s.sections,
                plural(s.sections, "section", "sections")
            );
        } else if s.review_closely == s.sections {
            let _ = writeln!(
                out,
                "**Review {} of {} {} closely.**",
                s.review_closely,
                s.sections,
                plural(s.sections, "section", "sections")
            );
        } else {
            let _ = writeln!(
                out,
                "**Review {} of {} {} closely, skim the rest.**",
                s.review_closely,
                s.sections,
                plural(s.sections, "section", "sections")
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
                    cell(&sec.reason)
                );
            }
        }

        if !self.over_cap.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "<details>");
            let _ = writeln!(
                out,
                "<summary>{} more {} also lacked verification (over the cap of {})</summary>",
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

fn write_list(out: &mut String, sections: &[Section]) {
    for sec in sections {
        let _ = writeln!(
            out,
            "- `{}` {}: {}, {}",
            code(&sec.file),
            sec.lines,
            sec.status.as_str(),
            lowercase_first(&sec.reason)
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
    use crate::classify::V0Rules;
    use crate::history::NullHistoryStore;
    use crate::record::{Change, CommandClass, EventKind, LineRange};
    use time::Duration;
    use time::macros::datetime;

    fn hunk(file: &str, new_start: u32, new_count: u32) -> Hunk {
        Hunk {
            file: file.into(),
            old_start: new_start,
            old_count: 0,
            new_start,
            new_count,
            added: vec!["x".into()],
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

    fn edit(file: &str, start: u32, added: u32) -> EventKind {
        let change = Change {
            start,
            removed: 0,
            added,
        };
        EventKind::Edit {
            file: file.into(),
            lines: Some(change.new_range()),
            change: Some(change),
            tool: "Edit".into(),
            via: None,
            line_hashes: None,
            removed_hashes: None,
        }
    }

    fn sample(max_flagged: usize) -> Report {
        let hunks = vec![
            hunk("src/a.rs", 1, 3),
            hunk("src/a.rs", 20, 2),
            hunk("src/b.rs", 5, 1),
            hunk("src/c.rs", 1, 1),
        ];
        let sessions = vec![vec![
            event(0, edit("src/a.rs", 1, 3)),
            event(1, edit("src/b.rs", 5, 1)),
            event(
                2,
                EventKind::Command {
                    classification: CommandClass::Test,
                    exit_code: Some(0),
                    command: "cargo test".into(),
                },
            ),
            event(3, edit("src/a.rs", 20, 2)),
            event(
                4,
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
            classifier: &V0Rules,
            history: &NullHistoryStore,
            commit_agent: &no_commit_agent,
        })
    }

    #[test]
    fn counts_and_orders_sections() {
        let report = sample(10);
        assert_eq!(report.summary.sections, 3);
        assert_eq!(report.summary.human, 1);
        assert_eq!(report.summary.review_closely, 2);
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
        assert_eq!(report.skim.len(), 1);
        assert_eq!(report.track_record.message, "No track record yet.");
    }

    #[test]
    fn markdown_starts_with_marker_and_escapes_cells() {
        let md = sample(10).to_markdown();
        assert!(md.starts_with(REPORT_MARKER));
        assert!(md.contains("**Review 2 of 3 sections closely, skim the rest.**"));
        assert!(md.contains("Assumed: the \\| separator is never escaped"));
        assert!(md.contains("<summary>1 section to skim</summary>"));
    }

    #[test]
    fn cap_moves_extra_flags_out_of_the_table() {
        let report = sample(1);
        assert_eq!(report.summary.review_closely, 1);
        assert_eq!(report.over_cap.len(), 1);
        assert_eq!(report.flagged[0].status, Status::Guessed);
        let md = report.to_markdown();
        assert!(md.contains("**Review 1 of 3 sections closely"));
        assert!(md.contains("1 more section also lacked verification"));
    }

    #[test]
    fn json_has_same_data() {
        let report = sample(10);
        let json = report.to_json().unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["flagged"][0]["status"], "guessed");
        assert_eq!(v["summary"]["sections"], 3);
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
            classifier: &V0Rules,
            history: &NullHistoryStore,
            commit_agent: &no_commit_agent,
        });
        assert!(report.to_markdown().contains("No agent-written sections"));
    }
}
