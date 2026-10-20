//! Agent track record: what happened to agent-written sections after they
//! landed, and how well Witness's flags predicted it.
//!
//! A section *survived* if at least [`SURVIVAL_THRESHOLD`] of its lines are
//! still in the file at the horizon (`[history] horizon_days` after it
//! landed), was *reverted* if a revert commit names its landing commit
//! before then, and was *rewritten* otherwise. Lines are compared by
//! content hash, so moves within the file and renames don't count as
//! rewrites. History is built from git (see `witness stats`); this module
//! only aggregates it.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::classify::Status;
use crate::record::line_hash;

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

/// Share of a section's lines that must remain for it to have survived.
pub const SURVIVAL_THRESHOLD: f64 = 0.8;

/// Samples needed before a track record is shown or weighs on a score.
pub const MIN_SAMPLES_SHOWN: u32 = 5;
pub const MIN_SAMPLES_WEIGHED: u32 = 10;

/// What later happened to a section an agent wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Survived,
    Rewritten,
    Reverted,
}

impl Outcome {
    /// From the share of lines still present and whether the change was
    /// reverted.
    pub fn judge(survival: f64, reverted: bool) -> Self {
        if reverted {
            Outcome::Reverted
        } else if survival >= SURVIVAL_THRESHOLD {
            Outcome::Survived
        } else {
            Outcome::Rewritten
        }
    }
}

/// The share of `hashes` (line hashes of a section) found among the lines
/// of `later` (the file at the horizon; `None` if it was deleted).
pub fn survival(hashes: &[String], later: Option<&str>) -> f64 {
    if hashes.is_empty() {
        return 1.0;
    }
    let Some(text) = later else {
        return 0.0;
    };
    let present: HashSet<String> = text.lines().map(line_hash).collect();
    let kept = hashes.iter().filter(|h| present.contains(*h)).count();
    kept as f64 / hashes.len() as f64
}

/// One agent-written section of a landed change, and what became of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The commit that landed the change on the base branch.
    pub commit: String,
    #[serde(with = "crate::record::timestamp")]
    pub landed_at: OffsetDateTime,
    pub file: String,
    pub agent: String,
    pub area: String,
    /// The section's status when it landed.
    pub status: Status,
    pub lines: usize,
    pub outcome: Outcome,
}

/// Outcome counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    pub samples: u32,
    pub rewritten: u32,
    pub reverted: u32,
}

impl Tally {
    fn add(&mut self, outcome: Outcome) {
        self.samples += 1;
        match outcome {
            Outcome::Survived => {}
            Outcome::Rewritten => self.rewritten += 1,
            Outcome::Reverted => self.reverted += 1,
        }
    }

    /// Share of samples rewritten or reverted.
    pub fn churn(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            f64::from(self.rewritten + self.reverted) / f64::from(self.samples)
        }
    }
}

/// Aggregate for one agent in one area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackRecord {
    pub agent: String,
    pub area: String,
    #[serde(flatten)]
    pub tally: Tally,
}

impl TrackRecord {
    /// E.g. "claude-code in core: 12% of 40 sections rewritten or reverted".
    pub fn describe(&self) -> String {
        format!(
            "{} in {}: {} of {} {} rewritten or reverted",
            self.agent,
            self.area,
            percent(self.tally.churn()),
            self.tally.samples,
            if self.tally.samples == 1 {
                "section"
            } else {
                "sections"
            }
        )
    }
}

/// How often sections Witness flags were rewritten or reverted, compared
/// with tested ones.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    /// Sections whose status is for close review by default (failing,
    /// guessed, unrecorded, unverified).
    pub flagged: Tally,
    pub tested: Tally,
}

impl Calibration {
    /// One sentence, or `None` without enough samples on both sides.
    pub fn sentence(&self, horizon_days: u32) -> Option<String> {
        if self.flagged.samples < MIN_SAMPLES_WEIGHED || self.tested.samples < MIN_SAMPLES_WEIGHED {
            return None;
        }
        let (f, t) = (self.flagged.churn(), self.tested.churn());
        let samples = self.flagged.samples + self.tested.samples;
        let comparison = if t > 0.0 && f / t >= 1.2 {
            format!("{:.1}× as often as", f / t)
        } else if t == 0.0 && f > 0.0 {
            "more often than".to_owned()
        } else if (t - f).abs() < 0.05 || (f > 0.0 && t / f < 1.2) {
            "about as often as".to_owned()
        } else {
            "less often than".to_owned()
        };
        Some(format!(
            "Within {horizon_days} days, sections flagged for close review were rewritten or \
             reverted {comparison} tested ones ({} vs {}, {samples} sections).",
            percent(f),
            percent(t)
        ))
    }
}

