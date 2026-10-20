//! Builds the review plan. Renderers: [`Report::to_markdown`],
//! [`Report::to_text`], [`Report::to_sarif`],
//! [`Report::to_github_annotations`], [`Report::to_gitlab_codequality`]
//! and JSON.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::claims::Claim;
use crate::classify::{Classifier, ClassifyContext, Evidence, Status, Verdict};
use crate::config::Config;
use crate::history::{HistoryStore, HunkFingerprint};
use crate::hunk::Hunk;
use crate::provenance::{self, Segment};
use crate::record::{CommandClass, Event, EventKind};
use crate::scope::Packages;
use crate::signals::{self, Signal, SignalKind};
use crate::testsum::{RunOutcome, TestCounts};
use crate::timeline::Timeline;

mod annotations;
pub mod explain;
mod markdown;
mod sarif;
mod text;

pub use markdown::MarkdownOptions;
pub use text::TextOptions;

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
    /// The head commit, when known; section links point at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// Directory of `.witness/` relative to the repository root: empty, or
    /// `/`-terminated. Section paths are relative to it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    pub summary: Summary,
    /// Sections to look at closely, highest priority first, up to the cap.
    pub flagged: Vec<Section>,
    /// Sections for close review beyond `report.max_flagged`.
    pub over_cap: Vec<Section>,
    /// Sections to skim, by path.
    pub skim: Vec<Section>,
    /// The agents' test and build runs on this branch, oldest first (the
    /// latest [`MAX_RUNS`]).
    #[serde(default)]
    pub runs: Vec<Run>,
    /// Who did the work.
    #[serde(default)]
    pub agents: Vec<AgentSummary>,
    /// Claims agents made at the end of a turn that the record contradicted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contradicted_claims: Vec<ClaimNote>,
    pub track_record: TrackRecordNote,
}

/// An agent's claim at the end of a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimNote {
    pub agent: String,
    pub claim: Claim,
    #[serde(with = "crate::record::timestamp")]
    pub at: OffsetDateTime,
}

/// Runs kept in [`Report::runs`].
pub const MAX_RUNS: usize = 20;

/// A test or build run by an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub agent: String,
    pub command: String,
    pub class: CommandClass,
    /// Absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RunOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestCounts>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub filtered: bool,
    #[serde(with = "crate::record::timestamp")]
    pub at: OffsetDateTime,
}

/// One agent's activity on the branch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub agent: String,
    pub sessions: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Distinct turns (user prompts), when the agent reports them.
    pub turns: usize,
    pub edits: usize,
    pub runs: usize,
    pub assumptions: usize,
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
    /// Link to the lines on the code host, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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
    /// See [`Report::prefix`].
    pub prefix: &'a str,
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

/// Everything decided about a change, before it is arranged into a report.
pub struct Analysis {
    pub timeline: Timeline,
    pub segments: Vec<Segment>,
    /// For each segment, its section (`None` for human-written ones) and
    /// whether it is for close review.
    pub sections: Vec<Option<(Section, bool)>>,
    pub summary: Summary,
}

/// Attributes, classifies and scores every segment of the change.
pub fn analyze(input: ReportInput<'_>) -> Analysis {
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

    let mut summary = Summary {
        sessions: session_count,
        skipped_lines: input.skipped_lines,
        ..Summary::default()
    };
    let mut sections = Vec::with_capacity(segments.len());
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
                sections.push(None);
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
            url: None,
            fingerprint: HunkFingerprint::new(&segment.file, &segment.added),
        };
        sections.push(Some((section, close)));
    }
    Analysis {
        timeline,
        segments,
        sections,
        summary,
    }
}

pub fn build(input: ReportInput<'_>) -> Report {
    let (config, history) = (input.config, input.history);
    let (base, head, prefix) = (input.base, input.head, input.prefix);
    let analysis = analyze(input);
    let mut summary = analysis.summary;
    let mut flagged = Vec::new();
    let mut skim = Vec::new();
    for (section, close) in analysis.sections.into_iter().flatten() {
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
            .is_some_and(|agent| matches!(history.track_record(agent, &s.area), Ok(Some(_))))
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
        base: base.to_owned(),
        head: head.to_owned(),
        head_sha: None,
        prefix: prefix.to_owned(),
        summary,
        flagged,
        over_cap,
        skim,
        runs: runs(&analysis.timeline),
        agents: agents(&analysis.timeline),
        contradicted_claims: analysis
            .timeline
            .events
            .iter()
            .filter_map(|p| match &p.event.kind {
                EventKind::Claim {
                    claim,
                    supported: false,
                } => Some(ClaimNote {
                    agent: p.event.agent.clone(),
                    claim: *claim,
                    at: p.event.timestamp,
                }),
                _ => None,
            })
            .collect(),
        track_record,
    }
}

