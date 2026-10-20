//! Observing what a tool call changed, by comparing files before and after
//! it. State lives under `.witness/state/` (never committed):
//!
//! - `index`: a private git index used to snapshot the working tree;
//! - `pre/<key>.tree`: the tree before a shell or unknown tool call;
//! - `pre/<key>.json`: copies of the files an edit tool was about to change;
//! - `last/<session>.tree`: the session's latest tree, for agents that only
//!   have after-hooks;
//! - `snapshots-off`: the session whose snapshots were too slow.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use witness_core::Config;
use witness_core::capture::textdiff::diff_hunks;
use witness_core::capture::{CapturePlan, Observed, normalize, relative_to};
use witness_core::hunk::Hunk;
use witness_core::record::{EditVia, session_file_stem};
use witness_git::GitCli;

use crate::workspace::STATE_DIR;

/// Files larger than this are not copied; an edit to one covers the whole
/// file.
const MAX_PREIMAGE_BYTES: u64 = 2 * 1024 * 1024;

pub struct Snapshots<'a> {
    root: &'a Path,
    state: PathBuf,
    budget: Duration,
}

/// What [`Snapshots::after`] found.
pub struct After {
    pub observed: Option<Observed>,
    /// Set when a snapshot was too slow and snapshots are now off.
    pub note: Option<String>,
}

impl<'a> Snapshots<'a> {
    pub fn new(root: &'a Path, config: &Config) -> Self {
        Self {
            root,
            state: root.join(STATE_DIR),
            budget: Duration::from_millis(config.raw.capture.snapshot_budget_ms),
        }
    }

    /// Before a tool call: save what is needed to see its changes later.
    pub fn before(&self, plan: &CapturePlan, key: &str, session: &str) -> Result<Option<String>> {
        match plan {
            CapturePlan::None => Ok(None),
            CapturePlan::Tree => {
                if self.disabled_for(session) {
                    return Ok(None);
                }
                let (tree, note) = self.timed_tree(session)?;
                self.write_state(&format!("pre/{key}.tree"), &tree)?;
                self.write_state(&last_path(session), &tree)?;
                Ok(note)
            }
            CapturePlan::Preimages(paths) => {
                let files: Vec<Value> = paths
                    .iter()
                    .map(|abs| {
                        let size = fs::metadata(abs).map(|m| m.len()).ok();
                        let content = match size {
                            Some(n) if n <= MAX_PREIMAGE_BYTES => fs::read(abs)
                                .ok()
                                .and_then(|b| String::from_utf8(b).ok())
                                .map_or(json!({"binary": true}), |c| json!({"content": c})),
                            Some(_) => json!({"binary": true}),
                            None => json!({"missing": true}),
                        };
                        json!({"path": abs, "before": content})
                    })
                    .collect();
                self.write_state(&format!("pre/{key}.json"), &serde_json::to_string(&files)?)?;
                Ok(None)
            }
        }
    }

    /// After a tool call: what it changed. Agents with only after-hooks
    /// (`post_only`) are compared against the session's latest snapshot.
    pub fn after(
        &self,
        plan: &CapturePlan,
        key: &str,
        session: &str,
        post_only: bool,
    ) -> Result<After> {
        let nothing = After {
            observed: None,
            note: None,
        };
        match plan {
            CapturePlan::None => Ok(nothing),
            CapturePlan::Tree => {
                let pre = self.take_state(&format!("pre/{key}.tree"));
                let base = match pre {
                    Some(tree) => Some(tree),
                    None if post_only => self.read_state(&last_path(session)),
                    None => None,
                };
                if self.disabled_for(session) || (base.is_none() && !post_only) {
                    return Ok(nothing);
                }
                let (tree, note) = self.timed_tree(session)?;
                self.write_state(&last_path(session), &tree)?;
                let Some(base) = base else {
                    // First call of a post-only session: just the baseline.
                    return Ok(After {
                        observed: None,
                        note,
                    });
                };
                let hunks = if base == tree {
                    Vec::new()
                } else {
                    let raw = self.git().diff_trees(&base, &tree)?;
                    witness_git::parse_unified_diff(&raw)?
                };
                Ok(After {
                    observed: Some(Observed {
                        via: EditVia::Snapshot,
                        hunks,
                    }),
                    note,
                })
            }
            CapturePlan::Preimages(_) => {
                let Some(saved) = self.take_state(&format!("pre/{key}.json")) else {
                    return Ok(nothing);
                };
                let files: Vec<Value> = serde_json::from_str(&saved).unwrap_or_default();
                let mut hunks = Vec::new();
                for file in &files {
                    let Some(abs) = file.get("path").and_then(Value::as_str) else {
                        continue;
                    };
                    hunks.extend(self.file_hunks(Path::new(abs), &file["before"]));
                }
                Ok(After {
                    observed: Some(Observed {
                        via: EditVia::Preimage,
                        hunks,
                    }),
                    note: None,
                })
            }
        }
    }

