//! The session log record format. See `docs/RECORD_FORMAT.md`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::scope::Scope;
use crate::testsum::{RunOutcome, TestCounts};

/// Version written by this build. Readers skip lines with a higher version.
pub const SCHEMA_VERSION: u32 = 1;

/// Agent name recorded for Claude Code sessions.
pub const AGENT_CLAUDE_CODE: &str = "claude-code";

/// Maximum stored length of command text, in characters.
pub const MAX_COMMAND_CHARS: usize = 200;

/// Maximum stored length of assumption text, in characters.
pub const MAX_ASSUMPTION_CHARS: usize = 500;

/// One line of a session log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u32,
    pub session_id: String,
    #[serde(with = "timestamp")]
    pub timestamp: OffsetDateTime,
    pub agent: String,
    /// The agent's id for the turn (user prompt) this happened in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    /// Subagent type (or id) when a subagent did this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    /// Model name, when the agent reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// What happened. Serialized with a `kind` tag next to the common fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    Edit {
        file: String,
        /// `None` means the whole file.
        lines: Option<LineRange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change: Option<Change>,
        tool: String,
        /// How the change was observed; absent in older logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<EditVia>,
        /// [`line_hash`] of each line the edit added, in order.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_hashes: Option<Vec<String>>,
        /// [`line_hash`] of each line the edit removed, in order.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        removed_hashes: Option<Vec<String>>,
    },
    Read {
        file: String,
    },
    Command {
        classification: CommandClass,
        exit_code: Option<i32>,
        command: String,
        /// Whether the test or build passed: from an attributable exit code,
        /// else from the runner's summary in the output. Absent if unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<RunOutcome>,
        /// Test counts from the runner's summary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tests: Option<TestCounts>,
        /// What the run covers; absent for the whole repository.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<Scope>,
    },
    Assumption {
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<LineRange>,
        text: String,
        /// [`line_hash`] of each line in `lines` when it was recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line_hashes: Option<Vec<String>>,
    },
    SessionEnd {
        reason: String,
    },
    /// Something about capture itself, e.g. that file snapshots were turned
    /// off for being too slow. Informational.
    Note {
        text: String,
    },
}

/// Where an edit's line range came from, most precise first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditVia {
    /// The agent's own patch of the edit.
    Patch,
    /// The file before the tool ran, diffed with the file after it.
    Preimage,
    /// Working-tree snapshots before and after a shell or unknown tool.
    Snapshot,
    /// The full content of a newly created file.
    Content,
    /// The new text, located in the file after the edit.
    Locate,
    /// Stated by the agent or a wrapper script (`witness record edit`).
    Reported,
    /// Not known; the edit covers the whole file.
    Unknown,
}

/// Inclusive, 1-based line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    /// Builds a range, swapping the bounds if needed and clamping to line 1.
    pub fn new(a: u32, b: u32) -> Self {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        Self {
            start: start.max(1),
            end: end.max(1),
        }
    }

    pub fn overlaps(&self, other: &LineRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

impl std::fmt::Display for LineRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.start == self.end {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}-{}", self.start, self.end)
        }
    }
}

/// An edit replaced `removed` lines starting at `start` with `added` lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub start: u32,
    pub removed: u32,
    pub added: u32,
}

impl Change {
    /// The line range this change occupies after the edit. A pure deletion
    /// covers the two lines around the gap.
    pub fn new_range(&self) -> LineRange {
        if self.added == 0 {
            LineRange::new(self.start.saturating_sub(1), self.start)
        } else {
            LineRange::new(self.start, self.start + self.added - 1)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandClass {
    Other,
    Lint,
    Build,
    Test,
}

impl CommandClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            CommandClass::Test => "test",
            CommandClass::Build => "build",
            CommandClass::Lint => "lint",
            CommandClass::Other => "other",
        }
    }

    /// Whether a passing run of this class counts as verifying behavior.
    pub fn verifies(&self) -> bool {
        matches!(self, CommandClass::Test | CommandClass::Build)
    }
}

