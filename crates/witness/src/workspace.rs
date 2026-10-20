//! Locating the repo root and the files under `.witness/`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use witness_core::Config;
use witness_core::capture::{normalize, relative_to};
use witness_core::record::{Event, LineRange, session_file_stem};

pub use witness_core::config::{CONFIG_FILE, WITNESS_DIR};
pub const SESSIONS_DIR: &str = ".witness/sessions";
pub const STATE_DIR: &str = ".witness/state";
const CURRENT_SESSION: &str = ".witness/state/current-session";
pub const HOOK_LOG: &str = ".witness/state/hook.log";
/// The hook log is trimmed to its newest half once it grows past this.
const HOOK_LOG_MAX_BYTES: u64 = 64 * 1024;

/// Nearest ancestor of `start` (inclusive) that contains `.witness/`.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(WITNESS_DIR).is_dir())
        .map(Path::to_path_buf)
}

/// The directory whose `.witness/` applies to `cwd`, and its path relative
/// to the git top level (empty, or `/`-terminated). Falls back to the top
/// level when no `.witness/` sits at or below it.
pub fn witness_root(cwd: &Path, toplevel: &Path) -> (PathBuf, String) {
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if let Some(root) = find_root(cwd)
        && let Ok(rel) = canonical(&root).strip_prefix(canonical(toplevel))
    {
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let prefix = if parts.is_empty() {
            String::new()
        } else {
            format!("{}/", parts.join("/"))
        };
        return (root, prefix);
    }
    (toplevel.to_path_buf(), String::new())
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

/// Appends a line to the local hook log (`.witness/state/hook.log`), which
/// `witness doctor` reads. Never fails: the hook must not.
pub fn log_hook(root: &Path, message: &str) {
    let path = root.join(HOOK_LOG);
    let _ = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::metadata(&path).is_ok_and(|m| m.len() > HOOK_LOG_MAX_BYTES) {
            let text = fs::read_to_string(&path).unwrap_or_default();
            // Cut after a newline byte, which is always a char boundary.
            let half = text.len() / 2;
            let keep = text.as_bytes()[half..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or("", |i| &text[half + i + 1..]);
            fs::write(&path, keep)?;
        }
        let stamp = time::OffsetDateTime::now_utc();
        let stamp = witness_core::record::timestamp::format(&stamp).unwrap_or_default();
        let line = format!("{stamp} {}\n", message.replace('\n', " "));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(line.as_bytes())
    })();
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

/// Line hashes of `range` in `file` (repo-relative) as it is now, for
/// events recorded by hand.
pub fn range_hashes(root: &Path, file: &str, range: Option<LineRange>) -> Option<Vec<String>> {
    let range = range?;
    let text = fs::read_to_string(root.join(file)).ok()?;
    let lines: Vec<&str> = text
        .lines()
        .skip(range.start as usize - 1)
        .take((range.end - range.start + 1) as usize)
        .collect();
    (!lines.is_empty()).then(|| witness_core::record::line_hashes(&lines))
}

/// The session and agent to record a manual event (`witness assume`,
/// `record`, `exec`) under: `session`, else `$WITNESS_SESSION_ID`, else the
/// session the hooks saw last; the agent is `agent`, else that session's.
pub fn session_for(
    root: &Path,
    session: Option<String>,
    agent: Option<String>,
) -> (String, String) {
    let current = read_current_session(root);
    let session = session
        .or_else(|| {
            std::env::var("WITNESS_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| current.as_ref().map(|(s, _)| s.clone()))
        .unwrap_or_else(|| "manual".to_owned());
    let agent = agent
        .or_else(|| current.map(|(_, agent)| agent))
        .unwrap_or_else(|| "manual".to_owned());
    (session, agent)
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

/// `10-24` or `7`.
pub fn parse_lines(text: &str) -> Result<LineRange> {
    let parse = |s: &str| -> Result<u32> {
        let n: u32 = s
            .trim()
            .parse()
            .with_context(|| format!("invalid line number {s:?} in --lines {text:?}"))?;
        if n == 0 {
            bail!("line numbers start at 1 (--lines {text:?})");
        }
        Ok(n)
    };
    match text.split_once('-') {
        Some((a, b)) => Ok(LineRange::new(parse(a)?, parse(b)?)),
        None => {
            let n = parse(text)?;
            Ok(LineRange::new(n, n))
        }
    }
}

pub fn repo_path(root: &Path, cwd: &Path, file: &str) -> Result<String> {
    let path = Path::new(file);
    let abs = if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&cwd.join(path))
    };
    // Either side may spell a symlinked directory differently (e.g.
    // `/var` and `/private/var` on macOS); fall back to resolved paths.
    let canonical = |p: &Path| std::fs::canonicalize(p).ok();
    relative_to(&abs, &normalize(root))
        .or_else(|| relative_to(&canonical(&abs)?, &canonical(root)?))
        .or_else(|| {
            let parent = canonical(abs.parent()?)?;
            relative_to(&parent.join(abs.file_name()?), &canonical(root)?)
        })
        .ok_or_else(|| anyhow!("{file} is outside the repository at {}", root.display()))
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
    fn witness_root_is_relative_to_toplevel() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path();
        fs::create_dir_all(top.join("pkg/app/.witness")).unwrap();
        fs::create_dir_all(top.join("pkg/app/src")).unwrap();
        let (root, prefix) = witness_root(&top.join("pkg/app/src"), top);
        assert_eq!(root, top.join("pkg/app"));
        assert_eq!(prefix, "pkg/app/");

        let other = tempfile::tempdir().unwrap();
        let (root, prefix) = witness_root(other.path(), other.path());
        assert_eq!(root, other.path());
        assert_eq!(prefix, "");
    }

    #[test]
    fn hook_log_stays_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let line = "x".repeat(1000);
        for _ in 0..200 {
            log_hook(tmp.path(), &line);
        }
        let text = fs::read_to_string(tmp.path().join(HOOK_LOG)).unwrap();
        assert!(text.len() as u64 <= HOOK_LOG_MAX_BYTES + 2000);
        assert!(text.lines().all(|l| l.ends_with(&line)));

        // Multi-byte text is cut at a line boundary, not inside a char.
        let wide = "é".repeat(777);
        for _ in 0..200 {
            log_hook(tmp.path(), &wide);
        }
        let text = fs::read_to_string(tmp.path().join(HOOK_LOG)).unwrap();
        assert!(text.lines().last().unwrap().ends_with(&wide));
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
