//! `witness report`, `status`, `explain`, `sessions` and `post`.

use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use witness_core::Report;
use witness_core::classify::Status;
use witness_core::history::History;
use witness_core::record::{EventKind, timestamp};
use witness_core::report::explain::explain;
use witness_core::report::{self, LinkStyle, Links, TextOptions};
use witness_core::signals::SignalKind;
use witness_core::testsum::RunOutcome;
use witness_git::Git;
use witness_platform::github::GitHub;
use witness_platform::gitlab::GitLab;
use witness_platform::http::Curl;
use witness_platform::{DryRun, Platform, PlatformError, PostOutcome, PostTarget, RenderedReport};

use crate::ci::{self, Host};
use crate::cli::{
    ColorArg, ExplainArgs, Format, OutputArgs, PlatformArg, PostArgs, ReportArgs, SessionsArgs,
    StatusArgs,
};
use crate::history;
use crate::pipeline::{self, Repo, Request};
use crate::workspace::{self, SESSIONS_DIR, STATE_DIR};

pub fn run_report(args: ReportArgs) -> Result<ExitCode> {
    let fail_on = FailOn::parse(&args.output.fail_on)?;
    let repo = Repo::open()?;
    let base = resolve_base(&repo.git, args.range.base.as_deref())?;
    let head = args.range.head.as_str();
    let worktree = repo.is_checked_out(head).then_some(repo.root.as_path());
    let req = Request {
        base: &base,
        head,
        prefix: &repo.prefix,
        worktree,
        diff: None,
    };
    let history = track_record(&repo, &base);
    let mut report =
        pipeline::compute(&repo.git, &req, &repo.config, &history).map_err(shallow_hint(&repo))?;
    if let Some(links) = links_for(&repo.git, head) {
        report.set_links(&links);
    }
    print_report(&report, &args.output)?;
    Ok(fail_on.check(&report))
}

pub fn run_status(args: StatusArgs) -> Result<ExitCode> {
    let fail_on = FailOn::parse(&args.output.fail_on)?;
    let repo = Repo::open()?;
    let base = resolve_base(&repo.git, args.base.as_deref())?;
    let (mut report, notes) = worktree_report(&repo, &base)?;
    report.head = "working tree".to_owned();
    print_report(&report, &args.output)?;
    for note in notes {
        eprintln!("witness: {note}");
    }
    Ok(fail_on.check(&report))
}

pub fn run_explain(args: ExplainArgs) -> Result<()> {
    let repo = Repo::open()?;
    let base = resolve_base(&repo.git, args.base.as_deref())?;
    let (file, line) = match args.target.rsplit_once(':') {
        Some((file, line)) if line.chars().all(|c| c.is_ascii_digit()) && !line.is_empty() => {
            (file, Some(line.parse::<u32>().context("line number")?))
        }
        _ => (args.target.as_str(), None),
    };
    let cwd = std::env::current_dir().context("finding the working directory")?;
    let file = workspace::repo_path(&repo.root, &cwd, file)?;
    let diff = worktree_diff(&repo, &base)?;
    let req = Request {
        base: &base,
        head: "HEAD",
        prefix: &repo.prefix,
        worktree: Some(&repo.root),
        diff: Some(&diff),
    };
    let gathered = pipeline::gather(&repo.git, &req)?;
    let found = pipeline::with_input(
        &repo.git,
        &req,
        &repo.config,
        &History::default(),
        gathered,
        |input, packages| {
            let analysis = report::analyze(input);
            explain(&analysis, &file, line, packages)
        },
    );
    match found {
        Some(explanation) => print!("{}", explanation.to_text()),
        None => println!("{file}: not changed on this branch (compared with {base})"),
    }
    Ok(())
}

