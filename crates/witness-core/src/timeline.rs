//! Orders session events and replays edits so recorded line ranges refer to
//! the file as it ended up, not as it was at the time of each event.

use crate::record::{Change, Event, EventKind, LineRange};

/// An event placed on the global timeline.
#[derive(Debug, Clone)]
pub struct Placed {
    /// Position on the timeline; larger is later.
    pub order: usize,
    pub event: Event,
    /// For edits and assumptions: span after replaying every later edit.
    /// `None` means the whole file (or the event has no file).
    pub span: Option<LineRange>,
}

impl Placed {
    pub fn file(&self) -> Option<&str> {
        self.event.file()
    }
}

/// Events from all sessions, in order, with spans corrected for later edits.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    pub events: Vec<Placed>,
}

impl Timeline {
    /// Builds a timeline from per-session event lists, each in file order.
    pub fn build(sessions: Vec<Vec<Event>>) -> Self {
        let mut all: Vec<(usize, Event)> = sessions
            .into_iter()
            .flat_map(|events| events.into_iter().enumerate())
            .collect();
        all.sort_by(|(ia, a), (ib, b)| {
            a.timestamp
                .cmp(&b.timestamp)
                .then_with(|| a.session_id.cmp(&b.session_id))
                .then_with(|| ia.cmp(ib))
        });

        let mut events: Vec<Placed> = Vec::with_capacity(all.len());
        for (order, (_, event)) in all.into_iter().enumerate() {
            let span = match &event.kind {
                EventKind::Edit {
                    file,
                    lines,
                    change,
                    ..
                } => {
                    if let Some(change) = change {
                        for earlier in events.iter_mut() {
                            if earlier.file() == Some(file.as_str())
                                && matches!(
                                    earlier.event.kind,
                                    EventKind::Edit { .. } | EventKind::Assumption { .. }
                                )
                            {
                                earlier.span = earlier.span.map(|r| shift(r, change));
                            }
                        }
                    }
                    *lines
                }
                EventKind::Assumption { lines, .. } => *lines,
                _ => None,
            };
            events.push(Placed { order, event, span });
        }
        Timeline { events }
    }
}

