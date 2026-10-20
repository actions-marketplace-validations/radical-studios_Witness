//! Line diffs of a file before and after a tool call.

use similar::{DiffOp, TextDiff};

use crate::hunk::Hunk;

/// Hunks turning `old` into `new`, in `git diff -U0` conventions: a pure
/// insertion's `old_start` and a pure deletion's `new_start` are the line
/// before the gap. A missing file (`None`) is empty.
pub fn diff_hunks(file: &str, old: Option<&str>, new: Option<&str>) -> Vec<Hunk> {
    let (old, new) = (old.unwrap_or(""), new.unwrap_or(""));
    if old == new {
        return Vec::new();
    }
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let diff = TextDiff::configure().diff_slices(&old_lines, &new_lines);

    let mut hunks: Vec<Hunk> = Vec::new();
    let mut pending: Option<(usize, usize, usize, usize)> = None; // old_idx, old_len, new_idx, new_len
    let flush = |p: Option<(usize, usize, usize, usize)>, hunks: &mut Vec<Hunk>| {
        let Some((oi, ol, ni, nl)) = p else { return };
        let to_u32 = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        // 1-based starts; an empty side points at the line before the gap.
        let old_start = if ol == 0 { oi } else { oi + 1 };
        let new_start = if nl == 0 { ni } else { ni + 1 };
        hunks.push(Hunk {
            file: file.to_owned(),
            old_start: to_u32(old_start),
            old_count: to_u32(ol),
            new_start: to_u32(new_start),
            new_count: to_u32(nl),
            added: new_lines[ni..ni + nl]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            removed: old_lines[oi..oi + ol]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            binary: false,
        });
    };
    for op in diff.ops() {
        let (oi, ol, ni, nl) = match *op {
            DiffOp::Equal { .. } => {
                flush(pending.take(), &mut hunks);
                continue;
            }
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => (old_index, old_len, new_index, 0),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => (old_index, 0, new_index, new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => (old_index, old_len, new_index, new_len),
        };
        // Adjacent operations (a delete next to an insert) form one hunk.
        pending = Some(match pending {
            Some((poi, pol, pni, pnl)) if poi + pol == oi && pni + pnl == ni => {
                (poi, pol + ol, pni, pnl + nl)
            }
            other => {
                flush(other, &mut hunks);
                (oi, ol, ni, nl)
            }
        });
    }
    flush(pending, &mut hunks);
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Change, LineRange};

    fn counts(h: &Hunk) -> (u32, u32, u32, u32) {
        (h.old_start, h.old_count, h.new_start, h.new_count)
    }

    #[test]
    fn follows_git_unified_zero_conventions() {
        let old = "a\nb\nc\nd\n";
        // Replace b, insert after d, delete nothing else.
        let hunks = diff_hunks("f", Some(old), Some("a\nB\nc\nd\ne\n"));
        assert_eq!(
            hunks.iter().map(counts).collect::<Vec<_>>(),
            vec![(2, 1, 2, 1), (4, 0, 5, 1)]
        );
        assert_eq!(hunks[0].added, vec!["B"]);
        assert_eq!(hunks[0].removed, vec!["b"]);
        // Pure deletion of b and c: the gap is after new line 1.
        let del = diff_hunks("f", Some(old), Some("a\nd\n"));
        assert_eq!(counts(&del[0]), (2, 2, 1, 0));
        assert_eq!(
            del[0].change(),
            Change {
                start: 2,
                removed: 2,
                added: 0
            }
        );
        assert_eq!(del[0].change().new_range(), LineRange::new(1, 2));
    }

    #[test]
    fn new_and_deleted_files() {
        let created = diff_hunks("f", None, Some("x\ny\n"));
        assert_eq!(counts(&created[0]), (0, 0, 1, 2));
        let deleted = diff_hunks("f", Some("x\n"), None);
        assert_eq!(counts(&deleted[0]), (1, 1, 0, 0));
        assert!(diff_hunks("f", Some("same"), Some("same")).is_empty());
    }
}