fn runs(timeline: &Timeline) -> Vec<Run> {
    let all: Vec<Run> = timeline
        .events
        .iter()
        .filter_map(|p| match &p.event.kind {
            EventKind::Command {
                classification,
                command,
                tests,
                scope,
                ..
            } if classification.verifies() => Some(Run {
                agent: p.event.agent.clone(),
                command: command.clone(),
                class: *classification,
                outcome: p.event.run_outcome(),
                tests: *tests,
                filtered: scope.as_ref().is_some_and(|s| s.filtered),
                at: p.event.timestamp,
            }),
            _ => None,
        })
        .collect();
    let skip = all.len().saturating_sub(MAX_RUNS);
    all.into_iter().skip(skip).collect()
}

fn agents(timeline: &Timeline) -> Vec<AgentSummary> {
    let mut by_agent: BTreeMap<&str, (AgentSummary, HashSet<&str>, HashSet<&str>)> =
        BTreeMap::new();
    for p in &timeline.events {
        let e = &p.event;
        let (summary, sessions, turns) = by_agent.entry(e.agent.as_str()).or_insert_with(|| {
            (
                AgentSummary {
                    agent: e.agent.clone(),
                    ..AgentSummary::default()
                },
                HashSet::new(),
                HashSet::new(),
            )
        });
        sessions.insert(e.session_id.as_str());
        if let Some(turn) = &e.turn {
            turns.insert(turn.as_str());
        }
        if let Some(model) = &e.model
            && !summary.models.contains(model)
        {
            summary.models.push(model.clone());
        }
        match &e.kind {
            EventKind::Edit { .. } => summary.edits += 1,
            EventKind::Command { classification, .. } if classification.verifies() => {
                summary.runs += 1;
            }
            EventKind::Assumption { .. } => summary.assumptions += 1,
            _ => {}
        }
    }
    by_agent
        .into_values()
        .map(|(mut summary, sessions, turns)| {
            summary.sessions = sessions.len();
            summary.turns = turns.len();
            summary
        })
        .collect()
}

impl Report {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Every section, close-review ones first.
    pub fn sections(&self) -> impl Iterator<Item = &Section> {
        self.flagged.iter().chain(&self.over_cap).chain(&self.skim)
    }

    /// Sections for close review, including those over the cap.
    pub fn close_review(&self) -> impl Iterator<Item = &Section> {
        self.flagged.iter().chain(&self.over_cap)
    }

    /// Path of a section's file relative to the repository root.
    pub fn repo_path(&self, file: &str) -> String {
        format!("{}{file}", self.prefix)
    }

    /// Fills in each section's link to its lines at head.
    pub fn set_links(&mut self, links: &Links) {
        let prefix = self.prefix.clone();
        for section in self
            .flagged
            .iter_mut()
            .chain(self.over_cap.iter_mut())
            .chain(self.skim.iter_mut())
        {
            section.url = Some(links.url(&prefix, section));
        }
        self.head_sha = Some(links.sha.clone());
    }

    /// Whether any section has one of `statuses` or `signals`.
    pub fn has_any(&self, statuses: &[Status], signals: &[SignalKind]) -> bool {
        self.sections().any(|s| {
            statuses.contains(&s.status) || s.signals.iter().any(|g| signals.contains(&g.kind))
        })
    }
}

/// How to link to a file's lines on the code host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Links {
    pub style: LinkStyle,
    /// e.g. `https://github.com/acme/app`.
    pub base: String,
    /// Commit to link to.
    pub sha: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStyle {
    GitHub,
    GitLab,
}

