//! The review plan as markdown, for pull/merge request comments.

use std::fmt::Write as _;

use super::{REPORT_MARKER, Report, Run, Section};
use crate::testsum::RunOutcome;

/// GitHub's limit on a comment body, in characters.
pub const GITHUB_COMMENT_LIMIT: usize = 65_536;

#[derive(Debug, Clone, Copy)]
pub struct MarkdownOptions {
    /// Longest output allowed, in characters; lists are shortened to fit.
    pub max_chars: usize,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            max_chars: GITHUB_COMMENT_LIMIT,
        }
    }
}

/// How much detail a rendering includes; lower levels fit in less space.
#[derive(Clone, Copy)]
struct Detail {
    /// Rows of the close-review table.
    table_rows: usize,
    /// Entries of each list (over-cap, skim).
    list_entries: usize,
    runs: bool,
}

const LEVELS: [Detail; 5] = [
    Detail {
        table_rows: usize::MAX,
        list_entries: usize::MAX,
        runs: true,
    },
    Detail {
        table_rows: usize::MAX,
        list_entries: 200,
        runs: true,
    },
    Detail {
        table_rows: usize::MAX,
        list_entries: 50,
        runs: false,
    },
    Detail {
        table_rows: 50,
        list_entries: 10,
        runs: false,
    },
    Detail {
        table_rows: 10,
        list_entries: 0,
        runs: false,
    },
];

impl Report {
    /// Markdown for a comment, within GitHub's size limit.
    pub fn to_markdown(&self) -> String {
        self.to_markdown_with(MarkdownOptions::default())
    }

    pub fn to_markdown_with(&self, options: MarkdownOptions) -> String {
        let mut out = String::new();
        for level in LEVELS {
            out = render(self, level);
            if out.chars().count() <= options.max_chars {
                return out;
            }
        }
        // Still too long (absurdly long file names): cut at a line.
        let mut cut: String = out
            .chars()
            .take(options.max_chars.saturating_sub(40))
            .collect();
        if let Some(idx) = cut.rfind('\n') {
            cut.truncate(idx + 1);
        }
        cut.push_str("\n_Report truncated to fit._\n");
        cut
    }
}

fn render(report: &Report, detail: Detail) -> String {
    let mut out = String::new();
    let s = &report.summary;
    // Writing to a String cannot fail; ignore the fmt::Result.
    let _ = writeln!(out, "{REPORT_MARKER}");
    let _ = writeln!(out, "### Witness review plan");
    let _ = writeln!(out);
    let sections = plural(s.sections, "section", "sections");
    if s.sections == 0 {
        let _ = writeln!(
            out,
            "**No agent-written sections in `{}...{}`.**",
            code(&report.base),
            code(&report.head)
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
    if let Some(line) = coverage_line(report) {
        let _ = writeln!(out);
        let _ = writeln!(out, "{line}");
    }
    for claim in &report.contradicted_claims {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "⚠ {} ended a turn saying {}, but its record didn't back that up.",
            claim.agent,
            claim.claim.describe()
        );
    }

    if !report.flagged.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "| File | Lines | Status | Why |");
        let _ = writeln!(out, "| --- | --- | --- | --- |");
        for sec in report.flagged.iter().take(detail.table_rows) {
            let lines = match &sec.url {
                Some(url) => format!("[{}]({url})", sec.lines),
                None => sec.lines.clone(),
            };
            let mut why = cell(&sec.reason);
            if !sec.signals.is_empty() {
                let notes: Vec<String> = sec.signals.iter().map(|g| cell(&g.detail)).collect();
                let _ = write!(why, "<br><sub>⚑ {}</sub>", notes.join(" · "));
            }
            let _ = writeln!(
                out,
                "| `{}` | {lines} | {} | {why} |",
                code(&sec.file),
                sec.status.as_str()
            );
        }
        let hidden = report.flagged.len().saturating_sub(detail.table_rows);
        if hidden > 0 {
            let _ = writeln!(
                out,
                "| | | | _{hidden} more {} not shown_ |",
                plural(hidden, "row", "rows")
            );
        }
    }

    if !report.over_cap.is_empty() {
        details(
            &mut out,
            &format!(
                "{} more {} to review closely (over the cap of {})",
                report.over_cap.len(),
                plural(report.over_cap.len(), "section", "sections"),
                report.flagged.len()
            ),
            &report.over_cap,
            detail.list_entries,
        );
    }
    if !report.skim.is_empty() {
        details(
            &mut out,
            &format!(
                "{} {} to skim",
                report.skim.len(),
                plural(report.skim.len(), "section", "sections")
            ),
            &report.skim,
            detail.list_entries,
        );
    }
    if detail.runs && !report.runs.is_empty() {
        runs_table(&mut out, &report.runs);
    }
    track_record(&mut out, report);

    let _ = writeln!(out);
    let mut notes: Vec<String> = report
        .agents
        .iter()
        .map(|a| {
            let mut text = format!(
                "{} ({} {}",
                a.agent,
                a.sessions,
                plural(a.sessions, "session", "sessions")
            );
            if !a.models.is_empty() {
                let _ = write!(text, ", {}", a.models.join(", "));
            }
            text.push(')');
            text
        })
        .collect();
    if notes.is_empty() {
        notes.push("No agent sessions".to_owned());
    }
    if s.human > 0 {
        notes.push(format!(
            "{} human-written {} not counted",
            s.human,
            plural(s.human, "section", "sections")
        ));
    }
    if s.skipped_lines > 0 {
        notes.push(format!(
            "{} unreadable log {} skipped",
            s.skipped_lines,
            plural(s.skipped_lines, "line", "lines")
        ));
    }
    notes.push(format!("Track record: {}", report.track_record.message));
    let _ = writeln!(out, "<sub>{}</sub>", notes.join(" · "));
    out
}

