//! End-to-end scenarios built with the `support` scenario builder.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::TestRepo;

/// An agent edit merged in an earlier change must not claim a later human
/// change to the same lines.
#[test]
fn old_sessions_do_not_claim_new_changes() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "fn a() {}\nfn b() {}\n");
    repo.init_witness();
    repo.commit_all("base");

    let agent = repo.claude("old");
    agent.edit("src/lib.rs", "fn a() {}", "fn a() { 1; }");
    agent.bash("cargo test");
    agent.end();
    repo.commit_all("agent change, merged");

    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("src/lib.rs", "fn a() { 2; }\nfn b() {}\n");
    repo.commit_all("human change");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["sections"], 0, "{report:#}");
    assert_eq!(report["summary"]["human"], 1);
    assert_eq!(report["summary"]["sessions"], 0);
}

/// `.witness/` can live in a package below the git top level; only that
/// package's changes are reported, with package-relative paths.
#[test]
fn witness_root_below_the_git_top_level() {
    let repo = TestRepo::new();
    repo.write("pkg/app/src/lib.rs", "fn a() {}\n");
    repo.write("other/lib.rs", "fn b() {}\n");
    repo.commit_all("base");
    let pkg = repo.abs("pkg/app");
    support::stdout_of(&support::witness(&pkg, &["init", "--here"], None));
    assert!(pkg.join("witness.toml").is_file());
    repo.commit_all("set up witness in the package");
    repo.git(&["checkout", "-q", "-b", "feature"]);

    let session = repo.claude("s").in_dir("pkg/app");
    session.edit("pkg/app/src/lib.rs", "fn a() {}", "fn a() { 1; }");
    repo.write("other/lib.rs", "fn b() { 2; }\n");
    repo.commit_all("feature");

    let out = support::witness(
        &pkg,
        &[
            "report", "--base", "main", "--head", "HEAD", "--format", "json",
        ],
        None,
    );
    let report: serde_json::Value = serde_json::from_str(&support::stdout_of(&out)).unwrap();
    assert_eq!(report["summary"]["sections"], 1, "{report:#}");
    assert_eq!(report["summary"]["human"], 0);
    assert_eq!(report["flagged"][0]["file"], "src/lib.rs");
}
