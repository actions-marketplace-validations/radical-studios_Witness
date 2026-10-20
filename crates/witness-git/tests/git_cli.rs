//! `GitCli` against a real repository.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use witness_core::record::LineRange;
use witness_git::{Git, GitCli};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn reads_history_files_diffs_and_blame() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "first"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    fs::create_dir_all(dir.join("d")).unwrap();
    fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    fs::write(dir.join("d/b.txt"), "b\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "second"]);

    let cli = GitCli::new(dir);
    let main = cli.rev_parse("main").unwrap();
    assert_eq!(main.len(), 40);
    assert_eq!(
        cli.merge_base("main", "feature").unwrap(),
        Some(main.clone())
    );
    assert!(cli.rev_parse("no-such-ref").is_err());

    let hunks = witness_git::hunks(&cli, "main", "feature").unwrap();
    let files: Vec<_> = hunks.iter().map(|h| h.file.as_str()).collect();
    assert_eq!(files, vec!["a.txt", "d/b.txt"]);
    assert_eq!((hunks[0].new_start, hunks[0].new_count), (2, 2));

    assert_eq!(cli.list_files("feature", "d").unwrap(), vec!["d/b.txt"]);
    assert_eq!(
        cli.show_file("main", "a.txt").unwrap().as_deref(),
        Some("one\ntwo\n")
    );
    assert_eq!(cli.show_file("main", "d/b.txt").unwrap(), None);

    let log = cli.log(Some("a.txt"), 5).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].subject, "second");
    assert_eq!(log[0].author, "Ada");

    let blame = cli.blame("feature", "a.txt", LineRange::new(1, 3)).unwrap();
    assert_eq!(blame.len(), 3);
    assert_eq!(blame[0].commit, main);
    assert_ne!(blame[1].commit, main);
}

#[test]
fn unrelated_histories_have_no_merge_base() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join("a"), "a").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "a"]);
    git(dir, &["checkout", "-q", "--orphan", "other"]);
    fs::write(dir.join("b"), "b").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "b"]);
    assert_eq!(GitCli::new(dir).merge_base("main", "other").unwrap(), None);
}

#[test]
fn snapshots_the_working_tree_with_a_private_index() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    fs::create_dir_all(dir.join("pkg")).unwrap();
    fs::write(dir.join("pkg/a.txt"), "one\ntwo\n").unwrap();
    fs::write(dir.join("top.txt"), "top\n").unwrap();
    fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "first"]);

    let pkg = GitCli::new(dir.join("pkg"));
    let index = dir.join("snap.index");
    fs::copy(pkg.git_path("index").unwrap(), &index).unwrap();
    let before = pkg.snapshot_tree(&index).unwrap();
    fs::write(dir.join("pkg/a.txt"), "one\nTWO\n").unwrap();
    fs::write(dir.join("pkg/new.txt"), "n\n").unwrap();
    fs::write(dir.join("pkg/debug.log"), "ignored\n").unwrap();
    fs::write(dir.join("top.txt"), "changed outside pkg\n").unwrap();
    let after = pkg.snapshot_tree(&index).unwrap();
    assert_ne!(before, after);

    let hunks = witness_git::parse_unified_diff(&pkg.diff_trees(&before, &after).unwrap()).unwrap();
    let summary: Vec<_> = hunks
        .iter()
        .map(|h| {
            (
                h.file.as_str(),
                h.new_start,
                h.added.clone(),
                h.removed.clone(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("a.txt", 2, vec!["TWO".to_owned()], vec!["two".to_owned()]),
            ("new.txt", 1, vec!["n".to_owned()], vec![]),
        ]
    );
    // The real index is untouched.
    let status = Command::new("git")
        .args(["diff", "--cached", "--name-only"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(status.stdout.is_empty());
}

#[test]
fn walks_first_parent_history_reverts_and_renames() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    // A feature branch merged with a merge commit.
    git(dir, &["checkout", "-q", "-b", "feature"]);
    fs::create_dir_all(dir.join("logs")).unwrap();
    fs::write(dir.join("logs/s.jsonl"), "{}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature work"]);
    git(dir, &["checkout", "-q", "main"]);
    fs::write(dir.join("b.txt"), "b\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "unrelated"]);
    git(
        dir,
        &["merge", "-q", "--no-ff", "-m", "merge feature", "feature"],
    );
    let cli = GitCli::new(dir);
    let merge = cli.rev_parse("HEAD").unwrap();

    let chain = cli.first_parent("HEAD", None, None).unwrap();
    assert_eq!(chain.len(), 3, "the feature commit is not on the chain");
    assert_eq!(chain[0].0, merge);
    assert!(chain[0].1 > 1_000_000_000);
    let landings = cli.first_parent("HEAD", Some("logs"), None).unwrap();
    assert_eq!(landings.len(), 1);
    assert_eq!(landings[0].0, merge);
    assert_eq!(cli.first_parent("HEAD", None, Some(1)).unwrap().len(), 1);

    git(dir, &["revert", "--no-edit", "-m", "1", "HEAD"]);
    let reverts = cli.reverts("HEAD").unwrap();
    assert_eq!(reverts.len(), 1);
    assert_eq!(reverts[0].0, merge);

    git(dir, &["mv", "a.txt", "c.txt"]);
    git(dir, &["commit", "-q", "-m", "rename"]);
    assert_eq!(
        cli.renames(&merge, "HEAD").unwrap(),
        vec![("a.txt".to_owned(), "c.txt".to_owned())]
    );
}
