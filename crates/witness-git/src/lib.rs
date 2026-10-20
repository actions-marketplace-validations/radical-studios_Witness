//! Access to git through the `git` CLI, behind a trait so tests can fake it.

use std::path::{Path, PathBuf};
use std::process::Command;

use witness_core::Hunk;
use witness_core::record::LineRange;

mod diff;

pub use diff::{DiffParseError, parse_unified_diff};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("could not run git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("git {args} failed ({status}): {stderr}")]
    Failed {
        args: String,
        status: String,
        stderr: String,
    },
    #[error("git output was not UTF-8")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)]
    Diff(#[from] DiffParseError),
    #[error("unexpected git output: {0}")]
    Parse(String),
}

/// A commit as listed by `git log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub author: String,
    /// Strict ISO 8601 author date.
    pub date: String,
    pub subject: String,
}

/// One line of `git blame` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub commit: String,
    pub line: u32,
}

/// The git operations Witness needs.
pub trait Git {
    /// Absolute path of the working tree's top level.
    fn toplevel(&self) -> Result<PathBuf, GitError>;
    /// Full commit SHA of `rev`.
    fn rev_parse(&self, rev: &str) -> Result<String, GitError>;
    /// Best common ancestor of `a` and `b`, or `None` if they share no history.
    fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>, GitError>;
    /// Raw `git diff -U0 base...head` output.
    fn diff(&self, base: &str, head: &str) -> Result<String, GitError>;
    /// Files under `dir` in `rev`'s tree, repo-relative.
    fn list_files(&self, rev: &str, dir: &str) -> Result<Vec<String>, GitError>;
    /// Contents of `path` at `rev`, or `None` if it doesn't exist there.
    fn show_file(&self, rev: &str, path: &str) -> Result<Option<String>, GitError>;
    /// Most recent commits touching `path` (or the whole repo), newest first.
    fn log(&self, path: Option<&str>, max: usize) -> Result<Vec<CommitInfo>, GitError>;
    /// Which commit last changed each line of `range` in `path` at `rev`.
    fn blame(&self, rev: &str, path: &str, range: LineRange) -> Result<Vec<BlameLine>, GitError>;
    /// A commit's `author <email>`, `committer <email>` and full message,
    /// one after the other.
    fn commit_text(&self, _sha: &str) -> Result<Option<String>, GitError> {
        Ok(None)
    }
    /// The short name a symbolic ref points to, e.g. `origin/main` for
    /// `refs/remotes/origin/HEAD`.
    fn symbolic_ref(&self, _name: &str) -> Result<Option<String>, GitError> {
        Ok(None)
    }
    /// The URL of a remote.
    fn remote_url(&self, _remote: &str) -> Result<Option<String>, GitError> {
        Ok(None)
    }
}

/// Hunks changed on `head` since its merge base with `base`.
pub fn hunks(git: &dyn Git, base: &str, head: &str) -> Result<Vec<Hunk>, GitError> {
    Ok(parse_unified_diff(&git.diff(base, head)?)?)
}

/// [`Git`] implemented by running the `git` binary.
#[derive(Debug, Clone)]
pub struct GitCli {
    dir: PathBuf,
}