    /// Updates the session's latest snapshot without diffing, for
    /// post-only agents after an edit they reported themselves.
    pub fn refresh_baseline(&self, session: &str) -> Result<()> {
        if self.disabled_for(session) {
            return Ok(());
        }
        let (tree, _) = self.timed_tree(session)?;
        self.write_state(&last_path(session), &tree)
    }

    fn file_hunks(&self, abs: &Path, before: &Value) -> Vec<Hunk> {
        let Some(rel) = relative_to(&normalize(abs), &normalize(self.root)) else {
            return Vec::new();
        };
        let after_size = fs::metadata(abs).map(|m| m.len()).ok();
        let after = match after_size {
            Some(n) if n <= MAX_PREIMAGE_BYTES => fs::read(abs)
                .ok()
                .map(|b| String::from_utf8(b).map_err(|_| ())),
            Some(_) => Some(Err(())),
            None => None,
        };
        let old = before.get("content").and_then(Value::as_str);
        let was_binary = before.get("binary").is_some();
        match after {
            // Too big or not text: the whole file changed, if anything did.
            Some(Err(())) => vec![whole_file(&rel)],
            _ if was_binary => vec![whole_file(&rel)],
            Some(Ok(new)) => diff_hunks(&rel, old, Some(&new)),
            None => diff_hunks(&rel, old, None),
        }
    }

    fn timed_tree(&self, session: &str) -> Result<(String, Option<String>)> {
        let started = Instant::now();
        let tree = self.tree()?;
        let elapsed = started.elapsed();
        if elapsed > self.budget {
            let ms = elapsed.as_millis();
            self.write_state("snapshots-off", &format!("{session}\n{ms}\n"))?;
            let note = format!(
                "working-tree snapshots took {ms} ms (budget {} ms); turned off for this session, \
                 so files changed by shell commands are not recorded",
                self.budget.as_millis()
            );
            return Ok((tree, Some(note)));
        }
        Ok((tree, None))
    }

    fn tree(&self) -> Result<String> {
        let git = self.git();
        let index = self.state.join("index");
        if !index.exists() {
            fs::create_dir_all(&self.state)?;
            // Start from the real index so unchanged files need no hashing.
            if let Ok(real) = git.git_path("index") {
                let _ = fs::copy(real, &index);
            }
        }
        git.snapshot_tree(&index)
            .context("snapshotting the working tree")
    }

    fn git(&self) -> GitCli {
        GitCli::new(self.root)
    }

    fn disabled_for(&self, session: &str) -> bool {
        self.read_state("snapshots-off")
            .is_some_and(|s| s.lines().next() == Some(session))
    }

    fn state_path(&self, rel: &str) -> PathBuf {
        self.state.join(rel)
    }

