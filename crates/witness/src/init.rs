//! `witness init`: set a repository up for Witness.
//!
//! Writes `witness.toml` and `.witness/`, marks session logs as generated
//! and union-merged in `.gitattributes`, and installs hooks for the agents
//! the repository or this machine uses. `--ci` adds a CI job and
//! `--git-hooks` a pre-commit hook that stages session logs.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use witness_git::{Git, GitCli};

use crate::cli::{CiArg, InitArgs};
use crate::hooks;
use crate::workspace::{CONFIG_FILE, SESSIONS_DIR, WITNESS_DIR};

/// The `.gitattributes` line for session logs: collapsed in pull request
/// diffs, and merged by keeping both sides' lines (logs are append-only).
const ATTRIBUTES: &str = ".witness/sessions/** linguist-generated=true merge=union";

/// Marks the pre-commit hook Witness wrote.
const GIT_HOOK_MARKER: &str = "# witness: stage agent session logs";

const GITHUB_WORKFLOW: &str = "\
name: Witness
on: pull_request
permissions:
  contents: read
  pull-requests: write
jobs:
  review-plan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: radical-studios/witness@v0
";

const GITLAB_INCLUDE: &str = "\
include:
  - remote: https://raw.githubusercontent.com/radical-studios/witness/main/ci/gitlab/witness.yml
";

pub fn run(args: InitArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("finding the working directory")?;
    let toplevel = GitCli::new(&cwd).toplevel();
    // By default .witness/ lives at the git top level; `--here` scopes it to
    // one directory (reports then cover only changes below it).
    let root = match (&toplevel, args.here) {
        (Ok(top), false) => top.clone(),
        (Err(err), false) => {
            eprintln!(
                "witness: not in a git repository ({err}); initializing {}",
                cwd.display()
            );
            cwd.clone()
        }
        (_, true) => cwd.clone(),
    };

    let sessions = root.join(SESSIONS_DIR);
    fs::create_dir_all(&sessions).with_context(|| format!("creating {}", sessions.display()))?;
    write_if_missing(&sessions.join(".gitkeep"), "")?;
    write_if_missing(
        &root.join(WITNESS_DIR).join(".gitignore"),
        "# Local state, not part of the record.\nstate/\n",
    )?;
    let config = root.join(CONFIG_FILE);
    if write_if_missing(&config, &witness_core::config::default_toml())? {
        println!("witness: wrote {CONFIG_FILE}");
    } else {
        println!("witness: kept the existing {CONFIG_FILE}");
    }
    if add_line(&root.join(".gitattributes"), ATTRIBUTES)? {
        println!("witness: marked session logs as generated in .gitattributes");
    }

    let agents: Vec<String> = if args.no_hooks {
        Vec::new()
    } else if !args.agents.is_empty() {
        args.agents.clone()
    } else {
        detect_agents(&root)
            .into_iter()
            .map(str::to_owned)
            .collect()
    };
    for agent in &agents {
        hooks::install_agent(&root, agent, false, true)?;
    }
    if agents.is_empty() && !args.no_hooks {
        println!(
            "witness: no coding agent detected; install hooks later with `witness hooks install \
             <agent>` ({})",
            hooks::agent_names().join(", ")
        );
    }

    match args.ci {
        Some(CiArg::Github) => {
            let path = root.join(".github/workflows/witness.yml");
            if path.exists() {
                println!("witness: kept the existing .github/workflows/witness.yml");
            } else {
                write(&path, GITHUB_WORKFLOW)?;
                println!("witness: wrote .github/workflows/witness.yml");
            }
        }
        Some(CiArg::Gitlab) => {
            let path = root.join(".gitlab-ci.yml");
            let text = fs::read_to_string(&path).unwrap_or_default();
            if text.contains("ci/gitlab/witness.yml") {
                println!("witness: .gitlab-ci.yml already includes the Witness job");
            } else if text.trim().is_empty() {
                write(&path, GITLAB_INCLUDE)?;
                println!("witness: wrote .gitlab-ci.yml");
            } else {
                println!("witness: add the Witness job to .gitlab-ci.yml:\n\n{GITLAB_INCLUDE}");
            }
        }
        None => {}
    }

    if args.git_hooks {
        match &toplevel {
            Ok(top) => install_git_hook(&GitCli::new(top), top, &root)?,
            Err(_) => eprintln!("witness: --git-hooks needs a git repository"),
        }
    }

    println!();
    println!(
        "Next: commit witness.toml, .witness/ and the files above, then work with your agent."
    );
    println!("`witness status` shows what is unchecked so far; `witness doctor` checks the setup.");
    Ok(())
}

/// Agents this repository or machine uses: their config directory or
/// instruction file is in the repository, or their CLI is on `PATH`.
fn detect_agents(root: &Path) -> Vec<&'static str> {
    let has = |path: &str| root.join(path).exists();
    let on_path = |program: &str| hooks::which(program).is_some();
    let mut found = Vec::new();
    if has(".claude") || has("CLAUDE.md") || on_path("claude") {
        found.push("claude-code");
    }
    if has(".codex") || on_path("codex") {
        found.push("codex");
    }
    if has(".github/copilot-instructions.md")
        || has(".github/hooks")
        || has(".github/workflows/copilot-setup-steps.yml")
        || on_path("copilot")
    {
        found.push("copilot");
    }
    if has(".cursor") || has(".cursorrules") || on_path("cursor-agent") {
        found.push("cursor");
    }
    if has(".gemini") || has("GEMINI.md") || on_path("gemini") {
        found.push("gemini-cli");
    }
    found
}

/// A pre-commit hook that stages the session logs, so they go out with
/// every commit. An existing hook is left alone.
fn install_git_hook(git: &GitCli, toplevel: &Path, root: &Path) -> Result<()> {
    let prefix = root
        .strip_prefix(toplevel)
        .ok()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| format!("{}/", p.to_string_lossy().replace('\\', "/")))
        .unwrap_or_default();
    let sessions = format!("{prefix}{SESSIONS_DIR}");
    let snippet = format!(
        "{GIT_HOOK_MARKER}\nif [ -d '{sessions}' ]; then git add -- '{sessions}' || true; fi\n"
    );
    let hooks_dir = git.git_path("hooks")?;
    let path = hooks_dir.join("pre-commit");
    match fs::read_to_string(&path) {
        Ok(text) if text.contains(GIT_HOOK_MARKER) => {
            println!("witness: the pre-commit hook already stages session logs");
        }
        Ok(_) => println!(
            "witness: {} exists; add this to it to stage session logs on commit:\n\n{snippet}",
            path.display()
        ),
        Err(_) => {
            write(&path, &format!("#!/bin/sh\n{snippet}exit 0\n"))?;
            make_executable(&path)?;
            println!("witness: installed a pre-commit hook that stages {sessions}/");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .with_context(|| format!("making {} executable", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Appends `line` to the file unless it is already there; true if added.
fn add_line(path: &Path, line: &str) -> Result<bool> {
    let text = fs::read_to_string(path).unwrap_or_default();
    if text.lines().any(|l| l.trim() == line) {
        return Ok(false);
    }
    let mut updated = text;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(line);
    updated.push('\n');
    write(path, &updated)?;
    Ok(true)
}

fn write(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

fn write_if_missing(path: &Path, contents: &str) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    write(path, contents)?;
    Ok(true)
}
