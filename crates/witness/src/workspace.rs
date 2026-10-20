//! Locating the repo root and the files under `.witness/`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use witness_core::Config;
use witness_core::record::{Event, session_file_stem};

pub const WITNESS_DIR: &str = ".witness";
pub const SESSIONS_DIR: &str = ".witness/sessions";
pub const CONFIG_FILE: &str = "witness.toml";
const CURRENT_SESSION: &str = ".witness/state/current-session";

/// Nearest ancestor of `start` (inclusive) that contains `.witness/`.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(WITNESS_DIR).is_dir())
        .map(Path::to_path_buf)
}

/// Loads `witness.toml` from `root`, or defaults if it is missing.
pub fn load_config(root: &Path) -> Result<Config> {
    let path = root.join(CONFIG_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => Config::parse(&text).with_context(|| format!("reading {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

/// Appends events to their session's log in one write.
pub fn append_events(root: &Path, events: &[Event]) -> Result<()> {
    let Some(first) = events.first() else {
        return Ok(());
    };
    let dir = root.join(SESSIONS_DIR);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.jsonl", session_file_stem(&first.session_id)));
    let mut buf = String::new();
    for event in events {
        buf.push_str(&event.to_line()?);
        buf.push('\n');
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(buf.as_bytes())
        .with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

/// The session `witness assume` records into by default: `session\tagent`.
pub fn write_current_session(root: &Path, session_id: &str, agent: &str) -> Result<()> {
    let path = root.join(CURRENT_SESSION);
    let line = format!("{session_id}\t{agent}\n");
    if fs::read_to_string(&path).is_ok_and(|old| old == line) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, line).with_context(|| format!("writing {}", path.display()))
}

pub fn read_current_session(root: &Path) -> Option<(String, String)> {
    let text = fs::read_to_string(root.join(CURRENT_SESSION)).ok()?;
    let line = text.lines().next()?;
    let (session, agent) = line.split_once('\t').unwrap_or((line, ""));
    let session = session.trim();
    if session.is_empty() {
        return None;
    }
    let agent = match agent.trim() {
        "" => witness_core::record::AGENT_CLAUDE_CODE,
        a => a,
    };
    Some((session.to_owned(), agent.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use witness_core::record::EventKind;

    #[test]
    fn finds_root_from_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".witness")).unwrap();
        fs::create_dir_all(tmp.path().join("a/b")).unwrap();
        assert_eq!(
            find_root(&tmp.path().join("a/b")).as_deref(),
            Some(tmp.path())
        );
    }

    #[test]
    fn appends_lines_and_tracks_current_session() {
        let tmp = tempfile::tempdir().unwrap();
        let ev = Event::new(
            "s/1",
            "claude-code",
            time::OffsetDateTime::UNIX_EPOCH,
            EventKind::Read { file: "a".into() },
        );
        append_events(tmp.path(), std::slice::from_ref(&ev)).unwrap();
        append_events(tmp.path(), &[ev]).unwrap();
        let text = fs::read_to_string(tmp.path().join(".witness/sessions/s_1.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);

        assert_eq!(read_current_session(tmp.path()), None);
        write_current_session(tmp.path(), "s/1", "claude-code").unwrap();
        assert_eq!(
            read_current_session(tmp.path()),
            Some(("s/1".into(), "claude-code".into()))
        );
    }
}
