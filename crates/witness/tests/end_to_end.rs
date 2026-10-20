//! End to end: a temp git repo, a simulated Claude Code session fed through
//! `witness hook`, commits, then `witness report`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

fn witness(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_witness"))
        .args(args)
        .current_dir(dir)
        .env_remove("WITNESS_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
    }
    child.wait_with_output().unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn payload(repo: &Path, event: &str, tool: &str, input: Value, response: Value) -> String {
    json!({
        "session_id": "sess-1",
        "transcript_path": "/home/user/.claude/projects/x/sess-1.jsonl",
        "cwd": repo.to_str().unwrap(),
        "permission_mode": "acceptEdits",
        "hook_event_name": event,
        "tool_name": tool,
        "tool_input": input,
        "tool_response": response,
        "tool_use_id": "toolu_01",
    })
    .to_string()
}

fn hook(repo: &Path, event_arg: &str, json: &str) {
    let out = witness(repo, &["hook", event_arg], Some(json));
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Applies an Edit on disk and reports it the way Claude Code does (without
/// a structuredPatch, so capture has to locate the new text itself).
fn edit(repo: &Path, file: &str, old: &str, new: &str) {
    let path = repo.join(file);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains(old));
    fs::write(&path, text.replacen(old, new, 1)).unwrap();
    let json = payload(
        repo,
        "PostToolUse",
        "Edit",
        json!({"file_path": path.to_str().unwrap(), "old_string": old, "new_string": new, "replace_all": false}),
        json!({"filePath": path.to_str().unwrap(), "userModified": false}),
    );
    hook(repo, "post-tool-use", &json);
}

fn bash(repo: &Path, command: &str) {
    let json = payload(
        repo,
        "PostToolUse",
        "Bash",
        json!({"command": command, "description": "run"}),
        json!({"stdout": "ok", "stderr": "", "interrupted": false, "isImage": false}),
    );
    hook(repo, "post-tool-use", &json);
}

const LIB_RS: &str = "pub fn a() -> u32 {\n    1\n}\n\npub fn middle() -> u32 {\n    5\n}\n\npub fn b() -> u32 {\n    2\n}\n";

#[test]
fn report_after_a_recorded_session() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // Base branch with witness initialized.
    git(repo, &["init", "-q", "-b", "main"]);
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(repo.join("src/config.rs"), "pub const RETRIES: u32 = 3;\n").unwrap();
    let init = witness(repo, &["init"], None);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(repo.join("witness.toml").is_file());
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["checkout", "-q", "-b", "feature"]);

    // The agent's session.
    let lib = repo.join("src/lib.rs");
    hook(
        repo,
        "post-tool-use",
        &payload(
            repo,
            "PostToolUse",
            "Read",
            json!({"file_path": lib.to_str().unwrap()}),
            json!({"type": "text"}),
        ),
    );
    edit(repo, "src/lib.rs", "    1\n", "    1 + 1\n");

    let parse_rs = repo.join("src/parse.rs");
    let content = "pub fn parse(input: &[u8]) -> String {\n    let text = std::str::from_utf8(input).unwrap_or_default();\n    text.trim().to_owned()\n}\n";
    fs::write(&parse_rs, content).unwrap();
    hook(
        repo,
        "post-tool-use",
        &payload(
            repo,
            "PostToolUse",
            "Write",
            json!({"file_path": parse_rs.to_str().unwrap(), "content": content}),
            json!({"type": "create", "filePath": parse_rs.to_str().unwrap(), "structuredPatch": []}),
        ),
    );

    bash(repo, "cargo test");
    edit(repo, "src/lib.rs", "    2\n", "    2 * 3\n");

    let assume = witness(
        repo,
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
    );
    assert!(
        assume.status.success(),
        "{}",
        String::from_utf8_lossy(&assume.stderr)
    );

    bash(repo, "cargo test 2>&1 | tail -20");
    hook(
        repo,
        "session-end",
        &json!({"session_id": "sess-1", "cwd": repo.to_str().unwrap(), "hook_event_name": "SessionEnd", "reason": "other"})
            .to_string(),
    );

    // A human change in the same branch.
    fs::write(repo.join("src/config.rs"), "pub const RETRIES: u32 = 5;\n").unwrap();

    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "feature"]);

    let log = fs::read_to_string(repo.join(".witness/sessions/sess-1.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 8, "{log}");
    assert!(
        !log.contains("from_utf8"),
        "file contents must never be stored"
    );
    assert!(
        !repo.join(".witness/state").exists()
            || String::from_utf8(
                Command::new("git")
                    .args(["ls-files", ".witness/state"])
                    .current_dir(repo)
                    .output()
                    .unwrap()
                    .stdout
            )
            .unwrap()
            .is_empty(),
        "local state must not be committed"
    );

    let out = witness(repo, &["report", "--base", "main", "--head", "HEAD"], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let markdown = String::from_utf8(out.stdout).unwrap();
    insta::assert_snapshot!("report_markdown", markdown);

    let out = witness(
        repo,
        &[
            "report", "--base", "main", "--head", "HEAD", "--format", "json",
        ],
        None,
    );
    assert!(out.status.success());
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["summary"]["sections"], 3);
    assert_eq!(report["summary"]["review_closely"], 2);
    assert_eq!(report["summary"]["human"], 1);
    assert_eq!(report["flagged"][0]["status"], "guessed");

    let out = witness(repo, &["post", "--platform", "dry-run"], None);
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("<!-- witness:report -->")
    );
    let out = witness(repo, &["post", "--platform", "github"], None);
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
    let json = payload(
        repo,
        "PostToolUse",
        "Bash",
        json!({"command": "cargo test"}),
        json!({}),
    );
    let out = witness(repo, &["hook", "post-tool-use"], Some(&json));
    assert!(out.status.success());
    assert!(!repo.join(".witness").exists());
}
