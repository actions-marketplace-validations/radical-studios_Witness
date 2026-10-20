//! Who wrote each line of a change.
//!
//! Every added line of a diff hunk is matched to the agent edits that wrote
//! that content (by [`line_hash`]), so attribution survives rebases, later
//! commits and squash merges that move lines around. A hunk whose lines
//! have different authors is split into [`Segment`]s, so a human tweak to
//! agent code is its own human section.
//!
//! Edits recorded without hashes (older logs, notebooks, whole-file edits)
//! fall back to the line ranges replayed by [`Timeline`].

use std::collections::{HashMap, HashSet};

use crate::hunk::{Hunk, spans_overlap};
use crate::record::{EventKind, LineRange, line_hash};
use crate::timeline::Timeline;

/// A run of a hunk's lines with one kind of author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub file: String,
    /// Lines at head; `None` for a whole-file (binary) change. A pure
    /// deletion covers the two lines around the gap.
    pub span: Option<LineRange>,
    /// The hunk is a pure deletion; `span.start` is the line before it.
    pub deletion: bool,
    /// The added lines in this segment.
    pub added: Vec<String>,
    /// Timeline positions of the agent edits that wrote these lines; empty
    /// when no agent edit did.
    pub edits: Vec<usize>,
    /// Timeline positions of assumptions about these lines.
    pub assumptions: Vec<usize>,
}

impl Segment {
    pub fn by_agent(&self) -> bool {
        !self.edits.is_empty()
    }

    /// Human-readable location, e.g. `12-18`, `after 40 (deletion)`.
    pub fn display_lines(&self) -> String {
        match self.span {
            None => "whole file".to_owned(),
            Some(span) if self.deletion => format!("after {} (deletion)", span.start),
            Some(span) => span.to_string(),
        }
    }

    /// First and last line at head, for reports; a deletion sits after its
    /// first line.
    pub fn bounds(&self) -> (Option<u32>, Option<u32>) {
        match self.span {
            None => (None, None),
            Some(span) if self.deletion => (Some(span.start), Some(span.start)),
            Some(span) => (Some(span.start), Some(span.end)),
        }
    }
}

/// An agent edit, indexed for matching.
struct EditRef {
    /// Position in the timeline.
    idx: usize,
    added: Option<HashSet<u64>>,
    removed: Option<HashSet<u64>>,
    span: Option<LineRange>,
}

/// Lines this short match by content only with support from their
/// position or neighbors: `return x;` or `} else {` recur too often.
const WEAK_LINE_CHARS: usize = 12;
/// How far a weak match may sit from the edit's replayed range.
const WEAK_SLACK: u32 = 2;

/// Splits each hunk into segments by author.
pub fn attribute(hunks: &[Hunk], timeline: &Timeline) -> Vec<Segment> {
    let mut by_file: HashMap<&str, Vec<EditRef>> = HashMap::new();
    for (idx, placed) in timeline.events.iter().enumerate() {
        if let EventKind::Edit {
            file,
            line_hashes,
            removed_hashes,
            ..
        } = &placed.event.kind
        {
            by_file.entry(file).or_default().push(EditRef {
                idx,
                added: line_hashes.as_deref().map(parse_hashes),
                removed: removed_hashes.as_deref().map(parse_hashes),
                span: placed.span,
            });
        }
    }
    let none = Vec::new();
    hunks
        .iter()
        .flat_map(|hunk| {
            let edits = by_file.get(hunk.file.as_str()).unwrap_or(&none);
            let mut segments = split_hunk(hunk, edits);
            for seg in &mut segments {
                seg.assumptions = assumptions_for(seg, timeline);
            }
            segments
        })
        .collect()
}

