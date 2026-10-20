//! `witness report` and `witness post`: git + session logs → report.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use witness_core::history::NullHistoryStore;
use witness_core::record::parse_log;
use witness_core::report::{self, ReportInput};
use witness_core::{Config, Hunk, Report, V0Rules};
use witness_git::{Git, GitCli};
use witness_platform::{DryRun, Platform, PostTarget, RenderedReport, github, gitlab};

use crate::cli::{Format, PlatformArg, PostArgs, RangeArgs, ReportArgs};
use crate::workspace::{self, CONFIG_FILE, SESSIONS_DIR, WITNESS_DIR};

pub fn run_report(args: ReportArgs) -> Result<()> {
    let report = report_for(&args.range)?;
    match args.format {
        Format::Markdown => print!("{}", report.to_markdown()),
        Format::Json => println!("{}", report.to_json()?),
    }
    Ok(())
}

pub fn run_post(args: PostArgs) -> Result<()> {
    let report = report_for(&args.range)?;
    let rendered = RenderedReport {
        markdown: report.to_markdown(),
    };
    let target = PostTarget {
        repository: args.repo,
        number: args.number,
    };
    let stdout = std::io::stdout();
    let dry_run = DryRun::new(stdout.lock());
    let platform: &dyn Platform = match args.platform {
        PlatformArg::DryRun => &dry_run,
        PlatformArg::Github => &github::GitHub,
        PlatformArg::Gitlab => &gitlab::GitLab,
    };
    platform
        .post(&rendered, &target)
        .with_context(|| format!("posting to {}", platform.name()))?;
    Ok(())
}

fn report_for(range: &RangeArgs) -> Result<Report> {
    let cwd = std::env::current_dir().context("finding the working directory")?;
    let toplevel = GitCli::new(&cwd)
        .toplevel()
        .context("witness report must run inside a git repository")?;
    let git = GitCli::new(&toplevel);
    let (root, prefix) = workspace::witness_root(&cwd, &toplevel);
    let config = workspace::load_config(&root)?;
    // Uncommitted session logs count when reporting on the checked-out commit.
    let head_sha = git.rev_parse(&range.head).ok();
    let current_sha = git.rev_parse("HEAD").ok();
    let worktree = (head_sha.is_some() && head_sha == current_sha).then_some(root.as_path());
    compute(
        &git,
        &Request {
            base: &range.base,
            head: &range.head,
            prefix: &prefix,
            worktree,
        },
        &config,
    )
}

/// What to report on.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub base: &'a str,
    pub head: &'a str,
    /// Directory holding `.witness/`, relative to the git top level: empty,
    /// or `/`-terminated. Only changes under it are reported, with paths
    /// relative to it.
    pub prefix: &'a str,
    /// When given, session logs in this directory's `.witness/sessions/`
    /// replace the committed copies (logs are append-only, so the working
    /// tree holds a superset).
    pub worktree: Option<&'a Path>,
}

/// Builds the report for `base...head`.
pub fn compute(git: &dyn Git, req: &Request<'_>, config: &Config) -> Result<Report> {
    let Request {
        base, head, prefix, ..
    } = *req;
    for rev in [base, head] {
        git.rev_parse(rev)
            .map_err(|err| anyhow!("unknown revision {rev:?}: {err}"))?;
    }
    let hunks: Vec<Hunk> = witness_git::hunks(git, base, head)
        .with_context(|| format!("diffing {base}...{head}"))?
        .into_iter()
        .filter_map(|mut h| {
            h.file = h.file.strip_prefix(prefix)?.to_owned();
            (!is_witness_file(&h.file)).then_some(h)
        })
        .collect();

    let logs = branch_logs(git, req)?;
    let mut sessions = Vec::new();
    let mut skipped = 0;
    for text in logs.values() {
        let parsed = parse_log(text);
        skipped += parsed.skipped;
        if !parsed.events.is_empty() {
            sessions.push(parsed.events);
        }
    }

    Ok(report::build(ReportInput {
        base,
        head,
        hunks: &hunks,
        sessions,
        skipped_lines: skipped,
        config,
        classifier: &V0Rules,
        history: &NullHistoryStore,
    }))
}