/// Track-record history: evaluated sections of past changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    pub horizon_days: u32,
    pub entries: Vec<HistoryEntry>,
}

impl History {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Landed changes the entries come from.
    pub fn changes(&self) -> usize {
        self.entries
            .iter()
            .map(|e| e.commit.as_str())
            .collect::<HashSet<_>>()
            .len()
    }

    pub fn track_record(&self, agent: &str, area: &str) -> Option<TrackRecord> {
        let mut tally = Tally::default();
        for e in self
            .entries
            .iter()
            .filter(|e| e.agent == agent && e.area == area)
        {
            tally.add(e.outcome);
        }
        (tally.samples > 0).then(|| TrackRecord {
            agent: agent.to_owned(),
            area: area.to_owned(),
            tally,
        })
    }

    /// Every (agent, area) pair, most samples first.
    pub fn track_records(&self) -> Vec<TrackRecord> {
        let mut by: BTreeMap<(&str, &str), Tally> = BTreeMap::new();
        for e in &self.entries {
            by.entry((&e.agent, &e.area)).or_default().add(e.outcome);
        }
        let mut records: Vec<TrackRecord> = by
            .into_iter()
            .map(|((agent, area), tally)| TrackRecord {
                agent: agent.to_owned(),
                area: area.to_owned(),
                tally,
            })
            .collect();
        records.sort_by_key(|r| std::cmp::Reverse(r.tally.samples));
        records
    }

    /// Outcomes per status at landing, in [`Status::ALL`] order.
    pub fn by_status(&self) -> Vec<(Status, Tally)> {
        Status::ALL
            .iter()
            .filter_map(|status| {
                let mut tally = Tally::default();
                for e in self.entries.iter().filter(|e| e.status == *status) {
                    tally.add(e.outcome);
                }
                (tally.samples > 0).then_some((*status, tally))
            })
            .collect()
    }

    /// All agent sections together.
    pub fn overall(&self) -> Tally {
        let mut tally = Tally::default();
        for e in &self.entries {
            tally.add(e.outcome);
        }
        tally
    }

    pub fn calibration(&self) -> Calibration {
        let mut flagged = Tally::default();
        let mut tested = Tally::default();
        for e in &self.entries {
            match e.status {
                Status::Failing | Status::Guessed | Status::Unrecorded | Status::Unverified => {
                    flagged.add(e.outcome);
                }
                Status::Tested => tested.add(e.outcome),
                Status::Compiled | Status::Human => {}
            }
        }
        Calibration { flagged, tested }
    }

    /// The track record of `agent` in `area` when it is notably worse than
    /// the repository's overall: enough samples, and a churn rate at least
    /// 25% and 1.5 times the overall one.
    pub fn weak_spot(&self, agent: &str, area: &str) -> Option<TrackRecord> {
        let record = self.track_record(agent, area)?;
        let churn = record.tally.churn();
        let overall = self.overall().churn();
        (record.tally.samples >= MIN_SAMPLES_WEIGHED && churn >= 0.25 && churn >= overall * 1.5)
            .then_some(record)
    }
}

