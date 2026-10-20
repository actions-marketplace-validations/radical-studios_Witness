//! `witness doctor`: check that Witness is set up and recording.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::Result;
use time::OffsetDateTime;
use witness_core::record::{EventKind, parse_log};
use witness_git::{Git, GitCli};

use crate::hooks;
use crate::pipeline::Repo;
use crate::review::resolve_base;
use crate::workspace::{self, HOOK_LOG, SESSIONS_DIR};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

struct Report {
    lines: Vec<(Level, String)>,
}

impl Report {
    fn add(&mut self, level: Level, text: impl Into<String>) {
        self.lines.push((level, text.into()));
    }
}

pub fn run() -> Result<ExitCode> {
    let mut r = Report { lines: Vec::new() };
    let version = env!("CARGO_PKG_VERSION");
    r.add(Level::Info, format!("witness {version}"));
    check(&mut r, version);
    let color = std::io::IsTerminal::is_terminal(&std::io::stdout())
        && std::env::var_os("NO_COLOR").is_none();
    for (level, text) in &r.lines {
        let (mark, code) = match level {
            Level::Ok => ("✓", "32"),
            Level::Info => ("·", "2"),
            Level::Warn => ("!", "33"),
            Level::Fail => ("✗", "31"),
        };
        if color {
            println!("\x1b[{code}m{mark}\x1b[0m {text}");
        } else {
            println!("{mark} {text}");
        }
    }
    let worst = r.lines.iter().map(|(l, _)| *l).max().unwrap_or(Level::Ok);
    Ok(if worst == Level::Fail {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn check(r: &mut Report, version: &str) {
    match Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => {
            r.add(
                Level::Ok,
                String::from_utf8_lossy(&out.stdout).trim().to_owned(),
            );
        }
        _ => {
            r.add(Level::Fail, "git is not on PATH; Witness needs it");
            return;
        }
    }
    check_binary(r, version);

    let Ok(cwd) = std::env::current_dir() else {
        r.add(Level::Fail, "can't read the working directory");
        return;
    };
    if GitCli::new(&cwd).toplevel().is_err() {
        r.add(Level::Fail, "not inside a git repository");
        return;
    }
    let Some(root) = workspace::find_root(&cwd) else {
        r.add(
            Level::Fail,
            "no .witness/ directory here or above; run `witness init`",
        );
        return;
    };
    let repo = match Repo::at(&cwd) {
        Ok(repo) => {
            r.add(
                Level::Ok,
                format!("config: {}", root.join("witness.toml").display()),
            );
            repo
        }
        Err(err) => {
            r.add(Level::Fail, format!("config: {err:#}"));
            return;
        }
    };

    match resolve_base(&repo.git, None) {
        Ok(base) => r.add(Level::Ok, format!("base branch: {base}")),
        Err(err) => r.add(
            Level::Warn,
            format!("base branch: {err:#}; pass --base to reports"),
        ),
    }
    if repo.git.is_shallow() {
        r.add(
            Level::Warn,
            "shallow clone: reports need the merge base (git fetch --unshallow)",
        );
    }

    check_hooks(r, &root);
    check_plugin(r, version);
    check_logs(r, &repo, &root);
    check_hook_log(r, &root);
    if !fs::read_to_string(root.join(".gitattributes"))
        .is_ok_and(|t| t.contains(".witness/sessions/**"))
    {
        r.add(
            Level::Info,
            "tip: `witness init` marks session logs as generated in .gitattributes (collapsed in \
             pull request diffs)",
        );
    }
}

/// The `witness` the hooks run is the one on PATH; it should be this one.
fn check_binary(r: &mut Report, version: &str) {
    let Some(on_path) = hooks::which("witness") else {
        r.add(
            Level::Warn,
            "witness is not on PATH, so agent hooks do nothing; add its directory to PATH",
        );
        return;
    };
    let other = Command::new(&on_path)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    if other.ends_with(version) {
        r.add(Level::Ok, format!("hooks run {}", on_path.display()));
    } else {
        r.add(
            Level::Warn,
            format!(
                "hooks run {} ({other}), not this witness ({version})",
                on_path.display()
            ),
        );
    }
}

fn check_hooks(r: &mut Report, root: &Path) {
    let states = hooks::states(root);
    let active: Vec<String> = states
        .iter()
        .filter(|(_, _, state)| *state != "not installed")
        .map(|(agent, _, state)| format!("{agent} ({state})"))
        .collect();
    if active.is_empty() && !user_plugin_enabled() {
        r.add(
            Level::Warn,
            "no agent hooks installed; run `witness hooks install <agent>` or install the Claude \
             Code plugin",
        );
    } else if active.is_empty() {
        r.add(Level::Ok, "hooks: claude-code (plugin, user settings)");
    } else {
        r.add(Level::Ok, format!("hooks: {}", active.join(", ")));
    }
}

/// The Claude Code plugin, if installed for this user, should be the same
/// release as the binary.
fn check_plugin(r: &mut Report, version: &str) {
    let Some(home) = home_dir() else { return };
    let Some(plugin) = find_plugin_version(&home.join(".claude").join("plugins"), 0) else {
        return;
    };
    let release = |v: &str| v.split('.').take(2).collect::<Vec<_>>().join(".");
    if release(&plugin) == release(version) {
        r.add(Level::Ok, format!("Claude Code plugin {plugin}"));
    } else {
        r.add(
            Level::Warn,
            format!(
                "Claude Code plugin {plugin} and binary {version} are different releases; run \
                 `/plugin update witness` or update the binary"
            ),
        );
    }
}

fn find_plugin_version(dir: &Path, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    let manifest = dir.join(".claude-plugin").join("plugin.json");
    if let Ok(text) = fs::read_to_string(&manifest)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&text)
        && v.get("name").and_then(|n| n.as_str()) == Some("witness")
    {
        return v.get("version")?.as_str().map(str::to_owned);
    }
    let mut found: Vec<String> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| find_plugin_version(&e.path(), depth + 1))
        .collect();
    found.sort_by_key(|v| std::cmp::Reverse(version_key(v)));
    found.into_iter().next()
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn user_plugin_enabled() -> bool {
    home_dir()
        .and_then(|h| fs::read_to_string(h.join(".claude").join("settings.json")).ok())
        .is_some_and(|t| t.contains("\"witness@"))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn check_logs(r: &mut Report, repo: &Repo, root: &Path) {
    let dir = root.join(SESSIONS_DIR);
    let mut logs = 0;
    let mut skipped = 0;
    let mut throttled = 0;
    let mut latest: Vec<(String, OffsetDateTime)> = Vec::new();
    for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
        if entry.path().extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        logs += 1;
        let parsed = parse_log(&text);
        skipped += parsed.skipped;
        if parsed
            .events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::Note { text } if text.contains("napshot")))
        {
            throttled += 1;
        }
        for event in &parsed.events {
            match latest.iter_mut().find(|(a, _)| *a == event.agent) {
                Some((_, t)) if *t < event.timestamp => *t = event.timestamp,
                Some(_) => {}
                None => latest.push((event.agent.clone(), event.timestamp)),
            }
        }
    }
    if logs == 0 {
        r.add(
            Level::Warn,
            "no sessions recorded yet; start an agent session in this repository",
        );
    } else {
        latest.sort_by_key(|l| std::cmp::Reverse(l.1));
        let seen: Vec<String> = latest
            .iter()
            .map(|(agent, t)| format!("{agent} {}", ago(*t)))
            .collect();
        r.add(
            Level::Ok,
            format!(
                "{logs} session {}; last recorded: {}",
                plural(logs, "log", "logs"),
                seen.join(", ")
            ),
        );
    }
    if skipped > 0 {
        r.add(
            Level::Warn,
            format!("{skipped} unreadable lines in session logs (reports skip them)"),
        );
    }
    if throttled > 0 {
        r.add(
            Level::Warn,
            format!(
                "snapshots were turned off as too slow in {throttled} sessions; edits made \
                 through the shell there were not recorded (see [capture] snapshot_budget_ms)"
            ),
        );
    }
    let sessions = format!("{}{SESSIONS_DIR}", repo.prefix);
    if let Ok(changed) = repo.git.status_porcelain(&sessions) {
        let n = changed.lines().filter(|l| l.ends_with(".jsonl")).count();
        if n > 0 {
            r.add(
                Level::Info,
                format!(
                    "{n} session {} not committed yet; commit them with your changes",
                    plural(n, "log", "logs")
                ),
            );
        }
    }
}

