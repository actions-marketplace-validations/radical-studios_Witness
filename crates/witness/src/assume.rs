//! `witness assume`: the agent records something it relied on without
//! verifying it.

use anyhow::{Context, Result, anyhow, bail};
use time::OffsetDateTime;
use witness_core::command::redact;
use witness_core::record::{Event, EventKind, MAX_ASSUMPTION_CHARS, truncate_chars};

use crate::cli::AssumeArgs;
use crate::workspace::{self, parse_lines, repo_path};

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

    let (session, agent) = workspace::session_for(&root, args.session, None);

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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use witness_core::record::LineRange;

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

    #[cfg(unix)]
    #[test]
    fn symlinked_spellings_of_the_root_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("src")).unwrap();
        std::fs::write(real.join("src/a.rs"), "").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let via_real = real.join("src/a.rs");
        assert_eq!(
            repo_path(&link, &link, via_real.to_str().unwrap()).unwrap(),
            "src/a.rs"
        );
        // A file that doesn't exist yet still resolves through its parent.
        let new_file = real.join("src/new.rs");
        assert_eq!(
            repo_path(&link, &link, new_file.to_str().unwrap()).unwrap(),
            "src/new.rs"
        );
    }
}
