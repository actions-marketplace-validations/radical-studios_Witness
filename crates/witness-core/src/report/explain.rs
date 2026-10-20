//! `witness explain`: the evidence behind one line's verdict.

use std::fmt::Write as _;

use time::OffsetDateTime;

use super::{Analysis, Section};
use crate::record::{EditVia, EventKind};
use crate::scope::{self, Coverage, Packages};
use crate::testsum::{RunOutcome, TestCounts};
use crate::timeline::Placed;

/// What is known about one line of a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explanation {
    pub file: String,
    pub line: Option<u32>,
    /// The section the line is in; `None` for human-written lines.
    pub section: Option<Section>,
    pub edits: Vec<Note>,
    pub assumptions: Vec<Note>,
    /// Test and build runs after the last edit.
    pub runs: Vec<RunNote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub at: OffsetDateTime,
    pub agent: String,
    pub session: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunNote {
    pub at: OffsetDateTime,
    pub command: String,
    pub outcome: Option<RunOutcome>,
    pub tests: Option<TestCounts>,
    /// Whether the run covers the file.
    pub covers: bool,
}

/// Finds the segment of `file` containing `line` (or the file's first
/// segment when `line` is `None`) and collects its evidence.
pub fn explain(
    analysis: &Analysis,
    file: &str,
    line: Option<u32>,
    packages: &dyn Packages,
) -> Option<Explanation> {
    let idx = analysis.segments.iter().position(|seg| {
        seg.file == file
            && match (line, seg.span) {
                (None, _) | (Some(_), None) => true,
                (Some(l), Some(span)) => span.start <= l && l <= span.end,
            }
    })?;
    let seg = &analysis.segments[idx];
    let events = &analysis.timeline.events;
    let note = |p: &Placed, text: String| Note {
        at: p.event.timestamp,
        agent: p.event.agent.clone(),
        session: p.event.session_id.clone(),
        text,
    };
    let mut edit_refs: Vec<&Placed> = seg.edits.iter().filter_map(|i| events.get(*i)).collect();
    edit_refs.sort_by_key(|p| p.order);
    let edits = edit_refs
        .iter()
        .map(|p| {
            let text = match &p.event.kind {
                EventKind::Edit { tool, via, .. } => {
                    let via = match via {
                        Some(EditVia::Patch) => " (agent's patch)",
                        Some(EditVia::Preimage) => " (diffed with the file before)",
                        Some(EditVia::Snapshot) => " (working-tree snapshot)",
                        Some(EditVia::Content) => " (new file)",
                        Some(EditVia::Locate) => " (located in the file)",
                        Some(EditVia::Reported) => " (reported)",
                        Some(EditVia::Unknown) | None => "",
                    };
                    format!("edited with {tool}{via}")
                }
                _ => String::new(),
            };
            note(p, text)
        })
        .collect();
    let assumptions = seg
        .assumptions
        .iter()
        .filter_map(|i| events.get(*i))
        .map(|p| match &p.event.kind {
            EventKind::Assumption { text, .. } => note(p, text.clone()),
            _ => note(p, String::new()),
        })
        .collect();
    let after = edit_refs.last().map_or(0, |p| p.order + 1);
    let runs = events
        .iter()
        .skip(after)
        .filter_map(|p| match &p.event.kind {
            EventKind::Command {
                classification,
                command,
                tests,
                scope: run_scope,
                ..
            } if classification.verifies() => Some(RunNote {
                at: p.event.timestamp,
                command: command.clone(),
                outcome: p.event.run_outcome(),
                tests: *tests,
                covers: scope::covers(run_scope.as_ref(), file, packages) == Coverage::Covers,
            }),
            _ => None,
        })
        .collect();
    Some(Explanation {
        file: file.to_owned(),
        line,
        section: analysis.sections[idx].as_ref().map(|(s, _)| s.clone()),
        edits,
        assumptions,
        runs,
    })
}

impl Explanation {
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let at = |t: &OffsetDateTime| crate::record::timestamp::format(t).unwrap_or_default();
        let location = match self.line {
            Some(l) => format!("{}:{l}", self.file),
            None => self.file.clone(),
        };
        match &self.section {
            Some(sec) => {
                let _ = writeln!(
                    out,
                    "{location}: {} (lines {}), {}",
                    sec.status.as_str(),
                    sec.lines,
                    sec.reason
                );
                for signal in &sec.signals {
                    let _ = writeln!(out, "  ⚑ {}", signal.detail);
                }
            }
            None => {
                let _ = writeln!(
                    out,
                    "{location}: written by a human (no agent edit recorded)"
                );
            }
        }
        if !self.edits.is_empty() {
            let _ = writeln!(out, "\nWritten by:");
            for e in &self.edits {
                let _ = writeln!(
                    out,
                    "  {}  {} in session {}: {}",
                    at(&e.at),
                    e.agent,
                    e.session,
                    e.text
                );
            }
        }
        if !self.assumptions.is_empty() {
            let _ = writeln!(out, "\nAssumptions:");
            for a in &self.assumptions {
                let _ = writeln!(out, "  {}  {}: {}", at(&a.at), a.agent, a.text);
            }
        }
        if self.section.is_some() {
            let _ = writeln!(out, "\nChecks after the last edit:");
            if self.runs.is_empty() {
                let _ = writeln!(out, "  none");
            }
            for r in &self.runs {
                let result = match r.outcome {
                    Some(RunOutcome::Passed) => "passed",
                    Some(RunOutcome::Failed) => "failed",
                    None => "result unknown",
                };
                let counts = r
                    .tests
                    .and_then(|t| t.describe())
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default();
                let covers = if r.covers {
                    ""
                } else {
                    ", doesn't cover this file"
                };
                let _ = writeln!(
                    out,
                    "  {}  `{}` {result}{counts}{covers}",
                    at(&r.at),
                    r.command
                );
            }
        }
        out
    }
}
