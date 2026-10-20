//! End-to-end scenarios built with the `support` scenario builder.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;
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

/// Files a shell command changes (here, as `sed -i` would) are recorded
/// from working-tree snapshots, so they count as agent-written.
#[test]
fn shell_edits_are_agent_written() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "fn a() -> u32 {\n    1\n}\n");
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);

    let agent = repo.claude("s");
    agent.bash_with("sed -i 's/1/2/' src/lib.rs", |r| {
        r.write("src/lib.rs", "fn a() -> u32 {\n    2\n}\n");
    });
    agent.bash_with("python gen.py", |r| r.write("src/gen.rs", "// generated\n"));
    repo.commit_all("feature");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["sections"], 2, "{report:#}");
    assert_eq!(report["summary"]["human"], 0);
    let log = repo.read(".witness/sessions/s.jsonl");
    assert!(log.contains(r#""via":"snapshot""#), "{log}");
    assert!(
        !log.contains("generated"),
        "file contents must never be stored"
    );
}

/// A Codex session: `apply_patch` edits are diffed from pre-images, and the
/// exit code of `Bash` comes from its output text.
#[test]
fn codex_session_end_to_end() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "fn a() -> u32 {\n    1\n}\n");
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);

    let cwd = repo.path.to_str().unwrap();
    let patch =
        "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-    1\n+    1 + 1\n*** End Patch\n";
    let base = |event: &str| {
        json!({
            "session_id": "cx-1", "turn_id": "t1", "transcript_path": null, "cwd": cwd,
            "hook_event_name": event, "model": "gpt-5-codex", "permission_mode": "default",
        })
    };
    let mut pre = base("PreToolUse");
    pre["tool_name"] = json!("apply_patch");
    pre["tool_input"] = json!({"command": patch});
    pre["tool_use_id"] = json!("call_1");
    support::send_hook(&repo, &["--agent", "codex", "pre-tool-use"], &pre);
    repo.write("src/lib.rs", "fn a() -> u32 {\n    1 + 1\n}\n");
    let mut post = pre.clone();
    post["hook_event_name"] = json!("PostToolUse");
    post["tool_response"] = json!("Success. Updated the following files:\nM src/lib.rs");
    support::send_hook(&repo, &["--agent", "codex", "post-tool-use"], &post);

    let mut test = base("PostToolUse");
    test["tool_name"] = json!("Bash");
    test["tool_input"] = json!({"command": "cargo test"});
    test["tool_response"] = json!("Exit code: 0\nWall time: 1.0 seconds\nOutput:\nok");
    test["tool_use_id"] = json!("call_2");
    support::send_hook(&repo, &["--agent", "codex", "post-tool-use"], &test);
    repo.commit_all("feature");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["sections"], 1, "{report:#}");
    assert_eq!(report["skim"][0]["status"], "checked");
    assert_eq!(report["skim"][0]["agent"], "codex");
    let log = repo.read(".witness/sessions/cx-1.jsonl");
    assert!(log.contains(r#""via":"preimage""#), "{log}");
    assert!(log.contains(r#""turn":"t1""#), "{log}");
}

/// A Copilot CLI session in the camelCase format, with no event name in the
/// payloads: the event comes from the hook command line.
#[test]
fn copilot_session_end_to_end() {
    let repo = TestRepo::new();
    repo.write("src/app.js", "export const a = 1;\n");
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);

    let cwd = repo.path.to_str().unwrap();
    let file = repo.abs("src/app.js");
    let edit = json!({
        "sessionId": "cp-1", "timestamp": 1_792_468_620_000_i64, "cwd": cwd,
        "toolName": "edit",
        "toolArgs": json!({"path": file, "old_str": "= 1", "new_str": "= 2"}).to_string(),
    });
    support::send_hook(&repo, &["--agent", "copilot", "pre-tool-use"], &edit);
    repo.write("src/app.js", "export const a = 2;\n");
    let mut done = edit.clone();
    done["toolResult"] = json!({"resultType": "success", "textResultForLlm": "ok"});
    support::send_hook(&repo, &["--agent", "copilot", "post-tool-use"], &done);

    let test = json!({
        "sessionId": "cp-1", "timestamp": 1_792_468_640_000_i64, "cwd": cwd,
        "toolName": "bash", "toolArgs": {"command": "npm test"},
        "error": "Tests: 1 failed\n<exited with exit code 1>",
    });
    support::send_hook(
        &repo,
        &["--agent", "copilot", "post-tool-use-failure"],
        &test,
    );
    repo.commit_all("feature");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["review_closely"], 1, "{report:#}");
    let reason = report["flagged"][0]["reason"].as_str().unwrap();
    assert!(reason.contains("`npm test` failed (exit 1)"), "{reason}");
    assert_eq!(report["flagged"][0]["agent"], "copilot");
}

/// `witness exec` passes output through, exits with the command's code and
/// records the exact result, even when its output is piped.
#[test]
fn exec_records_exact_exit_codes_and_changes() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "fn a() {}\n");
    repo.init_witness();
    repo.commit_all("base");

    let out = repo.witness(
        &[
            "exec",
            "--session",
            "m1",
            "--",
            "sh",
            "-c",
            "echo hi; exit 3",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");

    let out = repo.witness(
        &["exec", "--session", "m1", "sh -c 'echo x > src/new.rs'"],
        None,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let log = repo.read(".witness/sessions/m1.jsonl");
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["kind"], "command");
    assert_eq!(lines[0]["exit_code"], 3);
    assert_eq!(lines[0]["command"], "sh -c 'echo hi; exit 3'");
    assert!(
        lines
            .iter()
            .any(|l| l["kind"] == "edit" && l["file"] == "src/new.rs"),
        "{log}"
    );
}

#[test]
fn record_writes_manual_events() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", "fn a() {}\nfn b() {}\n");
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write("src/lib.rs", "fn a() { 1; }\nfn b() {}\n");

    let rec = |args: &[&str]| {
        let mut full = vec!["record", "--session", "r1", "--agent", "aider"];
        full.extend_from_slice(args);
        support::stdout_of(&repo.witness(&full, None));
    };
    rec(&["edit", "--file", "src/lib.rs", "--lines", "1"]);
    rec(&["command", "--exit-code", "0", "--", "cargo", "test"]);
    repo.commit_all("feature");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["skim"][0]["status"], "checked", "{report:#}");
    assert_eq!(report["skim"][0]["agent"], "aider");
}

#[test]
fn hooks_install_merges_and_uninstalls() {
    let repo = TestRepo::new();
    repo.init_witness();
    repo.write(
        ".cursor/hooks.json",
        r#"{"version": 1, "hooks": {"afterFileEdit": [{"command": "./format.sh"}]}}"#,
    );
    support::stdout_of(&repo.witness(&["hooks", "install", "--all"], None));
    for file in [
        ".claude/settings.json",
        ".codex/hooks.json",
        ".github/hooks/witness.json",
        ".cursor/hooks.json",
        ".gemini/settings.json",
    ] {
        assert!(repo.read(file).contains("hook --agent "), "{file}");
    }
    let status = support::stdout_of(&repo.witness(&["hooks", "status"], None));
    assert!(status.contains("codex        installed"), "{status}");

    support::stdout_of(&repo.witness(&["hooks", "uninstall", "--all"], None));
    assert!(!repo.abs(".github/hooks/witness.json").exists());
    let cursor = repo.read(".cursor/hooks.json");
    assert!(
        cursor.contains("./format.sh") && !cursor.contains("hook --agent"),
        "{cursor}"
    );
}