fn split_hunk(hunk: &Hunk, edits: &[EditRef]) -> Vec<Segment> {
    let whole = |edits: Vec<usize>| Segment {
        file: hunk.file.clone(),
        span: hunk.span(),
        deletion: hunk.is_pure_deletion(),
        added: hunk.added.clone(),
        edits,
        assumptions: Vec::new(),
    };
    if hunk.binary {
        return vec![whole(edits.iter().map(|e| e.idx).collect())];
    }
    if hunk.is_pure_deletion() || hunk.added.is_empty() {
        return vec![whole(deletion_origin(hunk, edits))];
    }
    if hunk.added.len() != hunk.new_count as usize {
        // Line text doesn't match the header: match the hunk as a whole.
        let overlapping = edits
            .iter()
            .filter(|e| spans_overlap(e.span, hunk.span()))
            .map(|e| e.idx)
            .collect();
        return vec![whole(overlapping)];
    }

    let origins = line_origins(hunk, edits);
    let mut segments: Vec<Segment> = Vec::new();
    let mut i = 0;
    while i < origins.len() {
        let by_agent = origins[i].is_some();
        let mut j = i;
        let mut seg_edits: Vec<usize> = Vec::new();
        while j < origins.len() && origins[j].is_some() == by_agent {
            if let Some(idx) = origins[j]
                && !seg_edits.contains(&idx)
            {
                seg_edits.push(idx);
            }
            j += 1;
        }
        seg_edits.sort_unstable();
        let start = hunk.new_start + to_u32(i);
        let end = hunk.new_start + to_u32(j) - 1;
        segments.push(Segment {
            file: hunk.file.clone(),
            span: Some(LineRange::new(start, end)),
            deletion: false,
            added: hunk.added[i..j].to_vec(),
            edits: seg_edits,
            assumptions: Vec::new(),
        });
        i = j;
    }
    segments
}

/// For each added line, the timeline position of the agent edit that wrote
/// it, or `None` for a human line.
fn line_origins(hunk: &Hunk, edits: &[EditRef]) -> Vec<Option<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Line {
        Neutral,
        Strong(Option<usize>),
        /// Matched by content only, pending support.
        Weak(usize),
    }
    let lines: Vec<Line> = hunk
        .added
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let norm = text.trim();
            if is_neutral(norm) {
                return Line::Neutral;
            }
            let pos = hunk.new_start + to_u32(i);
            let hash = parse_hash(&line_hash(text));
            let matched = edits
                .iter()
                .rev()
                .find(|e| e.added.as_ref().is_some_and(|set| set.contains(&hash)));
            match matched {
                Some(e) if norm.chars().count() >= WEAK_LINE_CHARS => Line::Strong(Some(e.idx)),
                Some(e) if near(e.span, pos) => Line::Strong(Some(e.idx)),
                Some(e) => Line::Weak(e.idx),
                None => Line::Strong(span_fallback(edits, pos)),
            }
        })
        .collect();

    // A weak match stands when a neighbor is the agent's too.
    let agent_at = |k: usize| matches!(lines.get(k), Some(Line::Strong(Some(_))));
    let mut origins: Vec<Option<Option<usize>>> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| match *line {
            Line::Neutral => None,
            Line::Strong(o) => Some(o),
            Line::Weak(idx) => {
                let supported = (i > 0 && agent_at(i - 1)) || agent_at(i + 1);
                Some(supported.then_some(idx))
            }
        })
        .collect();

    // Neutral lines (blank, braces) take the previous decided line's
    // author, or the next one's.
    if origins.iter().all(Option::is_none) {
        let whole = edits
            .iter()
            .rev()
            .find(|e| spans_overlap(e.span, hunk.span()))
            .map(|e| e.idx);
        return vec![whole; origins.len()];
    }
    let mut last: Option<Option<usize>> = None;
    for o in origins.iter_mut() {
        match o {
            Some(decided) => last = Some(*decided),
            None => *o = last,
        }
    }
    let mut next: Option<Option<usize>> = None;
    for o in origins.iter_mut().rev() {
        match o {
            Some(decided) => next = Some(*decided),
            None => *o = next,
        }
    }
    origins.into_iter().map(Option::flatten).collect()
}