impl Event {
    pub fn new(session_id: &str, agent: &str, timestamp: OffsetDateTime, kind: EventKind) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            session_id: session_id.to_owned(),
            timestamp,
            agent: agent.to_owned(),
            turn: None,
            subagent: None,
            model: None,
            kind,
        }
    }

    /// For a command: whether it passed, from `outcome`, or from
    /// `exit_code` in older logs.
    pub fn run_outcome(&self) -> Option<RunOutcome> {
        match &self.kind {
            EventKind::Command {
                outcome, exit_code, ..
            } => outcome.or_else(|| {
                exit_code.map(|c| {
                    if c == 0 {
                        RunOutcome::Passed
                    } else {
                        RunOutcome::Failed
                    }
                })
            }),
            _ => None,
        }
    }

    /// The file this event is about, if any.
    pub fn file(&self) -> Option<&str> {
        match &self.kind {
            EventKind::Edit { file, .. }
            | EventKind::Read { file }
            | EventKind::Assumption { file, .. } => Some(file),
            EventKind::Command { .. } | EventKind::SessionEnd { .. } | EventKind::Note { .. } => {
                None
            }
        }
    }

    /// Serializes to one JSON line, without the trailing newline.
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Hex digits kept of a line's SHA-256 (40 bits): enough to tell lines of a
/// file apart, too few to recover a line that isn't already in the repo.
pub const LINE_HASH_HEX: usize = 10;

/// Identifies a line's content independently of where it sits: the start
/// of the SHA-256 of the line with surrounding whitespace trimmed.
pub fn line_hash(line: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(line.trim().as_bytes());
    digest
        .iter()
        .take(LINE_HASH_HEX / 2)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// [`line_hash`] of each line.
pub fn line_hashes<S: AsRef<str>>(lines: &[S]) -> Vec<String> {
    lines.iter().map(|l| line_hash(l.as_ref())).collect()
}

/// Result of parsing a session log.
#[derive(Debug, Default)]
pub struct ParsedLog {
    pub events: Vec<Event>,
    /// Lines that were not valid records, had an unknown kind, or a newer
    /// schema version.
    pub skipped: usize,
}

/// Parses a JSONL session log, skipping lines it can't use.
pub fn parse_log(text: &str) -> ParsedLog {
    let mut parsed = ParsedLog::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(line) {
            Ok(event) if event.schema_version <= SCHEMA_VERSION => parsed.events.push(event),
            _ => parsed.skipped += 1,
        }
    }
    parsed
}

/// Turns an agent session id into a safe file stem.
pub fn session_file_stem(session_id: &str) -> String {
    let stem: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() {
        "unknown".to_owned()
    } else {
        stem
    }
}

/// Truncates to at most `max` characters, on a char boundary.
pub fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((idx, _)) => text[..idx].to_owned(),
        None => text.to_owned(),
    }
}

/// RFC 3339 with millisecond precision in UTC, e.g. `2026-10-20T03:37:00.123Z`.
pub mod timestamp {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::format_description::well_known::Rfc3339;
    use time::macros::format_description;
    use time::{OffsetDateTime, UtcOffset};

