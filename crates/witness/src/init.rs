//! `witness init`.

use std::fs;

use anyhow::{Context, Result};
use witness_git::{Git, GitCli};

use crate::workspace::{CONFIG_FILE, SESSIONS_DIR, WITNESS_DIR};

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir().context("finding the working directory")?;
    // Diff paths are relative to the git top level, so .witness/ lives there.
    let root = GitCli::new(&cwd).toplevel().unwrap_or_else(|err| {
        eprintln!(
            "witness: not in a git repository ({err}); initializing {}",
            cwd.display()
        );
        cwd.clone()
    });

    let sessions = root.join(SESSIONS_DIR);
    fs::create_dir_all(&sessions).with_context(|| format!("creating {}", sessions.display()))?;
    write_if_missing(&sessions.join(".gitkeep"), "")?;
    write_if_missing(
        &root.join(WITNESS_DIR).join(".gitignore"),
        "# Local state, not part of the record.\nstate/\n",
    )?;
    let config = root.join(CONFIG_FILE);
    if write_if_missing(&config, &witness_core::config::default_toml())? {
        println!("witness: wrote {}", config.display());
    } else {
        println!("witness: kept existing {}", config.display());
    }
    println!("witness: session logs go in {}", sessions.display());
    println!("witness: commit witness.toml and .witness/ with your changes");
    Ok(())
}

fn write_if_missing(path: &std::path::Path, contents: &str) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}
