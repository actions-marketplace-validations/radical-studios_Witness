//! End to end: a temp git repo, a simulated Claude Code session fed through
//! `witness hook`, commits, then `witness report`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;
use support::{TestRepo, stdout_of, witness};

const LIB_RS: &str = "pub fn a() -> u32 {\n    1\n}\n\npub fn middle() -> u32 {\n    5\n}\n\npub fn b() -> u32 {\n    2\n}\n";

#[test]
fn report_after_a_recorded_session() {
    let repo = TestRepo::new();

    // Base branch with witness initialized.
    repo.write("src/lib.rs", LIB_RS);
    repo.write("src/config.rs", "pub const RETRIES: u32 = 3;\n");
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);

    // The agent's session.
    let agent = repo.claude("sess-1");
    agent.read("src/lib.rs");
    agent.edit("src/lib.rs", "    1\n", "    1 + 1\n");
    agent.create(
        "src/parse.rs",
        "pub fn parse(input: &[u8]) -> String {\n    let text = std::str::from_utf8(input).unwrap_or_default();\n    text.trim().to_owned()\n}\n",
    );
    agent.bash("cargo test");
    agent.edit("src/lib.rs", "    2\n", "    2 * 3\n");

    // Without --session, `witness assume` uses the session the hooks saw last.
    stdout_of(&repo.witness(
        &[
            "assume",
            "--file",
            "src/parse.rs",
            "--lines",
            "2-3",
            "callers",
            "always",
            "pass",
            "UTF-8",
        ],
        None,
    ));

    agent.bash("cargo test 2>&1 | tail -20");
    agent.end();

    // A human change in the same branch.
    repo.write("src/config.rs", "pub const RETRIES: u32 = 5;\n");
    repo.commit_all("feature");

    let log = repo.read(".witness/sessions/sess-1.jsonl");
    assert_eq!(log.lines().count(), 8, "{log}");
    assert!(
        !log.contains("from_utf8"),
        "file contents must never be stored"
    );
    assert!(
        repo.git(&["ls-files", ".witness/state"]).is_empty(),
        "local state must not be committed"
    );

    let markdown = repo.report_markdown("main", "HEAD");
    insta::assert_snapshot!("report_markdown", markdown);

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["sections"], 3);
    assert_eq!(report["summary"]["review_closely"], 2);
    assert_eq!(report["summary"]["human"], 1);
    assert_eq!(report["flagged"][0]["status"], "guessed");

    let out = repo.witness(&["post", "--platform", "dry-run"], None);
    assert!(stdout_of(&out).contains("<!-- witness:report -->"));
    let out = repo.witness(&["post", "--platform", "github"], None);
    assert!(!out.status.success());
}

#[test]
fn hook_never_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for (args, stdin) in [
        (vec!["hook", "post-tool-use"], "not json"),
        (vec!["hook"], ""),
        (vec!["hook", "post-tool-use", "--bogus"], "{}"),
    ] {
        let out = witness(repo, &args, Some(stdin));
        assert!(out.status.success(), "{args:?}");
    }
    // Without `witness init`, nothing is written.
    let json = json!({
        "session_id": "sess-1",
        "cwd": repo.to_str().unwrap(),
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": "cargo test"},
        "tool_response": {},
    })
    .to_string();
    let out = witness(repo, &["hook", "post-tool-use"], Some(&json));
    assert!(out.status.success());
    assert!(!repo.join(".witness").exists());
}