    pub fn format(ts: &OffsetDateTime) -> Result<String, time::error::Format> {
        ts.to_offset(UtcOffset::UTC).format(format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        ))
    }

    pub fn parse(text: &str) -> Result<OffsetDateTime, time::error::Parse> {
        OffsetDateTime::parse(text, &Rfc3339)
    }

    pub fn serialize<S: Serializer>(ts: &OffsetDateTime, s: S) -> Result<S::Ok, S::Error> {
        let text = format(ts).map_err(serde::ser::Error::custom)?;
        s.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<OffsetDateTime, D::Error> {
        let text = String::deserialize(d)?;
        parse(&text).map_err(serde::de::Error::custom)
    }

    /// The same format for an optional timestamp.
    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};
        use time::OffsetDateTime;

        pub fn serialize<S: Serializer>(
            ts: &Option<OffsetDateTime>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match ts {
                Some(ts) => super::serialize(ts, s),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<OffsetDateTime>, D::Error> {
            Option::<String>::deserialize(d)?
                .map(|text| super::parse(&text).map_err(serde::de::Error::custom))
                .transpose()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn ts() -> OffsetDateTime {
        datetime!(2026-10-20 03:37:00.123 UTC)
    }

    #[test]
    fn edit_round_trips_with_documented_shape() {
        let event = Event::new(
            "s1",
            AGENT_CLAUDE_CODE,
            ts(),
            EventKind::Edit {
                file: "src/a.rs".into(),
                lines: Some(LineRange::new(12, 18)),
                change: Some(Change {
                    start: 12,
                    removed: 3,
                    added: 7,
                }),
                tool: "Edit".into(),
                via: None,
                line_hashes: None,
                removed_hashes: None,
            },
        );
        let line = event.to_line().unwrap();
        assert_eq!(
            line,
            r#"{"schema_version":1,"session_id":"s1","timestamp":"2026-10-20T03:37:00.123Z","agent":"claude-code","kind":"edit","file":"src/a.rs","lines":{"start":12,"end":18},"change":{"start":12,"removed":3,"added":7},"tool":"Edit"}"#
        );
        let parsed = parse_log(&line);
        assert_eq!(parsed.events, vec![event]);
    }

    #[test]
    fn optional_context_fields_round_trip() {
        let mut event = Event::new(
            "s1",
            "codex",
            ts(),
            EventKind::Edit {
                file: "a.rs".into(),
                lines: Some(LineRange::new(1, 2)),
                change: None,
                tool: "apply_patch".into(),
                via: Some(EditVia::Preimage),
                line_hashes: Some(vec![line_hash("a"), line_hash("b")]),
                removed_hashes: None,
            },
        );
        event.turn = Some("t1".into());
        event.subagent = Some("explore".into());
        event.model = Some("gpt-x".into());
        let line = event.to_line().unwrap();
        assert!(line.contains(r#""turn":"t1","subagent":"explore","model":"gpt-x","kind":"edit""#));
        assert!(line.contains(r#""via":"preimage""#));
        assert_eq!(parse_log(&line).events, vec![event]);
    }

    #[test]
    fn parse_log_skips_garbage_unknown_kinds_and_future_versions() {
        let text = concat!(
            r#"{"schema_version":1,"session_id":"s","timestamp":"2026-10-20T03:37:00.000Z","agent":"a","kind":"read","file":"x"}"#,
            "\n",
            "not json\n",
            r#"{"schema_version":1,"session_id":"s","timestamp":"2026-10-20T03:37:00.000Z","agent":"a","kind":"teleport"}"#,
            "\n",
            r#"{"schema_version":2,"session_id":"s","timestamp":"2026-10-20T03:37:00.000Z","agent":"a","kind":"read","file":"x"}"#,
            "\n",
            r#"{"schema_version":1,"session_id":"s","timestamp":"2026-10-20T03:37:00.000Z","agent":"a","kind":"command","classification":"test","exit_code":null,"command":"cargo test | tail"}"#,
            "\n",
            r#"{"schema_version":1,"session_id":"s","timestamp":"2026-10-20T03:37:00.000Z","agent":"a","kind":"read","fi"#,
        );
        let parsed = parse_log(text);
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.skipped, 4);
    }

    #[test]
    fn line_hashes_ignore_surrounding_whitespace() {
        assert_eq!(line_hash("  let x = 1;\t"), line_hash("let x = 1;"));
        assert_ne!(line_hash("let x = 1;"), line_hash("let x = 2;"));
        assert_eq!(line_hash("x").len(), LINE_HASH_HEX);
    }

    #[test]
    fn deletion_change_covers_lines_around_gap() {
        let change = Change {
            start: 10,
            removed: 4,
            added: 0,
        };
        assert_eq!(change.new_range(), LineRange::new(9, 10));
        let at_top = Change {
            start: 1,
            removed: 2,
            added: 0,
        };
        assert_eq!(at_top.new_range(), LineRange::new(1, 1));
    }

    #[test]
    fn session_stem_is_filesystem_safe() {
        assert_eq!(session_file_stem("abc-123_X"), "abc-123_X");
        assert_eq!(session_file_stem("../../etc/passwd"), "______etc_passwd");
        assert_eq!(session_file_stem(""), "unknown");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
        assert_eq!(truncate_chars("hi", 10), "hi");
    }
}