/// `Agents wrote 412 lines: 61% tested, 9% only compiled, 30% unchecked.`
fn coverage_line(report: &Report) -> Option<String> {
    let lines = &report.summary.lines;
    if lines.agent == 0 {
        return None;
    }
    let pct = |n: usize| (n * 100 + lines.agent / 2) / lines.agent;
    let unchecked = lines.agent - lines.tested - lines.compiled;
    let mut parts = vec![format!("{}% tested", pct(lines.tested))];
    if lines.compiled > 0 {
        parts.push(format!("{}% only compiled", pct(lines.compiled)));
    }
    if unchecked > 0 {
        parts.push(format!("{}% unchecked", pct(unchecked)));
    }
    Some(format!(
        "Agents wrote {} {}: {}.",
        lines.agent,
        plural(lines.agent, "line", "lines"),
        parts.join(", ")
    ))
}

fn details(out: &mut String, summary: &str, sections: &[Section], max: usize) {
    let _ = writeln!(out);
    let _ = writeln!(out, "<details>");
    let _ = writeln!(out, "<summary>{summary}</summary>");
    let _ = writeln!(out);
    for sec in sections.iter().take(max) {
        let lines = match &sec.url {
            Some(url) => format!("[{}]({url})", sec.lines),
            None => sec.lines.clone(),
        };
        let mut why = lowercase_first(&sec.reason);
        if !sec.signals.is_empty() {
            let notes: Vec<&str> = sec.signals.iter().map(|g| g.detail.as_str()).collect();
            let _ = write!(why, " (⚑ {})", notes.join("; "));
        }
        let _ = writeln!(
            out,
            "- `{}` {lines}: {}, {}",
            code(&sec.file),
            sec.status.as_str(),
            line_text(&why)
        );
    }
    let hidden = sections.len().saturating_sub(max);
    if hidden > 0 {
        let _ = writeln!(out, "- … and {hidden} more");
    }
    let _ = writeln!(out, "</details>");
}

/// The agents' track record in this report's areas, and how well flags
/// predicted rewrites.
fn track_record(out: &mut String, report: &Report) {
    let t = &report.track_record;
    if t.records.is_empty() && t.calibration.is_none() {
        return;
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "<details>");
    let _ = writeln!(out, "<summary>Track record</summary>");
    let _ = writeln!(out);
    if let Some(calibration) = &t.calibration {
        let _ = writeln!(out, "{calibration}");
        let _ = writeln!(out);
    }
    for record in &t.records {
        let _ = writeln!(out, "- {}", record.describe());
    }
    if !t.records.is_empty() {
        let _ = writeln!(out);
    }
    let _ = writeln!(out, "</details>");
}

fn runs_table(out: &mut String, runs: &[Run]) {
    let last = runs.iter().rev().find_map(|r| r.outcome);
    let summary = match last {
        Some(RunOutcome::Passed) => "last passed",
        Some(RunOutcome::Failed) => "last failed",
        None => "results unknown",
    };
    let _ = writeln!(out);
    let _ = writeln!(out, "<details>");
    let _ = writeln!(
        out,
        "<summary>Agent checks: {} test and build {}, {summary}</summary>",
        runs.len(),
        plural(runs.len(), "run", "runs")
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "| When (UTC) | Command | Result |");
    let _ = writeln!(out, "| --- | --- | --- |");
    for run in runs {
        let when = crate::record::timestamp::format(&run.at)
            .map(|t| t.get(5..16).unwrap_or(&t).replace('T', " "))
            .unwrap_or_default();
        let mut result = match run.outcome {
            Some(RunOutcome::Passed) => "passed".to_owned(),
            Some(RunOutcome::Failed) => "**failed**".to_owned(),
            None => "unknown".to_owned(),
        };
        if let Some(counts) = run.tests.and_then(|t| t.describe()) {
            let _ = write!(result, " ({counts})");
        }
        if run.filtered {
            result.push_str(", filtered");
        }
        let _ = writeln!(
            out,
            "| {when} | `{}` | {result} |",
            code(run.command.split('\n').next().unwrap_or(""))
        );
    }
    let _ = writeln!(out, "</details>");
}

pub(super) fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
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

/// Text safe on one list line.
fn line_text(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}