pub fn run_sessions(args: SessionsArgs) -> Result<()> {
    let repo = Repo::open()?;
    let base = resolve_base(&repo.git, args.base.as_deref())?;
    let req = Request {
        base: &base,
        head: "HEAD",
        prefix: &repo.prefix,
        worktree: Some(&repo.root),
        diff: Some(""),
    };
    let gathered = pipeline::gather(&repo.git, &req)?;
    let mut rows = Vec::new();
    for events in &gathered.sessions {
        let Some(first) = events.first() else {
            continue;
        };
        let last = events.last().unwrap_or(first);
        let count = |f: &dyn Fn(&EventKind) -> bool| events.iter().filter(|e| f(&e.kind)).count();
        let runs: Vec<Option<RunOutcome>> = events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::Command { classification, .. } if classification.verifies()))
            .map(|e| e.run_outcome())
            .collect();
        let mut models: Vec<&str> = events.iter().filter_map(|e| e.model.as_deref()).collect();
        models.dedup();
        rows.push(json!({
            "session": first.session_id,
            "agent": first.agent,
            "models": models,
            "started": timestamp::format(&first.timestamp).unwrap_or_default(),
            "last_event": timestamp::format(&last.timestamp).unwrap_or_default(),
            "ended": events.iter().any(|e| matches!(e.kind, EventKind::SessionEnd { .. })),
            "edits": count(&|k| matches!(k, EventKind::Edit { .. })),
            "reads": count(&|k| matches!(k, EventKind::Read { .. })),
            "checks": runs.len(),
            "checks_passed": runs.iter().filter(|o| **o == Some(RunOutcome::Passed)).count(),
            "checks_failed": runs.iter().filter(|o| **o == Some(RunOutcome::Failed)).count(),
            "assumptions": count(&|k| matches!(k, EventKind::Assumption { .. })),
        }));
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("No agent sessions recorded on this branch (compared with {base}).");
        return Ok(());
    }
    println!(
        "{:<38} {:<12} {:<17} {:>5} {:>13} {:>7}",
        "SESSION", "AGENT", "STARTED (UTC)", "EDITS", "CHECKS (✓/✗)", "ASSUMED"
    );
    for r in &rows {
        let started = r["started"].as_str().unwrap_or("");
        println!(
            "{:<38} {:<12} {:<17} {:>5} {:>13} {:>7}",
            r["session"].as_str().unwrap_or(""),
            r["agent"].as_str().unwrap_or(""),
            started.get(..16).unwrap_or(started).replace('T', " "),
            r["edits"],
            format!(
                "{} ({}/{})",
                r["checks"], r["checks_passed"], r["checks_failed"]
            ),
            r["assumptions"],
        );
    }
    Ok(())
}