/// The agent edits behind a pure deletion: those that removed these lines,
/// or, for edits recorded without removed-line hashes, those whose range
/// covers the gap.
fn deletion_origin(hunk: &Hunk, edits: &[EditRef]) -> Vec<usize> {
    let removed: HashSet<u64> = hunk
        .removed
        .iter()
        .filter(|l| !is_neutral(l.trim()))
        .map(|l| parse_hash(&line_hash(l)))
        .collect();
    let by_hash: Vec<usize> = edits
        .iter()
        .filter(|e| {
            e.removed
                .as_ref()
                .is_some_and(|set| set.iter().any(|h| removed.contains(h)))
        })
        .map(|e| e.idx)
        .collect();
    if !by_hash.is_empty() {
        return by_hash;
    }
    edits
        .iter()
        .filter(|e| e.removed.is_none() && spans_overlap(e.span, hunk.span()))
        .map(|e| e.idx)
        .collect()
}

/// The latest edit without content hashes whose range covers `pos`.
fn span_fallback(edits: &[EditRef], pos: u32) -> Option<usize> {
    edits
        .iter()
        .rev()
        .find(|e| e.added.is_none() && spans_overlap(e.span, Some(LineRange::new(pos, pos))))
        .map(|e| e.idx)
}

fn near(span: Option<LineRange>, pos: u32) -> bool {
    span.is_some_and(|s| {
        s.start.saturating_sub(WEAK_SLACK) <= pos && pos <= s.end.saturating_add(WEAK_SLACK)
    })
}

/// Lines that say nothing about who wrote them: blank, or punctuation and
/// at most two other characters (`}`, `});`, `end`, `fi`).
pub fn is_neutral(norm: &str) -> bool {
    norm.chars().filter(|c| c.is_alphanumeric()).count() <= 3 && norm.chars().count() <= 4
        || norm.chars().all(|c| !c.is_alphanumeric())
}

/// Assumptions about a segment: whole-file ones, ones recorded on the same
/// content, and ones whose replayed range overlaps it.
fn assumptions_for(seg: &Segment, timeline: &Timeline) -> Vec<usize> {
    let hashes: HashSet<u64> = seg
        .added
        .iter()
        .filter(|l| !is_neutral(l.trim()))
        .map(|l| parse_hash(&line_hash(l)))
        .collect();
    timeline
        .events
        .iter()
        .enumerate()
        .filter(|(_, p)| match &p.event.kind {
            EventKind::Assumption {
                file,
                lines,
                line_hashes,
                ..
            } if *file == seg.file => {
                lines.is_none()
                    || spans_overlap(p.span, seg.span)
                    || line_hashes
                        .as_deref()
                        .is_some_and(|hs| hs.iter().any(|h| hashes.contains(&parse_hash(h))))
            }
            _ => false,
        })
        .map(|(i, _)| i)
        .collect()
}

fn parse_hashes(hashes: &[String]) -> HashSet<u64> {
    hashes.iter().map(|h| parse_hash(h)).collect()
}