/// `0.123` → `"12%"`.
pub fn percent(share: f64) -> String {
    format!("{:.0}%", share * 100.0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::record::line_hashes;

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
    fn survival_counts_lines_still_present_anywhere_in_the_file() {
        let hashes = line_hashes(&lines(&[
            "let total = price * qty;",
            "apply_discount(&mut total);",
            "    return total;",
            "log::debug!(\"total {total}\");",
            "assert!(total >= 0);",
        ]));
        // Moved and re-indented: still there.
        let later = "fn f() {\n  assert!(total >= 0);\nlet total = price * qty;\n apply_discount(&mut total);\nreturn total;\nlog::debug!(\"total {total}\");\n}\n";
        assert!((survival(&hashes, Some(later)) - 1.0).abs() < 1e-9);
        let rewritten = "let total = price.checked_mul(qty)?;\nreturn total;\n";
        assert!((survival(&hashes, Some(rewritten)) - 0.2).abs() < 1e-9);
        assert!(survival(&hashes, None).abs() < 1e-9);
        assert_eq!(Outcome::judge(0.8, false), Outcome::Survived);
        assert_eq!(Outcome::judge(0.79, false), Outcome::Rewritten);
        assert_eq!(Outcome::judge(1.0, true), Outcome::Reverted);
    }

    fn entry(agent: &str, area: &str, status: Status, outcome: Outcome) -> HistoryEntry {
        HistoryEntry {
            commit: format!("{agent}{area}"),
            landed_at: OffsetDateTime::UNIX_EPOCH,
            file: "src/a.rs".into(),
            agent: agent.into(),
            area: area.into(),
            status,
            lines: 3,
            outcome,
        }
    }

    fn history(spec: &[(&str, &str, Status, Outcome, usize)]) -> History {
        let mut entries = Vec::new();
        for (agent, area, status, outcome, n) in spec {
            for _ in 0..*n {
                entries.push(entry(agent, area, *status, *outcome));
            }
        }
        History {
            horizon_days: 30,
            entries,
        }
    }

    #[test]
    fn aggregates_per_agent_area_and_status() {
        use Outcome::*;
        use Status::*;
        let h = history(&[
            ("claude-code", "core", Tested, Survived, 9),
            ("claude-code", "core", Unverified, Rewritten, 3),
            ("claude-code", "core", Unverified, Survived, 1),
            ("codex", "ui", Guessed, Reverted, 2),
        ]);
        let core = h.track_record("claude-code", "core").unwrap();
        assert_eq!(
            core.tally,
            Tally {
                samples: 13,
                rewritten: 3,
                reverted: 0
            }
        );
        assert_eq!(
            core.describe(),
            "claude-code in core: 23% of 13 sections rewritten or reverted"
        );
        assert_eq!(h.track_record("codex", "core"), None);
        assert_eq!(h.track_records()[0].agent, "claude-code");
        assert_eq!(h.changes(), 2);
        let by_status = h.by_status();
        assert_eq!(by_status.len(), 3);
        assert_eq!(by_status[0].0, Guessed);
    }

    #[test]
    fn calibration_compares_flagged_with_tested() {
        use Outcome::*;
        use Status::*;
        let h = history(&[
            ("a", "x", Tested, Survived, 18),
            ("a", "x", Tested, Rewritten, 2),
            ("a", "x", Unverified, Rewritten, 4),
            ("a", "x", Guessed, Reverted, 2),
            ("a", "x", Unverified, Survived, 6),
        ]);
        let sentence = h.calibration().sentence(30).unwrap();
        assert_eq!(
            sentence,
            "Within 30 days, sections flagged for close review were rewritten or reverted 5.0× \
             as often as tested ones (50% vs 10%, 32 sections)."
        );
        // Too few flagged samples: no claim either way.
        let h = history(&[
            ("a", "x", Tested, Survived, 20),
            ("a", "x", Unverified, Rewritten, 3),
        ]);
        assert_eq!(h.calibration().sentence(30), None);
        // Honest when the flags don't predict anything.
        let h = history(&[
            ("a", "x", Tested, Rewritten, 3),
            ("a", "x", Tested, Survived, 7),
            ("a", "x", Unverified, Rewritten, 3),
            ("a", "x", Unverified, Survived, 7),
        ]);
        assert!(
            h.calibration()
                .sentence(30)
                .unwrap()
                .contains("about as often as")
        );
    }

    #[test]
    fn weak_spots_stand_out_from_the_repository() {
        use Outcome::*;
        use Status::*;
        let h = history(&[
            ("a", "core", Tested, Survived, 40),
            ("a", "core", Tested, Rewritten, 2),
            ("a", "billing", Tested, Rewritten, 5),
            ("a", "billing", Tested, Survived, 7),
        ]);
        assert!(h.weak_spot("a", "billing").is_some());
        assert!(h.weak_spot("a", "core").is_none());
        assert!(h.weak_spot("b", "billing").is_none());
    }
}
