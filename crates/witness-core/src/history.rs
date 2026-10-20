//! Agent track record: types and storage interface only in this pass.
//!
//! History is keyed by file path plus a hash of the hunk's normalized
//! content, never by commit SHA, so squash and rebase merges don't break it.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// Identifies a hunk's content independently of the commit it landed in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HunkFingerprint {
    pub path: String,
    /// Hex SHA-256 of the normalized added lines.
    pub content_hash: String,
}

impl HunkFingerprint {
    /// Normalization: trim each line, drop blank lines, join with `\n`.
    pub fn new(path: &str, added_lines: &[String]) -> Self {
        let normalized = added_lines
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let digest = Sha256::digest(normalized.as_bytes());
        let content_hash = digest.iter().map(|b| format!("{b:02x}")).collect();
        Self {
            path: path.to_owned(),
            content_hash,
        }
    }
}

/// What later happened to a hunk an agent wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Survived,
    Rewritten,
    Reverted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub fingerprint: HunkFingerprint,
    pub agent: String,
    pub area: String,
    pub outcome: Outcome,
    #[serde(with = "crate::record::timestamp")]
    pub observed_at: OffsetDateTime,
}

/// Aggregate for one agent in one area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackRecord {
    pub agent: String,
    pub area: String,
    pub samples: u32,
    pub rewritten: u32,
    pub reverted: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("history store error: {0}")]
    Store(String),
}

/// Where track-record history is kept. Implementations come later.
pub trait HistoryStore {
    fn record(&mut self, entry: HistoryEntry) -> Result<(), HistoryError>;
    fn track_record(&self, agent: &str, area: &str) -> Result<Option<TrackRecord>, HistoryError>;
}

/// Store with no history: every lookup returns `None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullHistoryStore;

impl HistoryStore for NullHistoryStore {
    fn record(&mut self, _entry: HistoryEntry) -> Result<(), HistoryError> {
        Ok(())
    }

    fn track_record(&self, _agent: &str, _area: &str) -> Result<Option<TrackRecord>, HistoryError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn fingerprint_ignores_indentation_and_blank_lines() {
        let a = HunkFingerprint::new("src/a.rs", &lines(&["fn f() {", "    1", "", "}"]));
        let b = HunkFingerprint::new("src/a.rs", &lines(&["fn f() {", "\t1  ", "}"]));
        assert_eq!(a, b);
        assert_eq!(a.content_hash.len(), 64);
    }

    #[test]
    fn fingerprint_depends_on_path_and_content() {
        let a = HunkFingerprint::new("src/a.rs", &lines(&["x"]));
        assert_ne!(a, HunkFingerprint::new("src/b.rs", &lines(&["x"])));
        assert_ne!(a, HunkFingerprint::new("src/a.rs", &lines(&["y"])));
    }

    #[test]
    fn null_store_has_no_record() {
        assert_eq!(
            NullHistoryStore
                .track_record("claude-code", "default")
                .unwrap(),
            None
        );
    }
}
