//! `witness prune`: remove old session logs that have landed, and stale
//! local state.
//!
//! A log is removed only when its last event is older than the cutoff and
//! the base branch has the same content, so the change it records has
//! landed: reports only read a branch's own additions, and the track
//! record reads logs from history, so neither needs it anymore.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use time::OffsetDateTime;
use witness_core::record::parse_log;
use witness_git::Git;

use crate::cli::PruneArgs;
use crate::pipeline::Repo;
use crate::review::resolve_base;
use crate::workspace::{SESSIONS_DIR, STATE_DIR};

const DAY: u64 = 24 * 60 * 60;

pub fn run(args: PruneArgs) -> Result<()> {
    let days = parse_days(&args.older_than)?;
    let repo = Repo::open()?;
    let base = resolve_base(&repo.git, args.base.as_deref())?;
    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(i64::from(days));
    let verb = if args.dry_run {
        "would remove"
    } else {
        "removed"
    };

    let dir = repo.root.join(SESSIONS_DIR);
    let mut removed = 0;
    let mut entries: Vec<_> = fs::read_dir(&dir).into_iter().flatten().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let last = parse_log(&text).events.iter().map(|e| e.timestamp).max();
        if last.is_none_or(|t| t >= cutoff) {
            continue;
        }
        let path = format!("{}{SESSIONS_DIR}/{name}", repo.prefix);
        if repo.git.show_file(&base, &path)?.as_deref() != Some(text.as_str()) {
            continue;
        }
        if !args.dry_run {
            fs::remove_file(entry.path())?;
        }
        println!("witness: {verb} {SESSIONS_DIR}/{name}");
        removed += 1;
    }

    let state = repo.root.join(STATE_DIR);
    let mut stale = 0;
    for (sub, max_days) in [("pre", 1), ("reminders", 7), ("last", 30)] {
        stale += remove_older(&state.join(sub), max_days, args.dry_run);
    }

    match (removed, args.dry_run) {
        (0, _) => println!("witness: no landed session logs older than {days} days"),
        (n, false) => println!(
            "witness: removed {n} session {}; commit the deletion",
            if n == 1 { "log" } else { "logs" }
        ),
        (_, true) => {}
    }
    if stale > 0 {
        println!("witness: {verb} {stale} stale files from {STATE_DIR}/");
    }
    Ok(())
}

/// Files in `dir` not modified for `days`; removed unless `dry_run`.
fn remove_older(dir: &Path, days: u64, dry_run: bool) -> usize {
    let Some(limit) = SystemTime::now().checked_sub(Duration::from_secs(days * DAY)) else {
        return 0;
    };
    let mut n = 0;
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t < limit);
        if old && entry.file_type().is_ok_and(|t| t.is_file()) {
            if !dry_run {
                let _ = fs::remove_file(entry.path());
            }
            n += 1;
        }
    }
    n
}

/// `90d`, `12w` or `90` (days).
fn parse_days(text: &str) -> Result<u32> {
    let text = text.trim();
    let (number, unit) = match text.strip_suffix('d') {
        Some(n) => (n, 1),
        None => match text.strip_suffix('w') {
            Some(n) => (n, 7),
            None => (text, 1),
        },
    };
    match number.parse::<u32>() {
        Ok(n) => Ok(n * unit),
        Err(_) => bail!("--older-than: expected days like 90d or weeks like 12w, got {text:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_days("90d").unwrap(), 90);
        assert_eq!(parse_days("12w").unwrap(), 84);
        assert_eq!(parse_days("30").unwrap(), 30);
        assert!(parse_days("3 months").is_err());
    }
}
