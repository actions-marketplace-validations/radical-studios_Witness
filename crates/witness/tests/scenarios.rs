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
    assert_eq!(report["skim"][0]["status"], "tested");
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
    assert_eq!(report["skim"][0]["status"], "tested", "{report:#}");
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

const LIB: &str = "fn alpha() -> u32 {\n    1\n}\n\nfn beta() -> u32 {\n    2\n}\n";

/// Rebasing onto a base that added lines above the agent's code moves it;
/// attribution follows the content, not the old line numbers.
#[test]
fn attribution_survives_a_rebase() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", LIB);
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let agent = repo.claude("s");
    agent.edit(
        "src/lib.rs",
        "fn beta() -> u32 {\n    2\n}\n",
        "fn beta() -> u32 {\n    let doubled = compute_double(1);\n    doubled\n}\n",
    );
    agent.bash("cargo test");
    repo.commit_all("agent change");

    // Meanwhile, main grows above the agent's code.
    repo.git(&["checkout", "-q", "main"]);
    repo.write(
        "src/lib.rs",
        &format!("// header line one\n// header line two\n// header line three\n{LIB}"),
    );
    repo.commit_all("main moves on");
    repo.git(&["checkout", "-q", "feature"]);
    repo.git(&["rebase", "-q", "main"]);

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["sections"], 1, "{report:#}");
    assert_eq!(report["summary"]["human"], 0);
    assert_eq!(report["skim"][0]["status"], "tested");
    assert_eq!(report["skim"][0]["lines"], "9-10");
}

/// A human edit inside agent code becomes its own (human) section.
#[test]
fn human_changes_to_agent_lines_are_split_out() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", LIB);
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let agent = repo.claude("s");
    agent.edit(
        "src/lib.rs",
        "    2\n",
        "    let first_value = load_first();\n    let second_value = load_second();\n    first_value + second_value\n",
    );
    // The human then rewrites the middle line by hand.
    let text = repo
        .read("src/lib.rs")
        .replace("load_second()", "load_second_cached()");
    repo.write("src/lib.rs", &text);
    repo.commit_all("feature");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["sections"], 2, "{report:#}");
    assert_eq!(report["summary"]["human"], 1);
    let lines: Vec<_> = report["flagged"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lines"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(lines, vec!["6", "8"]);
}

/// Code from a commit attributed to an agent that left no record is
/// flagged, unless the branch has that agent's records.
#[test]
fn agent_commits_without_records_are_unrecorded() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", LIB);
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.write(
        "src/lib.rs",
        &LIB.replace("    2\n", "    compute_value(2)\n"),
    );
    repo.git(&["add", "-A"]);
    repo.git(&[
        "commit",
        "-q",
        "-m",
        "Compute beta\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>",
    ]);
    repo.write("src/other.rs", "pub fn other() {}\n");
    repo.commit_all("human commit");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["unrecorded"], 1, "{report:#}");
    assert_eq!(report["summary"]["human"], 1);
    assert_eq!(report["flagged"][0]["status"], "unrecorded");
    assert_eq!(report["flagged"][0]["agent"], "claude-code");
    assert_eq!(report["flagged"][0]["file"], "src/lib.rs");

    // Once Claude Code's sessions are recorded on the branch, lines outside
    // the record are trusted to be human.
    repo.claude("s").read("src/lib.rs");
    repo.commit_all("add the session log");
    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["unrecorded"], 0, "{report:#}");
}

/// `cargo test | tail` hides cargo's exit code, but its summary line still
/// says the tests failed.
#[test]
fn piped_test_runs_are_judged_by_their_summary() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", LIB);
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let agent = repo.claude("s");
    agent.read("src/lib.rs");
    agent.edit("src/lib.rs", "    2\n", "    compute_beta_value()\n");
    agent.bash_output(
        "cargo test 2>&1 | tail -3",
        "test tests::beta ... FAILED\n\ntest result: FAILED. 7 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
    );
    repo.commit_all("feature");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(report["summary"]["failing"], 1, "{report:#}");
    assert_eq!(report["flagged"][0]["status"], "failing");
    assert_eq!(
        report["flagged"][0]["reason"],
        "`cargo test 2>&1 | tail -3` failed (1 failed) after the last edit"
    );
}

/// `cargo test -p a` says nothing about crate `b`.
#[test]
fn scoped_runs_cover_only_their_packages() {
    let repo = TestRepo::new();
    repo.write("Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n");
    repo.write("crates/a/Cargo.toml", "[package]\nname = \"a\"\n");
    repo.write("crates/a/src/lib.rs", LIB);
    repo.write("crates/b/Cargo.toml", "[package]\nname = \"b\"\n");
    repo.write("crates/b/src/lib.rs", LIB);
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let agent = repo.claude("s");
    agent.read("crates/a/src/lib.rs");
    agent.read("crates/b/src/lib.rs");
    agent.edit(
        "crates/a/src/lib.rs",
        "    2\n",
        "    compute_in_crate_a()\n",
    );
    agent.edit(
        "crates/b/src/lib.rs",
        "    2\n",
        "    compute_in_crate_b()\n",
    );
    agent.bash("cargo test -p a");
    repo.commit_all("feature");

    let report = repo.report_json("main", "HEAD");
    assert_eq!(
        report["skim"][0]["file"], "crates/a/src/lib.rs",
        "{report:#}"
    );
    assert_eq!(report["skim"][0]["status"], "tested");
    assert_eq!(report["flagged"][0]["file"], "crates/b/src/lib.rs");
    assert_eq!(report["flagged"][0]["status"], "unverified");
    let reason = report["flagged"][0]["reason"].as_str().unwrap();
    assert!(reason.ends_with("covers only `crates/a`"), "{reason}");
}