pub fn run_post(args: PostArgs) -> Result<ExitCode> {
    let fail_on = FailOn::parse(&args.fail_on)?;
    let ci = ci::current();
    let platform = match (args.platform, ci.host) {
        (PlatformArg::Auto, Some(Host::GitHub)) | (PlatformArg::Github, _) => PlatformArg::Github,
        (PlatformArg::Auto, Some(Host::GitLab)) | (PlatformArg::Gitlab, _) => PlatformArg::Gitlab,
        (PlatformArg::DryRun, _) => PlatformArg::DryRun,
        (PlatformArg::Auto, None) => bail!(
            "not running in GitHub Actions or GitLab CI; pass --platform github|gitlab|dry-run"
        ),
    };
    let repo = Repo::open()?;
    let base = resolve_base(&repo.git, args.range.base.as_deref())?;
    let head = args.range.head.as_str();
    let worktree = repo.is_checked_out(head).then_some(repo.root.as_path());
    let req = Request {
        base: &base,
        head,
        prefix: &repo.prefix,
        worktree,
        diff: None,
    };
    let history = track_record(&repo, &base);
    let mut report =
        pipeline::compute(&repo.git, &req, &repo.config, &history).map_err(shallow_hint(&repo))?;
    if let Some(mut links) = links_for(&repo.git, head) {
        // GitHub builds pull requests on a temporary merge commit; link to
        // the change's own head instead.
        if let Some(sha) = &ci.head_sha {
            links.sha.clone_from(sha);
        }
        report.set_links(&links);
    }
    let markdown = report.to_markdown();
    if let Some(summary) = &ci.step_summary
        && let Err(err) = append(summary, &markdown)
    {
        eprintln!("witness: could not write the job summary: {err}");
    }
    let rendered = RenderedReport { markdown };
    let target = PostTarget {
        repository: args.repo.or(ci.repository.clone()),
        number: args.number.or(ci.number),
    };
    let token = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
    };
    let outcome = match platform {
        PlatformArg::DryRun | PlatformArg::Auto => {
            let stdout = std::io::stdout();
            DryRun::new(stdout.lock()).post(&rendered, &target)
        }
        PlatformArg::Github => {
            let token = token(&["WITNESS_GITHUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"])
                .ok_or_else(|| anyhow!("set GITHUB_TOKEN (or WITNESS_GITHUB_TOKEN) to post"))?;
            let github = GitHub {
                http: &Curl,
                token,
                api_url: ci
                    .api_url
                    .clone()
                    .unwrap_or_else(|| "https://api.github.com".to_owned()),
            };
            github.post(&rendered, &target)
        }
        PlatformArg::Gitlab => {
            let token = token(&["WITNESS_GITLAB_TOKEN", "GITLAB_TOKEN"]).ok_or_else(|| {
                anyhow!("set WITNESS_GITLAB_TOKEN to a token with api scope to post")
            })?;
            let gitlab = GitLab {
                http: &Curl,
                token,
                api_url: ci
                    .api_url
                    .clone()
                    .unwrap_or_else(|| "https://gitlab.com/api/v4".to_owned()),
                mr_url: ci.mr_url.clone(),
            };
            gitlab.post(&rendered, &target)
        }
    };
    match outcome {
        Ok(PostOutcome::Created { url }) => {
            eprintln!("witness: posted the review plan{}", at(url.as_deref()));
        }
        Ok(PostOutcome::Updated { url }) => {
            eprintln!("witness: updated the review plan{}", at(url.as_deref()));
        }
        Ok(PostOutcome::DryRun) => {}
        // A pull request from a fork gets a read-only token. Don't fail the
        // build over it: the plan is in the job summary and annotations.
        Err(PlatformError::Forbidden(message)) if ci.host.is_some() => {
            eprintln!(
                "witness: warning: can't comment ({message}); this is expected for pull requests                  from forks. The review plan is in the job summary."
            );
            if ci.host == Some(Host::GitHub) {
                println!(
                    "::warning title=Witness::Could not post the review plan comment ({message}); see the job summary."
                );
            }
        }
        Err(err) => return Err(anyhow!(err)).context("posting the review plan"),
    }
    Ok(fail_on.check(&report))
}

fn at(url: Option<&str>) -> String {
    url.map(|u| format!(": {u}")).unwrap_or_default()
}

fn append(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // The marker comment is noise in a job summary.
    let body = text
        .strip_prefix(witness_core::REPORT_MARKER)
        .unwrap_or(text)
        .trim_start();
    writeln!(file, "{body}")
}

/// The track record for reports on `base`, updated from git history. A
/// failure only costs the track record.
fn track_record(repo: &Repo, base: &str) -> History {
    match history::load(repo, base, true) {
        Ok(built) => {
            for note in built.notes {
                eprintln!("witness: {note}");
            }
            built.history
        }
        Err(err) => {
            eprintln!("witness: no track record: {err:#}");
            History::default()
        }
    }
}

/// The report for the branch plus uncommitted work, and notes for the user.
pub fn worktree_report(repo: &Repo, base: &str) -> Result<(Report, Vec<String>)> {
    let diff = worktree_diff(repo, base)?;
    let req = Request {
        base,
        head: "HEAD",
        prefix: &repo.prefix,
        worktree: Some(&repo.root),
        diff: Some(&diff),
    };
    // The cached track record only: examining history is too slow here.
    let history = history::load(repo, base, false)
        .map(|built| built.history)
        .unwrap_or_default();
    let report = pipeline::compute(&repo.git, &req, &repo.config, &history)?;
    let mut notes = Vec::new();
    let sessions = format!("{}{SESSIONS_DIR}", repo.prefix);
    if let Ok(changed) = repo.git.status_porcelain(&sessions) {
        let n = changed.lines().filter(|l| l.ends_with(".jsonl")).count();
        if n > 0 {
            notes.push(format!(
                "{n} session {} not committed yet; commit {sessions}/ with your changes so reviewers see the record",
                if n == 1 { "log is" } else { "logs are" }
            ));
        }
    }
    Ok((report, notes))
}

