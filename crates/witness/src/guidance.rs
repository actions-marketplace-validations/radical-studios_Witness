//! Instructions for agents, kept as a marked section of the instruction file
//! each agent reads (`AGENTS.md`, `GEMINI.md`, `CLAUDE.md`). The Claude Code
//! plugin carries the same guidance as a skill.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

const BEGIN: &str = "<!-- witness:begin -->";
const END: &str = "<!-- witness:end -->";

/// The section between the markers.
const SECTION: &str = "\
## Witness

This repository uses [Witness](https://github.com/radical-studios/witness).
Hooks record the files you read, the edits you make and the commands you
run, and reviewers get a plan of which changes were tested and which were
guessed. Help it be accurate:

- After changing code, run the tests or build that cover it before you
  finish. If you pipe or filter the output, run it as
  `witness exec -- <command>` so the real result is recorded.
- When code relies on something you didn't check (how an API behaves, a
  data format, a default value, a business rule), record it right away:
  `witness assume --file <path> --lines <start-end> \"<one sentence>\"`.
  Never put secrets or personal data in it.
- Before you say you're done, run `witness status`. Check what it lists, or
  say in your reply why it can't be checked.
- Don't say the tests pass or the build succeeds unless you ran them after
  your last edit.
- Commit `.witness/sessions/` with your changes.
";

/// The instruction file `agent` reads, relative to the repository root.
pub fn file_for(agent: &str) -> Option<&'static str> {
    match agent {
        "codex" | "copilot" | "cursor" => Some("AGENTS.md"),
        "gemini-cli" => Some("GEMINI.md"),
        "claude-code" => Some("CLAUDE.md"),
        _ => None,
    }
}

/// Adds or refreshes the section in `path`; true if the file changed.
pub fn write(path: &Path) -> Result<bool> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let block = format!("{BEGIN}\n{SECTION}{END}\n");
    let updated = match span(&text) {
        Some((start, end)) => format!("{}{block}{}", &text[..start], &text[end..]),
        None if text.trim().is_empty() => block,
        None => format!("{}\n\n{block}", text.trim_end()),
    };
    if updated == text {
        return Ok(false);
    }
    fs::write(path, updated).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// Removes the section from `path`, and the file if nothing else is left;
/// true if the file changed.
pub fn remove(path: &Path) -> Result<bool> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(false);
    };
    let Some((start, end)) = span(&text) else {
        return Ok(false);
    };
    let (before, after) = (text[..start].trim(), text[end..].trim());
    let rest = match (before.is_empty(), after.is_empty()) {
        (true, true) => {
            fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
            return Ok(true);
        }
        (false, false) => format!("{before}\n\n{after}\n"),
        _ => format!("{before}{after}\n"),
    };
    fs::write(path, rest).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// Whether `path` has the section.
pub fn present(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|text| span(&text).is_some())
}

/// Byte range of the section, including the end marker's newline.
fn span(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BEGIN)?;
    let end = start + text[start..].find(END)? + END.len();
    let end = if text[end..].starts_with('\n') {
        end + 1
    } else {
        end
    };
    Some((start, end))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn writes_refreshes_and_removes_the_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        fs::write(&path, "# Project\n\nUse tabs.\n").unwrap();

        assert!(write(&path).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Project\n\nUse tabs.\n\n<!-- witness:begin -->\n## Witness"));
        assert!(text.ends_with("<!-- witness:end -->\n"));
        assert!(!write(&path).unwrap(), "already up to date");

        // An outdated section is replaced in place.
        let old = text.replace("witness status", "witness old");
        fs::write(&path, format!("{old}\nMore notes.\n")).unwrap();
        assert!(write(&path).unwrap());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("witness status") && !text.contains("witness old"));
        assert!(text.ends_with("<!-- witness:end -->\n\nMore notes.\n"));

        assert!(remove(&path).unwrap());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Project\n\nUse tabs.\n\nMore notes.\n"
        );
        assert!(!remove(&path).unwrap());
    }

    #[test]
    fn a_file_with_only_the_section_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        assert!(write(&path).unwrap());
        assert!(present(&path));
        assert!(remove(&path).unwrap());
        assert!(!path.exists());
    }
}