/// Moves `range` to account for a later `change` in the same file.
pub fn shift(range: LineRange, change: &Change) -> LineRange {
    let (start, end) = (i64::from(range.start), i64::from(range.end));
    let c_start = i64::from(change.start);
    let removed = i64::from(change.removed);
    let delta = i64::from(change.added) - removed;
    let to_u32 = |v: i64| u32::try_from(v.max(1)).unwrap_or(u32::MAX);

    if removed == 0 {
        // Pure insertion before line `c_start`.
        return if end < c_start {
            range
        } else if start >= c_start {
            LineRange::new(to_u32(start + delta), to_u32(end + delta))
        } else {
            LineRange::new(range.start, to_u32(end + delta))
        };
    }
    let old_end = c_start + removed - 1;
    if end < c_start {
        range
    } else if start > old_end {
        LineRange::new(to_u32(start + delta), to_u32(end + delta))
    } else {
        // The range overlapped the replaced lines: it now covers whatever
        // survived of it plus the replacement.
        let new_change_end = change.new_range().end;
        let new_start = start.min(c_start);
        let new_end = if end > old_end {
            end + delta
        } else {
            i64::from(new_change_end)
        };
        LineRange::new(
            to_u32(new_start),
            to_u32(new_end.max(i64::from(new_change_end))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{AGENT_CLAUDE_CODE, CommandClass};
    use time::Duration;
    use time::macros::datetime;

    fn ch(start: u32, removed: u32, added: u32) -> Change {
        Change {
            start,
            removed,
            added,
        }
    }

    #[test]
    fn shift_cases() {
        let r = LineRange::new(10, 12);
        // Change entirely below: unchanged.
        assert_eq!(shift(r, &ch(20, 2, 5)), r);
        // Insertion above: moves down.
        assert_eq!(shift(r, &ch(3, 0, 4)), LineRange::new(14, 16));
        // Replacement above that shrinks: moves up.
        assert_eq!(shift(r, &ch(2, 5, 1)), LineRange::new(6, 8));
        // Insertion inside: grows.
        assert_eq!(shift(r, &ch(11, 0, 2)), LineRange::new(10, 14));
        // Replacement overlapping the start.
        assert_eq!(shift(r, &ch(9, 2, 3)), LineRange::new(9, 13));
        // Replacement swallowing the whole range.
        assert_eq!(shift(r, &ch(8, 10, 2)), LineRange::new(8, 9));
    }

    proptest::proptest! {
        /// Lines of a range that survive a later change stay inside the
        /// shifted range. Checked against a simulated file.
        #[test]
        fn shifted_range_covers_surviving_lines(
            len in 1u32..40,
            a in 1u32..40,
            b in 1u32..40,
            start in 1u32..42,
            removed in 0u32..10,
            added in 0u32..10,
        ) {
            let (a, b) = (a.min(len), b.min(len));
            let range = LineRange::new(a, b);
            let start = start.min(len + 1);
            let removed = removed.min(len + 1 - start);
            let change = ch(start, removed, added);

            // Old lines are ids 1..=len; inserted lines get ids above len.
            let old: Vec<u32> = (1..=len).collect();
            let s = (start - 1) as usize;
            let mut new: Vec<u32> = old[..s].to_vec();
            new.extend((0..added).map(|i| len + 1 + i));
            new.extend_from_slice(&old[s + removed as usize..]);

            let shifted = shift(range, &change);
            proptest::prop_assert!(shifted.start >= 1 && shifted.start <= shifted.end);
            for id in range.start..=range.end {
                if let Some(pos) = new.iter().position(|&x| x == id) {
                    let line = u32::try_from(pos).unwrap() + 1;
                    proptest::prop_assert!(
                        shifted.start <= line && line <= shifted.end,
                        "line {} of {:?} is at {} after {:?}, outside {:?}",
                        id, range, line, change, shifted
                    );
                }
            }
        }
    }

    fn ev(session: &str, secs: i64, kind: EventKind) -> Event {
        Event::new(
            session,
            AGENT_CLAUDE_CODE,
            datetime!(2026-10-20 00:00:00 UTC) + Duration::seconds(secs),
            kind,
        )
    }

    fn edit(file: &str, start: u32, removed: u32, added: u32) -> EventKind {
        let change = ch(start, removed, added);
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

    #[test]
    fn later_edits_shift_earlier_spans_in_same_file_only() {
        let timeline = Timeline::build(vec![vec![
            ev("s", 1, edit("a.rs", 50, 1, 1)),
            ev("s", 2, edit("b.rs", 1, 0, 10)),
            ev("s", 3, edit("a.rs", 10, 0, 5)),
        ]]);
        let spans: Vec<_> = timeline.events.iter().map(|p| p.span).collect();
        assert_eq!(
            spans,
            vec![
                Some(LineRange::new(55, 55)),
                Some(LineRange::new(1, 10)),
                Some(LineRange::new(10, 14)),
            ]
        );
    }

    #[test]
    fn sessions_interleave_by_timestamp() {
        let cmd = EventKind::Command {
            classification: CommandClass::Test,
            exit_code: Some(0),
            command: "cargo test".into(),
            outcome: None,
            tests: None,
            scope: None,
        };
        let timeline = Timeline::build(vec![
            vec![ev("b", 5, cmd)],
            vec![
                ev("a", 1, edit("a.rs", 1, 0, 1)),
                ev("a", 9, edit("a.rs", 1, 0, 1)),
            ],
        ]);
        let order: Vec<_> = timeline.events.iter().map(|p| p.event.timestamp).collect();
        assert!(order.windows(2).all(|w| w[0] <= w[1]));
        assert!(matches!(
            timeline.events[1].event.kind,
            EventKind::Command { .. }
        ));
    }
}
