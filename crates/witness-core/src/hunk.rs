//! Diff hunks, as seen at the head revision.

use serde::{Deserialize, Serialize};

use crate::record::LineRange;

/// One hunk of `git diff -U0 base...head`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// Path at head, relative to the repo root.
    pub file: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Added lines without the leading `+`. Used for fingerprints only.
    #[serde(skip)]
    pub added: Vec<String>,
    /// Binary change: no line information, covers the whole file.
    #[serde(default)]
    pub binary: bool,
}

impl Hunk {
    /// The lines this hunk occupies at head. A pure deletion covers the two
    /// lines around the gap; a binary change covers the whole file (`None`).
    pub fn span(&self) -> Option<LineRange> {
        if self.binary {
            None
        } else if self.new_count == 0 {
            Some(LineRange::new(self.new_start, self.new_start + 1))
        } else {
            Some(LineRange::new(
                self.new_start,
                self.new_start + self.new_count - 1,
            ))
        }
    }

    pub fn is_pure_deletion(&self) -> bool {
        !self.binary && self.new_count == 0
    }

    /// Human-readable location, e.g. `12-18`, `after 40 (deletion)`, `whole file`.
    pub fn display_lines(&self) -> String {
        if self.binary {
            "whole file".to_owned()
        } else if self.new_count == 0 {
            format!("after {} (deletion)", self.new_start)
        } else {
            LineRange::new(self.new_start, self.new_start + self.new_count - 1).to_string()
        }
    }
}

/// Whether two optional spans overlap; `None` means the whole file.
pub fn spans_overlap(a: Option<LineRange>, b: Option<LineRange>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.overlaps(&b),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(new_start: u32, new_count: u32) -> Hunk {
        Hunk {
            file: "a".into(),
            old_start: 1,
            old_count: 1,
            new_start,
            new_count,
            added: Vec::new(),
            binary: false,
        }
    }

    #[test]
    fn spans() {
        assert_eq!(hunk(5, 3).span(), Some(LineRange::new(5, 7)));
        assert_eq!(hunk(9, 0).span(), Some(LineRange::new(9, 10)));
        assert_eq!(hunk(0, 0).span(), Some(LineRange::new(1, 1)));
        assert_eq!(hunk(5, 3).display_lines(), "5-7");
        assert_eq!(hunk(5, 1).display_lines(), "5");
        assert_eq!(hunk(9, 0).display_lines(), "after 9 (deletion)");
    }

    #[test]
    fn whole_file_overlaps_everything() {
        assert!(spans_overlap(None, Some(LineRange::new(3, 4))));
        assert!(spans_overlap(
            Some(LineRange::new(3, 4)),
            Some(LineRange::new(4, 9))
        ));
        assert!(!spans_overlap(
            Some(LineRange::new(3, 4)),
            Some(LineRange::new(5, 9))
        ));
    }
}
