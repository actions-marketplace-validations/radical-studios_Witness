//! The review plan for a terminal.

use std::fmt::Write as _;

use super::markdown::plural;
use super::{Report, Section};
use crate::classify::Status;

#[derive(Debug, Clone, Copy, Default)]
pub struct TextOptions {
    /// Use ANSI colors.
    pub color: bool,
    /// List skimmable sections too.
    pub all: bool,
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

fn status_color(status: Status) -> &'static str {
    match status {
        Status::Failing => "\x1b[1;31m",
        Status::Guessed | Status::Unrecorded | Status::Unverified => "\x1b[33m",
        Status::Compiled => "\x1b[36m",
        Status::Tested => "\x1b[32m",
        Status::Human => DIM,
    }
}

impl Report {
    pub fn to_text(&self, options: TextOptions) -> String {
        let paint = |style: &str, text: &str| {
            if options.color {
                format!("{style}{text}{RESET}")
            } else {
                text.to_owned()
            }
        };
        let mut out = String::new();
        let s = &self.summary;
        let head = format!(
            "Witness: review {} of {} {} closely ({}...{})",
            s.review_closely,
            s.sections,
            plural(s.sections, "section", "sections"),
            self.base,
            self.head
        );
        let _ = writeln!(out, "{}", paint(BOLD, &head));
        if s.lines.agent > 0 {
            let pct = |n: usize| (n * 100 + s.lines.agent / 2) / s.lines.agent;
            let unchecked = s.lines.agent - s.lines.tested - s.lines.compiled;
            let _ = writeln!(
                out,
                "  {} agent {}: {}% tested, {}% only compiled, {}% unchecked",
                s.lines.agent,
                plural(s.lines.agent, "line", "lines"),
                pct(s.lines.tested),
                pct(s.lines.compiled),
                pct(unchecked)
            );
        }
        let width = self
            .sections()
            .map(|sec| location(sec).chars().count())
            .max()
            .unwrap_or(0)
            .min(48);
        let list = |out: &mut String, sections: &[Section]| {
            for sec in sections {
                let label = format!("{:<10}", sec.status.as_str().to_uppercase());
                let _ = writeln!(
                    out,
                    "  {} {:<width$}  {}",
                    paint(status_color(sec.status), &label),
                    location(sec),
                    sec.reason
                );
                for signal in &sec.signals {
                    let _ = writeln!(
                        out,
                        "  {:<10} {:<width$}  {}",
                        "",
                        "",
                        paint(DIM, &format!("⚑ {}", signal.detail))
                    );
                }
            }
        };
        if !self.flagged.is_empty() || !self.over_cap.is_empty() {
            let _ = writeln!(out);
            list(&mut out, &self.flagged);
            list(&mut out, &self.over_cap);
        }
        if !self.skim.is_empty() {
            let _ = writeln!(out);
            if options.all {
                let _ = writeln!(out, "{}", paint(BOLD, "To skim:"));
                list(&mut out, &self.skim);
            } else {
                let _ = writeln!(
                    out,
                    "{}",
                    paint(
                        DIM,
                        &format!(
                            "  {} more {} to skim (--all lists them)",
                            self.skim.len(),
                            plural(self.skim.len(), "section", "sections")
                        )
                    )
                );
            }
        }
        if s.sections == 0 {
            let _ = writeln!(out, "  No agent-written sections.");
        }
        out
    }
}

fn location(sec: &Section) -> String {
    match sec.start {
        Some(_) => format!("{}:{}", sec.file, sec.lines),
        None => format!("{} ({})", sec.file, sec.lines),
    }
}
