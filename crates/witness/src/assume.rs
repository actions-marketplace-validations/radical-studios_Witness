//! `witness assume`: the agent records something it relied on without
//! verifying it.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use time::OffsetDateTime;
use witness_core::capture::{normalize, relative_to};
use witness_core::command::redact;
use witness_core::record::{
    AGENT_CLAUDE_CODE, Event, EventKind, LineRange, MAX_ASSUMPTION_CHARS, truncate_chars,
};

use crate::cli::AssumeArgs;
use crate::workspace;

pub fn run(args: AssumeArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("finding the working directory")?;
    let root = workspace::find_root(&cwd).ok_or_else(|| {
        anyhow!("no .witness/ directory found; run `witness init` at the repo root")
    })?;

    let file = repo_path(&root, &cwd, &args.file)?;
    if !root.join(&file).exists() {
        eprintln!("witness: warning: {file} does not exist (recording anyway)");
    }
    let lines = args.lines.as_deref().map(parse_lines).transpose()?;
    let text = args.text.join(" ");
    let text = truncate_chars(&redact(text.trim()), MAX_ASSUMPTION_CHARS);
    if text.is_empty() {
        bail!("assumption text is empty");
    }

    let current = workspace::read_current_session(&root);
    let session = args
        .session
        .or_else(|| {
            std::env::var("WITNESS_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| current.as_ref().map(|(s, _)| s.clone()))
        .unwrap_or_else(|| "manual".to_owned());
    let agent = current
        .map(|(_, agent)| agent)
        .unwrap_or_else(|| AGENT_CLAUDE_CODE.to_owned());

    let event = Event::new(
        &session,
        &agent,
        OffsetDateTime::now_utc(),
        EventKind::Assumption {
            file: file.clone(),
            lines,
            text,
        },
    );
    workspace::append_events(&root, &[event])?;
    match lines {
        Some(range) => println!("witness: recorded assumption for {file}:{range}"),
        None => println!("witness: recorded assumption for {file}"),
    }
    Ok(())
}

/// `10-24` or `7`.
fn parse_lines(text: &str) -> Result<LineRange> {
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

fn repo_path(root: &Path, cwd: &Path, file: &str) -> Result<String> {
    let path = Path::new(file);
    let abs = if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&cwd.join(path))
    };
    relative_to(&abs, &normalize(root))
        .ok_or_else(|| anyhow!("{file} is outside the repository at {}", root.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_line_ranges() {
        assert_eq!(parse_lines("10-24").unwrap(), LineRange::new(10, 24));
        assert_eq!(parse_lines("7").unwrap(), LineRange::new(7, 7));
        assert_eq!(parse_lines("24-10").unwrap(), LineRange::new(10, 24));
        assert!(parse_lines("0").is_err());
        assert!(parse_lines("a-b").is_err());
    }

    #[test]
    fn repo_paths_are_relative_to_root() {
        let root = Path::new("/r");
        assert_eq!(
            repo_path(root, Path::new("/r/src"), "a.rs").unwrap(),
            "src/a.rs"
        );
        assert_eq!(
            repo_path(root, Path::new("/r/src"), "../b.rs").unwrap(),
            "b.rs"
        );
        assert_eq!(repo_path(root, Path::new("/x"), "/r/c.rs").unwrap(), "c.rs");
        assert!(repo_path(root, Path::new("/r"), "../outside").is_err());
    }
}