/// A test edited right after it failed is put up for close review even
/// though the tests then passed.
#[test]
fn tests_changed_after_failing_are_escalated() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", LIB);
    repo.write(
        "tests/beta.rs",
        "#[test]\nfn beta_is_two() {\n    assert_eq!(beta(), 2);\n}\n",
    );
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let agent = repo.claude("s");
    agent.read("src/lib.rs");
    agent.read("tests/beta.rs");
    agent.edit("src/lib.rs", "    2\n", "    compute_beta_value()\n");
    agent.bash_fail("cargo test", 101);
    agent.edit(
        "tests/beta.rs",
        "    assert_eq!(beta(), 2);\n",
        "    assert!(beta() > 0);\n",
    );
    agent.bash("cargo test");
    repo.commit_all("feature");

    let report = repo.report_json("main", "HEAD");
    let flagged = &report["flagged"][0];
    assert_eq!(flagged["file"], "tests/beta.rs", "{report:#}");
    assert_eq!(flagged["status"], "tested");
    assert_eq!(flagged["signals"][0]["kind"], "test-changed-after-failure");
    let md = repo.report_markdown("main", "HEAD");
    assert!(md.contains("changed after `cargo test` failed"), "{md}");
}

/// `witness status` reports on uncommitted work too, and says when session
/// logs aren't committed yet.
#[test]
fn status_covers_uncommitted_work() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", LIB);
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let agent = repo.claude("s");
    agent.read("src/lib.rs");
    agent.edit("src/lib.rs", "    2\n", "    compute_beta_value()\n");
    agent.create("src/new.rs", "pub fn brand_new_function() {}\n");

    // Nothing committed: base is found on its own, and both files show.
    let out = repo.witness(&["status", "--format", "text", "--all"], None);
    let text = support::stdout_of(&out);
    assert!(
        text.starts_with("Witness: review 2 of 2 sections closely (main...working tree)"),
        "{text}"
    );
    assert!(text.contains("src/new.rs:1"), "{text}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("1 session log is not committed yet"),
        "{stderr}"
    );

    // --fail-on turns findings into an exit code.
    let failed = repo.witness(
        &["status", "--format", "json", "--fail-on", "unverified"],
        None,
    );
    assert_eq!(failed.status.code(), Some(1));
    let passed = repo.witness(
        &["status", "--format", "json", "--fail-on", "failing"],
        None,
    );
    assert_eq!(passed.status.code(), Some(0));
    let bad = repo.witness(&["status", "--fail-on", "sloppy"], None);
    assert!(!bad.status.success());
}

#[test]
fn explain_and_sessions() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", LIB);
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let agent = repo.claude("sess-42");
    agent.read("src/lib.rs");
    agent.edit("src/lib.rs", "    2\n", "    compute_beta_value()\n");
    agent.bash("cargo test -p other");
    agent.bash_fail("cargo test", 101);
    repo.commit_all("feature");

    let text = support::stdout_of(&repo.witness(&["explain", "src/lib.rs:6"], None));
    assert!(
        text.starts_with("src/lib.rs:6: failing (lines 6)"),
        "{text}"
    );
    assert!(
        text.contains("claude-code in session sess-42: edited with Edit (located in the file)"),
        "{text}"
    );
    assert!(text.contains("`cargo test` failed"), "{text}");
    let human = support::stdout_of(&repo.witness(&["explain", "src/lib.rs:1"], None));
    assert!(human.contains("not changed on this branch"), "{human}");

    let table = support::stdout_of(&repo.witness(&["sessions"], None));
    assert!(table.contains("sess-42"), "{table}");
    let json: serde_json::Value = serde_json::from_str(&support::stdout_of(
        &repo.witness(&["sessions", "--json"], None),
    ))
    .unwrap();
    assert_eq!(json[0]["edits"], 1);
    assert_eq!(json[0]["checks"], 2);
    assert_eq!(json[0]["checks_failed"], 1);
}

#[test]
fn ci_output_formats() {
    let repo = TestRepo::new();
    repo.write("src/lib.rs", LIB);
    repo.init_witness();
    repo.commit_all("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.git(&["remote", "add", "origin", "git@github.com:acme/app.git"]);
    let agent = repo.claude("s");
    agent.read("src/lib.rs");
    agent.edit("src/lib.rs", "    2\n", "    compute_beta_value()\n");
    repo.commit_all("feature");

    let report =
        |format: &str| support::stdout_of(&repo.witness(&["report", "--format", format], None));
    let md = report("markdown");
    let sha = repo.git(&["rev-parse", "HEAD"]);
    let link = format!(
        "https://github.com/acme/app/blob/{}/src/lib.rs#L6",
        sha.trim()
    );
    assert!(md.contains(&link), "{md}");
    let annotations = report("github");
    assert!(
        annotations.starts_with(
            "::warning file=src/lib.rs,line=6,endLine=6,title=Witness%3A unverified::"
        ),
        "{annotations}"
    );
    let sarif: serde_json::Value = serde_json::from_str(&report("sarif")).unwrap();
    assert_eq!(
        sarif["runs"][0]["results"][0]["ruleId"],
        "witness/unverified"
    );
    let gitlab: serde_json::Value = serde_json::from_str(&report("gitlab-codequality")).unwrap();
    assert_eq!(gitlab[0]["location"]["lines"]["begin"], 6);
}