/// `git diff` from the merge base with `base` to the working tree,
/// untracked files included (through a snapshot in a private index).
fn worktree_diff(repo: &Repo, base: &str) -> Result<String> {
    let merge_base = repo
        .git
        .merge_base(base, "HEAD")?
        .ok_or_else(|| anyhow!("HEAD shares no history with {base}"))?;
    let index = repo.root.join(STATE_DIR).join("status.index");
    if !index.exists() {
        std::fs::create_dir_all(repo.root.join(STATE_DIR))?;
        if let Ok(real) = repo.git.git_path("index") {
            let _ = std::fs::copy(real, &index);
        }
    }
    let tree = repo
        .git
        .snapshot_tree(&index)
        .context("snapshotting the working tree")?;
    Ok(repo.git.diff_trees(&merge_base, &tree)?)
}

fn print_report(report: &Report, output: &OutputArgs) -> Result<()> {
    let terminal = std::io::stdout().is_terminal();
    let format = match output.format {
        Format::Auto if terminal => Format::Text,
        Format::Auto => Format::Markdown,
        f => f,
    };
    match format {
        Format::Markdown | Format::Auto => print!("{}", report.to_markdown()),
        Format::Json => println!("{}", report.to_json()?),
        Format::Text => {
            let color = match output.color {
                ColorArg::Always => true,
                ColorArg::Never => false,
                ColorArg::Auto => terminal && std::env::var_os("NO_COLOR").is_none(),
            };
            print!(
                "{}",
                report.to_text(TextOptions {
                    color,
                    all: output.all
                })
            );
        }
        Format::Sarif => println!(
            "{}",
            serde_json::to_string_pretty(&report.to_sarif(env!("CARGO_PKG_VERSION")))?
        ),
        Format::Github => print!("{}", report.to_github_annotations()),
        Format::GitlabCodequality => println!(
            "{}",
            serde_json::to_string_pretty(&report.to_gitlab_codequality())?
        ),
    }
    Ok(())
}

/// `--fail-on`: statuses and signals that make the command exit 1.
struct FailOn {
    statuses: Vec<Status>,
    signals: Vec<SignalKind>,
}

impl FailOn {
    fn parse(values: &[String]) -> Result<Self> {
        let mut statuses = Vec::new();
        let mut signals = Vec::new();
        for v in values {
            let quoted = serde_json::Value::String(v.trim().to_owned());
            if let Ok(status) = serde_json::from_value::<Status>(quoted.clone()) {
                statuses.push(status);
            } else if let Ok(signal) = serde_json::from_value::<SignalKind>(quoted) {
                signals.push(signal);
            } else {
                bail!(
                    "--fail-on: unknown status or signal {v:?}; statuses: failing, guessed, unrecorded, \
                     unverified, compiled, tested; signals: test-changed-after-failure, \
                     assertions-removed, high-risk-area, blind-edit, thrash, filtered-test-run, \
                     human-modified, rewritten-often"
                );
            }
        }
        Ok(Self { statuses, signals })
    }