    fn write_state(&self, rel: &str, text: &str) -> Result<()> {
        let path = self.state_path(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    fn read_state(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.state_path(rel))
            .ok()
            .map(|s| s.trim_end().to_owned())
    }

    fn take_state(&self, rel: &str) -> Option<String> {
        let text = self.read_state(rel)?;
        let _ = fs::remove_file(self.state_path(rel));
        Some(text)
    }
}

fn last_path(session: &str) -> String {
    format!("last/{}.tree", session_file_stem(session))
}

fn whole_file(rel: &str) -> Hunk {
    Hunk {
        file: rel.to_owned(),
        old_start: 0,
        old_count: 0,
        new_start: 0,
        new_count: 0,
        added: Vec::new(),
        removed: Vec::new(),
        binary: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        fs::create_dir_all(tmp.path().join(".witness/sessions")).unwrap();
        fs::write(tmp.path().join(".witness/.gitignore"), "state/\n").unwrap();
        fs::write(tmp.path().join("a.txt"), "1\n2\n3\n").unwrap();
        git(tmp.path(), &["add", "-A"]);
        git(tmp.path(), &["commit", "-q", "-m", "base"]);
        tmp
    }

    #[test]
    fn tree_snapshots_see_shell_changes() {
        let tmp = repo();
        let config = Config::default();
        let snaps = Snapshots::new(tmp.path(), &config);
        snaps.before(&CapturePlan::Tree, "k1", "s").unwrap();
        fs::write(tmp.path().join("a.txt"), "1\ntwo\n3\n").unwrap();
        fs::write(tmp.path().join(".witness/sessions/s.jsonl"), "{}\n").unwrap();
        let after = snaps.after(&CapturePlan::Tree, "k1", "s", false).unwrap();
        let observed = after.observed.unwrap();
        assert_eq!(observed.via, EditVia::Snapshot);
        let files: Vec<_> = observed.hunks.iter().map(|h| h.file.as_str()).collect();
        // Session logs change too; capture drops Witness's own files later.
        assert!(files.contains(&"a.txt"), "{files:?}");
        assert!(after.note.is_none());
        // The pre state was consumed.
        let again = snaps.after(&CapturePlan::Tree, "k1", "s", false).unwrap();
        assert!(again.observed.is_none());
    }

    #[test]
    fn post_only_sessions_compare_with_the_last_snapshot() {
        let tmp = repo();
        let config = Config::default();
        let snaps = Snapshots::new(tmp.path(), &config);
        // The first call only sets the baseline.
        let first = snaps.after(&CapturePlan::Tree, "k", "s", true).unwrap();
        assert!(first.observed.is_none());
        fs::write(tmp.path().join("b.txt"), "new\n").unwrap();
        let second = snaps.after(&CapturePlan::Tree, "k", "s", true).unwrap();
        let hunks = second.observed.unwrap().hunks;
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].file, "b.txt");
    }

    #[test]
    fn preimages_diff_edited_files() {
        let tmp = repo();
        let config = Config::default();
        let snaps = Snapshots::new(tmp.path(), &config);
        let a = tmp.path().join("a.txt");
        let c = tmp.path().join("c.txt");
        let plan = CapturePlan::Preimages(vec![a.clone(), c.clone()]);
        snaps.before(&plan, "k", "s").unwrap();
        fs::write(&a, "1\n2\n2.5\n3\n").unwrap();
        fs::write(&c, "created\n").unwrap();
        let observed = snaps
            .after(&plan, "k", "s", false)
            .unwrap()
            .observed
            .unwrap();
        assert_eq!(observed.via, EditVia::Preimage);
        let summary: Vec<_> = observed
            .hunks
            .iter()
            .map(|h| (h.file.as_str(), h.new_start, h.new_count))
            .collect();
        assert_eq!(summary, vec![("a.txt", 3, 1), ("c.txt", 1, 1)]);
    }

    #[test]
    fn slow_snapshots_turn_off_for_the_session() {
        let tmp = repo();
        let mut config = Config::default();
        config.raw.capture.snapshot_budget_ms = 0;
        let snaps = Snapshots::new(tmp.path(), &config);
        let note = snaps.before(&CapturePlan::Tree, "k", "s").unwrap();
        assert!(note.is_some_and(|n| n.contains("turned off")));
        assert!(snaps.disabled_for("s"));
        assert!(!snaps.disabled_for("other"));
        let after = snaps.after(&CapturePlan::Tree, "k", "s", false).unwrap();
        assert!(after.observed.is_none());
    }
}