/// Session log text written on this branch, keyed by path: for each log at
/// head (or in the working tree), what it gained since the merge base.
/// Events already in the base were part of earlier changes and must not
/// match this one's hunks.
fn branch_logs(git: &dyn Git, req: &Request<'_>) -> Result<BTreeMap<String, String>> {
    let sessions_dir = format!("{}{SESSIONS_DIR}", req.prefix);
    let mut logs: BTreeMap<String, String> = BTreeMap::new();
    for path in git.list_files(req.head, &sessions_dir)? {
        if path.ends_with(".jsonl")
            && let Some(text) = git.show_file(req.head, &path)?
        {
            logs.insert(path, text);
        }
    }
    if let Some(root) = req.worktree {
        let dir = root.join(SESSIONS_DIR);
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".jsonl") {
                    let text = fs::read_to_string(entry.path())
                        .with_context(|| format!("reading {}", entry.path().display()))?;
                    logs.insert(format!("{sessions_dir}/{name}"), text);
                }
            }
        }
    }

    let Some(merge_base) = git.merge_base(req.base, req.head)? else {
        return Ok(logs);
    };
    for (path, text) in logs.iter_mut() {
        if let Some(old) = git.show_file(&merge_base, path)? {
            let new = branch_suffix(text, &old);
            *text = new.to_owned();
        }
    }
    Ok(logs)
}

/// What `head_text` appends to `base_text`. Logs are append-only, so the
/// base copy is normally a prefix; if it isn't (the log was rewritten), the
/// whole log counts. A partial last line in the base is re-read whole.
fn branch_suffix<'a>(head_text: &'a str, base_text: &str) -> &'a str {
    let complete = match base_text.rfind('\n') {
        Some(idx) => &base_text[..=idx],
        None => "",
    };
    head_text.strip_prefix(complete).unwrap_or(head_text)
}