fn check_hook_log(r: &mut Report, root: &Path) {
    let Ok(text) = fs::read_to_string(root.join(HOOK_LOG)) else {
        return;
    };
    let errors: Vec<&str> = text
        .lines()
        .filter(|l| l.contains(": error:") || l.contains("failed"))
        .collect();
    if let Some(last) = errors.last() {
        r.add(
            Level::Warn,
            format!(
                "{} hook errors in {HOOK_LOG}; latest: {}",
                errors.len(),
                last.chars().take(200).collect::<String>()
            ),
        );
    }
    let mut ms: Vec<u64> = text
        .lines()
        .filter_map(|l| l.rsplit_once("ok in ")?.1.strip_suffix(" ms")?.parse().ok())
        .collect();
    if !ms.is_empty() {
        ms.sort_unstable();
        let p50 = ms[ms.len() / 2];
        let max = ms[ms.len() - 1];
        let level = if p50 > 150 { Level::Warn } else { Level::Ok };
        r.add(
            level,
            format!(
                "hook timings: median {p50} ms, slowest {max} ms ({} calls)",
                ms.len()
            ),
        );
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

fn ago(t: OffsetDateTime) -> String {
    let secs = (OffsetDateTime::now_utc() - t).whole_seconds().max(0);
    match secs {
        s if s < 120 => "just now".to_owned(),
        s if s < 2 * 3600 => format!("{} min ago", s / 60),
        s if s < 2 * 86400 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}