    fn check(&self, report: &Report) -> ExitCode {
        if report.has_any(&self.statuses, &self.signals) {
            eprintln!("witness: failing: a section matches --fail-on");
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

/// The ref a change merges into, when not given: the CI target branch,
/// else origin's default branch, else `main` or `master`.
pub fn resolve_base(git: &dyn Git, arg: Option<&str>) -> Result<String> {
    if let Some(base) = arg {
        return Ok(base.to_owned());
    }
    let exists = |rev: &str| git.rev_parse(rev).is_ok();
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let mut candidates: Vec<String> = Vec::new();
    candidates.extend(env("WITNESS_BASE"));
    candidates.extend(env("CI_MERGE_REQUEST_DIFF_BASE_SHA"));
    for branch in [
        env("GITHUB_BASE_REF"),
        env("CI_MERGE_REQUEST_TARGET_BRANCH_NAME"),
    ]
    .into_iter()
    .flatten()
    {
        candidates.push(format!("origin/{branch}"));
        candidates.push(branch);
    }
    if let Ok(Some(default)) = git.symbolic_ref("refs/remotes/origin/HEAD") {
        candidates.push(default);
    }
    candidates.extend(["main", "master", "origin/main", "origin/master"].map(str::to_owned));
    candidates.into_iter().find(|c| exists(c)).ok_or_else(|| {
        anyhow!("can't tell which branch this change merges into; pass --base <ref>")
    })
}

/// Where section links point: the CI's repository, else origin's URL on
/// github.com or gitlab.com.
fn links_for(git: &dyn Git, head: &str) -> Option<Links> {
    let sha = git.rev_parse(head).ok()?;
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let (Some(server), Some(repo)) = (env("GITHUB_SERVER_URL"), env("GITHUB_REPOSITORY")) {
        return Some(Links {
            style: LinkStyle::GitHub,
            base: format!("{}/{repo}", server.trim_end_matches('/')),
            sha,
        });
    }
    if let Some(url) = env("CI_PROJECT_URL") {
        return Some(Links {
            style: LinkStyle::GitLab,
            base: url,
            sha,
        });
    }
    let url = git.remote_url("origin").ok()??;
    let (style, base) = host_link(&url)?;
    Some(Links { style, base, sha })
}

/// `https://github.com/o/r` from `git@github.com:o/r.git` and the like.
pub fn host_link(remote: &str) -> Option<(LinkStyle, String)> {
    let rest = remote
        .strip_prefix("git@")
        .map(|r| r.replacen(':', "/", 1))
        .or_else(|| {
            remote
                .strip_prefix("https://")
                .or_else(|| remote.strip_prefix("http://"))
                .or_else(|| remote.strip_prefix("ssh://git@"))
                .map(str::to_owned)
        })?;
    // Drop credentials (`user:token@host`).
    let rest = rest.rsplit_once('@').map_or(rest.as_str(), |(_, r)| r);
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let host = rest.split('/').next()?;
    let style = match host {
        "github.com" => LinkStyle::GitHub,
        "gitlab.com" => LinkStyle::GitLab,
        _ => return None,
    };
    Some((style, format!("https://{rest}")))
}

/// Explains a failed diff in a shallow clone, the usual CI pitfall.
fn shallow_hint(repo: &Repo) -> impl FnOnce(anyhow::Error) -> anyhow::Error + '_ {
    move |err| {
        if repo.git.is_shallow() {
            err.context(
                "this is a shallow clone, so the merge base may be missing; fetch full history \
                 (actions/checkout: fetch-depth: 0; GitLab: GIT_DEPTH: 0)",
            )
        } else {
            err
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_links_from_remote_urls() {
        for (remote, expected) in [
            (
                "git@github.com:acme/app.git",
                Some("https://github.com/acme/app"),
            ),
            (
                "https://github.com/acme/app",
                Some("https://github.com/acme/app"),
            ),
            (
                "https://x-access-token:abc@github.com/acme/app.git",
                Some("https://github.com/acme/app"),
            ),
            (
                "ssh://git@gitlab.com/group/sub/app.git",
                Some("https://gitlab.com/group/sub/app"),
            ),
            ("https://git.example.com/acme/app.git", None),
            ("/srv/git/app.git", None),
        ] {
            assert_eq!(
                host_link(remote).map(|(_, b)| b).as_deref(),
                expected,
                "{remote}"
            );
        }
    }
}