impl Links {
    /// Link to a section's lines.
    pub fn url(&self, prefix: &str, section: &Section) -> String {
        let base = self.base.trim_end_matches('/');
        let path = format!("{prefix}{}", section.file)
            .split('/')
            .map(encode_path_segment)
            .collect::<Vec<_>>()
            .join("/");
        let anchor = match (section.start, section.end, self.style) {
            (Some(a), Some(b), LinkStyle::GitHub) if a != b => format!("#L{a}-L{b}"),
            (Some(a), Some(b), LinkStyle::GitLab) if a != b => format!("#L{a}-{b}"),
            (Some(a), _, _) => format!("#L{a}"),
            _ => String::new(),
        };
        match self.style {
            LinkStyle::GitHub => format!("{base}/blob/{}/{path}{anchor}", self.sha),
            LinkStyle::GitLab => format!("{base}/-/blob/{}/{path}{anchor}", self.sha),
        }
    }
}

fn encode_path_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::DefaultRules;
    use crate::history::NullHistoryStore;
    use crate::record::{Change, LineRange};
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
            prefix: "",
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
    fn links_point_at_lines_on_the_host() {
        let mut report = sample(10);
        report.prefix = "pkg/".into();
        report.set_links(&Links {
            style: LinkStyle::GitHub,
            base: "https://github.com/acme/app".into(),
            sha: "abc123".into(),
        });
        let a = report.skim.iter().find(|s| s.file == "src/a.rs").unwrap();
        assert_eq!(
            a.url.as_deref(),
            Some("https://github.com/acme/app/blob/abc123/pkg/src/a.rs#L1-L3")
        );
        let md = report.to_markdown();
        assert!(
            md.contains("[5](https://github.com/acme/app/blob/abc123/pkg/src/b.rs#L5)"),
            "{md}"
        );
        let gitlab = Links {
            style: LinkStyle::GitLab,
            base: "https://gitlab.com/acme/app/".into(),
            sha: "abc".into(),
        };
        assert_eq!(
            gitlab.url("", a),
            "https://gitlab.com/acme/app/-/blob/abc/src/a.rs#L1-3"
        );
    }

    #[test]
    fn markdown_has_coverage_runs_and_agents() {
        let md = sample(10).to_markdown();
        assert!(
            md.contains("Agents wrote 7 lines: 43% tested, 14% only compiled, 43% unchecked."),
            "{md}"
        );
        assert!(
            md.contains("Agent checks: 2 test and build runs, last passed"),
            "{md}"
        );
        assert!(md.contains("| `cargo test` | passed |"), "{md}");
        assert!(md.contains("<sub>claude-code (1 session)"), "{md}");
    }

    #[test]
    fn markdown_fits_the_size_limit() {
        let mut report = sample(10);
        let many = report.skim[0].clone();
        report.skim = (0..5000)
            .map(|i| Section {
                file: format!("src/generated/file_{i}.rs"),
                ..many.clone()
            })
            .collect();
        let md = report.to_markdown_with(MarkdownOptions { max_chars: 20_000 });
        assert!(md.chars().count() <= 20_000);
        assert!(md.contains("… and"), "lists are shortened");
        assert!(md.starts_with(REPORT_MARKER));
    }

    #[test]
    fn text_sarif_and_annotations() {
        let report = sample(10);
        let text = report.to_text(TextOptions {
            color: false,
            all: true,
        });
        assert!(text.starts_with("Witness: review 2 of 4 sections closely (main...HEAD)"));
        assert!(text.contains("GUESSED    src/b.rs:5"), "{text}");
        assert!(text.contains("To skim:"));
        assert!(!text.contains('\x1b'));

        let sarif = report.to_sarif("0.2.0");
        let results = sarif["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["ruleId"], "witness/guessed");
        assert_eq!(results[0]["level"], "warning");
        assert_eq!(
            results[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            5
        );

        let annotations = report.to_github_annotations();
        let first = annotations.lines().next().unwrap();
        assert!(
            first
                .starts_with("::warning file=src/b.rs,line=5,endLine=5,title=Witness%3A guessed::"),
            "{first}"
        );
        let gitlab = report.to_gitlab_codequality();
        assert_eq!(gitlab[0]["location"]["path"], "src/b.rs");
        assert_eq!(gitlab[0]["severity"], "major");
    }

    #[test]
    fn empty_change() {
        let config = Config::default();
        let report = build(ReportInput {
            base: "main",
            head: "HEAD",
            prefix: "",
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