fn parse_hash(hex: &str) -> u64 {
    u64::from_str_radix(hex, 16).unwrap_or(u64::MAX)
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Change, Event, line_hashes};
    use time::Duration;
    use time::macros::datetime;

    /// Events for one file, built in order.
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
        /// The agent wrote `lines` at `start` (an insertion).
        fn wrote(self, start: u32, lines: &[&str]) -> Self {
            let change = Change {
                start,
                removed: 0,
                added: u32::try_from(lines.len()).unwrap(),
            };
            self.push(EventKind::Edit {
                file: "a.rs".into(),
                lines: Some(change.new_range()),
                change: Some(change),
                tool: "Edit".into(),
                via: None,
                line_hashes: Some(line_hashes(lines)),
                removed_hashes: None,
            })
        }
        fn deleted(self, start: u32, lines: &[&str]) -> Self {
            let change = Change {
                start,
                removed: u32::try_from(lines.len()).unwrap(),
                added: 0,
            };
            self.push(EventKind::Edit {
                file: "a.rs".into(),
                lines: Some(change.new_range()),
                change: Some(change),
                tool: "Edit".into(),
                via: None,
                line_hashes: Some(Vec::new()),
                removed_hashes: Some(line_hashes(lines)),
            })
        }
        /// An edit from an old log: a range, no hashes.
        fn legacy(self, start: u32, added: u32) -> Self {
            let change = Change {
                start,
                removed: 0,
                added,
            };
            self.push(EventKind::Edit {
                file: "a.rs".into(),
                lines: Some(change.new_range()),
                change: Some(change),
                tool: "Edit".into(),
                via: None,
                line_hashes: None,
                removed_hashes: None,
            })
        }
        fn assume(self, lines: Option<(u32, u32)>, hashed: &[&str]) -> Self {
            self.push(EventKind::Assumption {
                file: "a.rs".into(),
                lines: lines.map(|(a, b)| LineRange::new(a, b)),
                text: "x".into(),
                line_hashes: (!hashed.is_empty()).then(|| line_hashes(hashed)),
            })
        }
        fn segments(self, hunk: &Hunk) -> Vec<Segment> {
            attribute(std::slice::from_ref(hunk), &Timeline::build(vec![self.0]))
        }
    }

    fn hunk(new_start: u32, added: &[&str]) -> Hunk {
        Hunk {
            file: "a.rs".into(),
            old_start: new_start,
            old_count: 0,
            new_start,
            new_count: u32::try_from(added.len()).unwrap(),
            added: added.iter().map(|s| (*s).to_owned()).collect(),
            removed: Vec::new(),
            binary: false,
        }
    }

    fn summary(segments: &[Segment]) -> Vec<(String, bool)> {
        segments
            .iter()
            .map(|s| (s.display_lines(), s.by_agent()))
            .collect()
    }

    const CODE: [&str; 3] = [
        "let total = items.iter().sum();",
        "if total > LIMIT {",
        "    return Err(TooBig(total));",
    ];

    #[test]
    fn lines_match_by_content_wherever_they_moved() {
        // Written at line 10; a rebase moved it to 40.
        let segs = Log::new().wrote(10, &CODE).segments(&hunk(40, &CODE));
        assert_eq!(summary(&segs), vec![("40-42".into(), true)]);
        assert_eq!(segs[0].edits, vec![0]);
    }

    #[test]
    fn a_human_change_to_agent_code_splits_the_hunk() {
        let changed = [CODE[0], "if total >= LIMIT {", CODE[2]];
        let segs = Log::new().wrote(10, &CODE).segments(&hunk(10, &changed));
        assert_eq!(
            summary(&segs),
            vec![
                ("10".into(), true),
                ("11".into(), false),
                ("12".into(), true),
            ]
        );
    }

    #[test]
    fn neutral_lines_take_their_neighbors_author() {
        let lines = [CODE[1], CODE[2], "}", "", "fn other_function() {}"];
        let segs = Log::new()
            .wrote(5, &[CODE[1], CODE[2], "}"])
            .segments(&hunk(5, &lines));
        assert_eq!(
            summary(&segs),
            vec![("5-8".into(), true), ("9".into(), false)]
        );
    }

    #[test]
    fn short_lines_need_position_or_neighbors() {
        // The agent wrote `return x;` far away; a human wrote one here.
        let segs = Log::new()
            .wrote(100, &["return x;"])
            .segments(&hunk(10, &["return x;"]));
        assert_eq!(summary(&segs), vec![("10".into(), false)]);
        // At the agent's own position it counts.
        let segs = Log::new()
            .wrote(10, &["return x;"])
            .segments(&hunk(10, &["return x;"]));
        assert_eq!(summary(&segs), vec![("10".into(), true)]);
        // Next to the agent's longer lines it counts too.
        let segs = Log::new()
            .wrote(100, &[CODE[0], "return x;"])
            .segments(&hunk(10, &[CODE[0], "return x;"]));
        assert_eq!(summary(&segs), vec![("10-11".into(), true)]);
    }

    #[test]
    fn later_edits_win() {
        let segs = Log::new()
            .wrote(10, &CODE)
            .wrote(10, &CODE)
            .segments(&hunk(10, &CODE));
        assert_eq!(segs[0].edits, vec![1]);
    }

    #[test]
    fn deletions_match_removed_lines() {
        let mut del = hunk(9, &[]);
        del.old_count = 2;
        del.removed = vec![CODE[0].into(), CODE[1].into()];
        let segs = Log::new().deleted(50, &CODE[..2]).segments(&del);
        assert!(segs[0].by_agent());
        assert_eq!(segs[0].display_lines(), "after 9 (deletion)");
        // A deletion of other lines is human.
        del.removed = vec!["fn something_else() {}".into()];
        assert!(!Log::new().deleted(50, &CODE[..2]).segments(&del)[0].by_agent());
    }

    #[test]
    fn edits_without_hashes_fall_back_to_ranges() {
        let segs = Log::new().legacy(10, 2).segments(&hunk(
            10,
            &[
                "fn alpha_one() {}",
                "fn beta_two() {}",
                "fn gamma_three() {}",
            ],
        ));
        assert_eq!(
            summary(&segs),
            vec![("10-11".into(), true), ("12".into(), false)]
        );
    }

    #[test]
    fn assumptions_attach_by_range_content_or_file() {
        let segs = Log::new()
            .wrote(10, &CODE)
            .assume(Some((90, 91)), &[CODE[1]])
            .segments(&hunk(40, &CODE));
        assert_eq!(segs[0].assumptions, vec![1]);
        let segs = Log::new()
            .wrote(10, &CODE)
            .assume(None, &[])
            .segments(&hunk(40, &CODE));
        assert_eq!(segs[0].assumptions, vec![1]);
        let segs = Log::new()
            .wrote(10, &CODE)
            .assume(Some((90, 91)), &["something unrelated here"])
            .segments(&hunk(40, &CODE));
        assert!(segs[0].assumptions.is_empty());
    }

    proptest::proptest! {
        /// Agent lines with distinctive content keep their author however far
        /// unrelated lines push them, and human lines next to them stay human.
        #[test]
        fn distinctive_lines_keep_their_author(
            agent_lines in proptest::collection::vec("[a-z]{6,12} = [a-z]{6,12}\\(\\);", 1..6),
            human_lines in proptest::collection::vec("[A-Z]{6,12} := [A-Z]{6,12};", 0..4),
            written_at in 1u32..50,
            shift in 0u32..200,
        ) {
            let agent: Vec<&str> = agent_lines.iter().map(String::as_str).collect();
            let mut all: Vec<&str> = agent.clone();
            all.extend(human_lines.iter().map(String::as_str));
            let segs = Log::new()
                .wrote(written_at, &agent)
                .segments(&hunk(written_at + shift, &all));
            let agent_count: usize = segs.iter().filter(|s| s.by_agent()).map(|s| s.added.len()).sum();
            let human_count: usize = segs.iter().filter(|s| !s.by_agent()).map(|s| s.added.len()).sum();
            proptest::prop_assert_eq!(agent_count, agent.len());
            proptest::prop_assert_eq!(human_count, human_lines.len());
        }
    }

    #[test]
    fn neutral_lines() {
        for l in ["", "}", "});", "end", "fi", ")", "*/", "]", "{"] {
            assert!(is_neutral(l), "{l:?}");
        }
        for l in ["x = 1", "return x;", "foo()", "mod a;"] {
            assert!(!is_neutral(l), "{l:?}");
        }
    }
}