impl GitCli {
    /// Runs git commands in `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn output(&self, args: &[&str]) -> Result<std::process::Output, GitError> {
        self.output_with_index(args, None)
    }

    fn output_with_index(
        &self,
        args: &[&str],
        index: Option<&Path>,
    ) -> Result<std::process::Output, GitError> {
        let mut cmd = Command::new("git");
        cmd.args(["-c", "core.quotePath=false", "-c", "color.ui=never"])
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");
        match index {
            Some(index) => cmd.env("GIT_INDEX_FILE", index),
            None => cmd.env_remove("GIT_INDEX_FILE"),
        };
        Ok(cmd.output()?)
    }

    fn run(&self, args: &[&str]) -> Result<String, GitError> {
        self.run_with_index(args, None)
    }

    fn run_with_index(&self, args: &[&str], index: Option<&Path>) -> Result<String, GitError> {
        let out = self.output_with_index(args, index)?;
        if !out.status.success() {
            return Err(GitError::Failed {
                args: args.join(" "),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8(out.stdout)?)
    }

    /// Path of a file in the git directory, e.g. `index`.
    pub fn git_path(&self, name: &str) -> Result<PathBuf, GitError> {
        let path = PathBuf::from(self.run(&["rev-parse", "--git-path", name])?.trim());
        Ok(if path.is_absolute() {
            path
        } else {
            self.dir.join(path)
        })
    }

    /// Records the working tree under this directory in a tree object,
    /// using the private index file `index` so the real index is left
    /// alone, and returns the tree's id. Ignored files are left out. A
    /// concurrent snapshot holding the index lock is waited for briefly.
    pub fn snapshot_tree(&self, index: &Path) -> Result<String, GitError> {
        let mut attempt = 0;
        loop {
            match self.run_with_index(&["add", "-A", "--ignore-errors", "--", "."], Some(index)) {
                Err(GitError::Failed { stderr, .. })
                    if stderr.contains("index.lock") && attempt < 40 =>
                {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                other => {
                    other?;
                    break;
                }
            }
        }
        Ok(self
            .run_with_index(&["write-tree"], Some(index))?
            .trim()
            .to_owned())
    }

    /// `git status --porcelain` for `path`: one line per changed file.
    pub fn status_porcelain(&self, path: &str) -> Result<String, GitError> {
        self.run(&["status", "--porcelain", "--", path])
    }

    /// Whether this is a shallow clone (history cut off).
    pub fn is_shallow(&self) -> bool {
        self.run(&["rev-parse", "--is-shallow-repository"])
            .is_ok_and(|out| out.trim() == "true")
    }

    /// Raw `git diff -U0` between two trees, with paths relative to this
    /// directory and changes outside it left out.
    pub fn diff_trees(&self, old: &str, new: &str) -> Result<String, GitError> {
        self.run(&[
            "diff-tree",
            "-r",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "-U0",
            "-M",
            "--relative",
            old,
            new,
        ])
    }
}

impl Git for GitCli {
    fn toplevel(&self) -> Result<PathBuf, GitError> {
        Ok(PathBuf::from(
            self.run(&["rev-parse", "--show-toplevel"])?.trim(),
        ))
    }

    fn rev_parse(&self, rev: &str) -> Result<String, GitError> {
        let spec = format!("{rev}^{{commit}}");
        Ok(self
            .run(&["rev-parse", "--verify", "--quiet", &spec])?
            .trim()
            .to_owned())
    }

    fn merge_base(&self, a: &str, b: &str) -> Result<Option<String>, GitError> {
        let out = self.output(&["merge-base", a, b])?;
        // Exit 1 with no output means no common ancestor.
        if !out.status.success() {
            if out.status.code() == Some(1) && out.stderr.is_empty() {
                return Ok(None);
            }
            return Err(GitError::Failed {
                args: format!("merge-base {a} {b}"),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(Some(String::from_utf8(out.stdout)?.trim().to_owned()))
    }

    fn diff(&self, base: &str, head: &str) -> Result<String, GitError> {
        let range = format!("{base}...{head}");
        self.run(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "-U0",
            "-M",
            &range,
        ])
    }

    fn list_files(&self, rev: &str, dir: &str) -> Result<Vec<String>, GitError> {
        let out = self.run(&["ls-tree", "-r", "-z", "--name-only", rev, "--", dir])?;
        Ok(out
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn show_file(&self, rev: &str, path: &str) -> Result<Option<String>, GitError> {
        let spec = format!("{rev}:{path}");
        let out = self.output(&["cat-file", "blob", &spec])?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8(out.stdout)?))
    }

    fn log(&self, path: Option<&str>, max: usize) -> Result<Vec<CommitInfo>, GitError> {
        let max = format!("-n{max}");
        let mut args = vec!["log", "--format=%H%x1f%an%x1f%aI%x1f%s", max.as_str()];
        if let Some(path) = path {
            args.extend(["--", path]);
        }
        self.run(&args)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let mut f = line.split('\x1f');
                match (f.next(), f.next(), f.next(), f.next()) {
                    (Some(sha), Some(author), Some(date), Some(subject)) => Ok(CommitInfo {
                        sha: sha.to_owned(),
                        author: author.to_owned(),
                        date: date.to_owned(),
                        subject: subject.to_owned(),
                    }),
                    _ => Err(GitError::Parse(format!("log line: {line}"))),
                }
            })
            .collect()
    }

    fn blame(&self, rev: &str, path: &str, range: LineRange) -> Result<Vec<BlameLine>, GitError> {
        let lines = format!("{},{}", range.start, range.end);
        let out = self.run(&["blame", "--porcelain", "-L", &lines, rev, "--", path])?;
        Ok(parse_blame_porcelain(&out))
    }

    fn symbolic_ref(&self, name: &str) -> Result<Option<String>, GitError> {
        let out = self.output(&["symbolic-ref", "--quiet", "--short", name])?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|s| !s.is_empty()))
    }

    fn remote_url(&self, remote: &str) -> Result<Option<String>, GitError> {
        let out = self.output(&["remote", "get-url", remote])?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .filter(|s| !s.is_empty()))
    }

    fn commit_text(&self, sha: &str) -> Result<Option<String>, GitError> {
        let out = self.output(&["show", "-s", "--format=%an <%ae>%n%cn <%ce>%n%n%B", sha])?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()))
    }
}

/// Extracts (commit, final line) pairs from `git blame --porcelain`.
pub fn parse_blame_porcelain(text: &str) -> Vec<BlameLine> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split(' ');
            let sha = parts.next()?;
            if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let _orig = parts.next()?;
            let final_line = parts.next()?.parse().ok()?;
            Some(BlameLine {
                commit: sha.to_owned(),
                line: final_line,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blame_porcelain() {
        let sha = "a".repeat(40);
        let text = format!(
            "{sha} 1 1 2\nauthor Ada\nauthor-mail <a@x>\nfilename src/a.rs\n\tfn main() {{\n{sha} 2 2\n\t}}\n"
        );
        assert_eq!(
            parse_blame_porcelain(&text),
            vec![
                BlameLine {
                    commit: sha.clone(),
                    line: 1
                },
                BlameLine {
                    commit: sha,
                    line: 2
                },
            ]
        );
    }
}