fn is_witness_file(path: &str) -> bool {
    path == CONFIG_FILE || path.starts_with(&format!("{WITNESS_DIR}/"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use witness_core::Status;
    use witness_core::record::LineRange;
    use witness_git::{BlameLine, CommitInfo, GitError};

    use super::*;

    struct FakeGit {
        diff: String,
        /// Files at head.
        files: Vec<(String, String)>,
        /// Files at the merge base.
        base_files: Vec<(String, String)>,
    }

    impl FakeGit {
        fn new(diff: &str, files: Vec<(String, String)>) -> Self {
            Self {
                diff: diff.into(),
                files,
                base_files: Vec::new(),
            }
        }
    }

    fn request<'a>(base: &'a str, worktree: Option<&'a Path>) -> Request<'a> {
        Request {
            base,
            head: "HEAD",
            prefix: "",
            worktree,
        }
    }

    impl Git for FakeGit {
        fn toplevel(&self) -> Result<PathBuf, GitError> {
            Ok(PathBuf::from("/repo"))
        }
        fn rev_parse(&self, rev: &str) -> Result<String, GitError> {
            if rev == "missing" {
                Err(GitError::Parse("no such ref".into()))
            } else {
                Ok(format!("sha-of-{rev}"))
            }
        }
        fn merge_base(&self, _a: &str, _b: &str) -> Result<Option<String>, GitError> {
            Ok(Some("base".into()))
        }
        fn diff(&self, _base: &str, _head: &str) -> Result<String, GitError> {
            Ok(self.diff.clone())
        }
        fn list_files(&self, _rev: &str, dir: &str) -> Result<Vec<String>, GitError> {
            Ok(self
                .files
                .iter()
                .filter(|(p, _)| p.starts_with(dir))
                .map(|(p, _)| p.clone())
                .collect())
        }
        fn show_file(&self, rev: &str, path: &str) -> Result<Option<String>, GitError> {
            let files = if rev == "base" {
                &self.base_files
            } else {
                &self.files
            };
            Ok(files
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, t)| t.clone()))
        }
        fn log(&self, _path: Option<&str>, _max: usize) -> Result<Vec<CommitInfo>, GitError> {
            Ok(Vec::new())
        }
        fn blame(
            &self,
            _rev: &str,
            _path: &str,
            _range: LineRange,
        ) -> Result<Vec<BlameLine>, GitError> {
            Ok(Vec::new())
        }
    }

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,0 +2,2 @@\n+x\n+y\n\
diff --git a/.witness/sessions/s.jsonl b/.witness/sessions/s.jsonl\n--- /dev/null\n+++ b/.witness/sessions/s.jsonl\n@@ -0,0 +1 @@\n+{}\n";

    const LOG: &str = concat!(
        r#"{"schema_version":1,"session_id":"s","timestamp":"2026-10-20T00:00:01.000Z","agent":"claude-code","kind":"edit","file":"src/a.rs","lines":{"start":2,"end":3},"change":{"start":2,"removed":0,"added":2},"tool":"Edit"}"#,
        "\n",
        r#"{"schema_version":1,"session_id":"s","timestamp":"2026-10-20T00:00:02.000Z","agent":"claude-code","kind":"command","classification":"test","exit_code":0,"command":"cargo test"}"#,
        "\n",
        "garbage\n",
    );

    #[test]
    fn computes_report_from_fake_git_and_skips_witness_files() {
        let git = FakeGit::new(DIFF, vec![(".witness/sessions/s.jsonl".into(), LOG.into())]);
        let report = compute(&git, &request("main", None), &Config::default()).unwrap();
        assert_eq!(report.summary.sections, 1);
        assert_eq!(report.summary.human, 0);
        assert_eq!(report.summary.skipped_lines, 1);
        assert_eq!(report.skim[0].status, Status::Checked);
    }

    #[test]
    fn worktree_logs_override_committed_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(SESSIONS_DIR);
        fs::create_dir_all(&dir).unwrap();
        // The working-tree copy has an extra edit after the test run.
        let extra = r#"{"schema_version":1,"session_id":"s","timestamp":"2026-10-20T00:00:03.000Z","agent":"claude-code","kind":"edit","file":"src/a.rs","lines":{"start":3,"end":3},"tool":"Edit"}"#;
        fs::write(dir.join("s.jsonl"), format!("{LOG}{extra}\n")).unwrap();
        let git = FakeGit::new(DIFF, vec![(".witness/sessions/s.jsonl".into(), LOG.into())]);
        let report = compute(&git, &request("main", Some(tmp.path())), &Config::default()).unwrap();
        assert_eq!(report.flagged[0].status, Status::Unverified);
    }

    #[test]
    fn unknown_ref_is_an_error() {
        let git = FakeGit::new("", Vec::new());
        let err = compute(&git, &request("missing", None), &Config::default()).unwrap_err();
        assert!(err.to_string().contains("unknown revision"));
    }

    #[test]
    fn events_already_in_the_base_do_not_count() {
        // The edit and the test run landed in an earlier change; on this
        // branch the log only gained an unrelated read.
        let read = r#"{"schema_version":1,"session_id":"s","timestamp":"2026-10-20T00:00:05.000Z","agent":"claude-code","kind":"read","file":"src/a.rs"}"#;
        let mut git = FakeGit::new(
            DIFF,
            vec![(".witness/sessions/s.jsonl".into(), format!("{LOG}{read}\n"))],
        );
        git.base_files = vec![(".witness/sessions/s.jsonl".into(), LOG.into())];
        let report = compute(&git, &request("main", None), &Config::default()).unwrap();
        assert_eq!(report.summary.sections, 0);
        assert_eq!(report.summary.human, 1);
        assert_eq!(report.summary.sessions, 1);
        assert_eq!(report.summary.skipped_lines, 0);
    }

    #[test]
    fn only_changes_under_the_prefix_are_reported() {
        let diff = DIFF
            .replace("src/a.rs", "pkg/src/a.rs")
            .replace(".witness/", "pkg/.witness/")
            + "diff --git a/other/b.rs b/other/b.rs\n--- a/other/b.rs\n+++ b/other/b.rs\n@@ -1,0 +1 @@\n+z\n";
        let git = FakeGit::new(
            &diff,
            vec![("pkg/.witness/sessions/s.jsonl".into(), LOG.into())],
        );
        let req = Request {
            prefix: "pkg/",
            ..request("main", None)
        };
        let report = compute(&git, &req, &Config::default()).unwrap();
        assert_eq!(report.summary.sections, 1);
        assert_eq!(report.summary.human, 0);
        assert_eq!(report.skim[0].file, "src/a.rs");
    }

    #[test]
    fn branch_suffix_handles_rewrites_and_partial_lines() {
        assert_eq!(branch_suffix("a\nb\n", "a\n"), "b\n");
        assert_eq!(branch_suffix("a\nb\n", ""), "a\nb\n");
        assert_eq!(branch_suffix("x\nb\n", "a\n"), "x\nb\n");
        // The base ended mid-line; the completed line is read again.
        assert_eq!(branch_suffix("a\nbc\n", "a\nb"), "bc\n");
    }
}
