use std::path::{Path, PathBuf};

use time::macros::datetime;

use super::*;
use crate::config::Config;
use crate::record::{Change, CommandClass, EditVia, Event, EventKind};

const ROOT: &str = "/home/user/my-project";

struct Run<'a> {
    agent: Option<&'a str>,
    event_arg: &'a str,
    files: Vec<(PathBuf, String)>,
    observed: Option<Observed>,
}

impl<'a> Run<'a> {
    fn new() -> Self {
        Self {
            agent: Some(claude_code::NAME),
            event_arg: "",
            files: Vec::new(),
            observed: None,
        }
    }
    fn agent(mut self, agent: Option<&'a str>, event_arg: &'a str) -> Self {
        self.agent = agent;
        self.event_arg = event_arg;
        self
    }
    fn file(mut self, path: &str, content: &str) -> Self {
        self.files.push((PathBuf::from(path), content.to_owned()));
        self
    }
    fn observed(mut self, observed: Observed) -> Self {
        self.observed = Some(observed);
        self
    }
    fn events(&self, json: &str) -> Vec<Event> {
        let config = Config::default();
        let ctx = context(&config);
        let read = |p: &Path| {
            self.files
                .iter()
                .find(|(fp, _)| fp == p)
                .map(|(_, c)| c.clone())
        };
        let input = parse_hook(json, self.agent, self.event_arg).unwrap();
        events_for(&input, &ctx, &read, self.observed.as_ref())
    }
    fn kinds(&self, json: &str) -> Vec<EventKind> {
        self.events(json).into_iter().map(|e| e.kind).collect()
    }
}

fn context(config: &Config) -> CaptureContext<'_> {
    CaptureContext {
        config,
        root: Path::new(ROOT),
        canonical_root: Some(Path::new("/private/home/user/my-project")),
        now: datetime!(2026-10-20 03:37:00 UTC),
    }
}

fn capture(json: &str) -> Vec<EventKind> {
    Run::new().kinds(json)
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hooks")
        .join(name);
    std::fs::read_to_string(&path).unwrap()
}

fn input(json: &str, agent: Option<&str>, event_arg: &str) -> HookInput {
    parse_hook(json, agent, event_arg).unwrap()
}

fn edit(file: &str, start: u32, removed: u32, added: u32, tool: &str, via: EditVia) -> EventKind {
    let change = Change {
        start,
        removed,
        added,
    };
    EventKind::Edit {
        file: file.into(),
        lines: Some(change.new_range()),
        change: Some(change),
        tool: tool.into(),
        via: Some(via),
    }
}

fn command(class: CommandClass, exit: Option<i32>, text: &str) -> EventKind {
    EventKind::Command {
        classification: class,
        exit_code: exit,
        command: text.into(),
    }
}

// Claude Code.

/// Payloads captured from a real Claude Code 2.x session, with paths and
/// ids anonymized.
#[test]
fn real_claude_code_payloads() {
    let real = |name: &str| capture(&fixture(&format!("claude-code-v2/{name}.json")));
    assert_eq!(
        real("post_tool_use_read"),
        vec![EventKind::Read {
            file: "main.rs".into()
        }]
    );
    assert_eq!(
        real("post_tool_use_edit"),
        vec![edit("main.rs", 2, 1, 1, "Edit", EditVia::Patch)]
    );
    assert_eq!(
        real("post_tool_use_write"),
        vec![edit("notes.txt", 1, 0, 2, "Write", EditVia::Content)]
    );
    assert_eq!(
        real("post_tool_use_bash"),
        vec![command(CommandClass::Other, Some(0), "echo hi && exit 0")]
    );
    assert_eq!(
        real("post_tool_use_failure_bash"),
        vec![command(CommandClass::Other, Some(1), "false")]
    );
    assert_eq!(
        real("session_end"),
        vec![EventKind::SessionEnd {
            reason: "other".into()
        }]
    );
}

#[test]
fn claude_turn_and_context_fields_are_kept() {
    let events = Run::new().events(&fixture("claude-code-v2/post_tool_use_read.json"));
    assert_eq!(events[0].agent, "claude-code");
    assert_eq!(
        events[0].turn.as_deref(),
        Some("c05f2951-a336-40e5-8270-3b1a220a5ea6")
    );
    let sub = fixture("claude-code-v2/post_tool_use_read.json").replace(
        "\"hook_event_name\"",
        "\"agent_type\": \"Explore\", \"agent_id\": \"a1\", \"hook_event_name\"",
    );
    assert_eq!(
        Run::new().events(&sub)[0].subagent.as_deref(),
        Some("Explore")
    );
}

#[test]
fn bash_success_is_exit_zero() {
    assert_eq!(
        capture(&fixture("post_tool_use_bash.json")),
        vec![command(CommandClass::Test, Some(0), "npm test")]
    );
}

#[test]
fn bash_failure_reads_exit_code() {
    assert_eq!(
        capture(&fixture("post_tool_use_failure_bash.json")),
        vec![command(CommandClass::Test, Some(1), "npm test")]
    );
}

#[test]
fn bash_background_and_piped_have_no_exit_code() {
    let kinds = capture(&fixture("post_tool_use_bash_background.json"));
    assert!(matches!(
        kinds[0],
        EventKind::Command {
            exit_code: None,
            ..
        }
    ));
    let piped = fixture("post_tool_use_bash.json").replace("npm test", "cargo test 2>&1 | tail -5");
    assert!(matches!(
        capture(&piped)[0],
        EventKind::Command {
            classification: CommandClass::Test,
            exit_code: None,
            ..
        }
    ));
}

#[test]
fn bash_command_text_is_redacted() {
    let json = fixture("post_tool_use_bash.json").replace(
        "npm test",
        "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz012345 npm test",
    );
    assert!(matches!(
        &capture(&json)[0],
        EventKind::Command { command, .. } if command == "GITHUB_TOKEN=[REDACTED] npm test"
    ));
}

#[test]
fn write_create_covers_whole_new_file() {
    assert_eq!(
        capture(&fixture("post_tool_use_write.json")),
        vec![edit("src/greeting.rs", 1, 0, 3, "Write", EditVia::Content)]
    );
}

#[test]
fn edit_uses_structured_patch_without_context() {
    assert_eq!(
        capture(&fixture("post_tool_use_edit.json")),
        vec![edit("src/lib.rs", 12, 1, 2, "Edit", EditVia::Patch)]
    );
}

#[test]
fn edit_without_patch_locates_new_string_in_file() {
    let mut v: Value = serde_json::from_str(&fixture("post_tool_use_edit.json")).unwrap();
    v["tool_response"] = serde_json::json!({ "filePath": "/home/user/my-project/src/lib.rs" });
    let mut content = String::new();
    for i in 1..=11 {
        content.push_str(&format!("line {i}\n"));
    }
    content.push_str("    let total = items.iter().sum::<u32>();\n    total * 2\n}\n");
    let kinds = Run::new()
        .file("/home/user/my-project/src/lib.rs", &content)
        .kinds(&v.to_string());
    assert_eq!(
        kinds,
        vec![edit("src/lib.rs", 12, 1, 2, "Edit", EditVia::Locate)]
    );
}

#[test]
fn edit_that_cannot_be_located_covers_whole_file() {
    let mut v: Value = serde_json::from_str(&fixture("post_tool_use_edit.json")).unwrap();
    v["tool_response"] = Value::Null;
    assert!(matches!(
        &capture(&v.to_string())[0],
        EventKind::Edit {
            lines: None,
            change: None,
            via: Some(EditVia::Unknown),
            ..
        }
    ));
}

#[test]
fn read_records_relative_path() {
    assert_eq!(
        capture(&fixture("post_tool_use_read.json")),
        vec![EventKind::Read {
            file: "src/lib.rs".into()
        }]
    );
}

#[test]
fn files_outside_repo_and_witness_files_are_ignored() {
    let read = fixture("post_tool_use_read.json");
    for path in [
        "/etc/hosts",
        "/home/user/my-project/../other/x",
        "/home/user/my-project/.witness/sessions/s.jsonl",
        "/home/user/my-project/witness.toml",
    ] {
        let json = read.replace("/home/user/my-project/src/lib.rs", path);
        assert!(capture(&json).is_empty(), "{path}");
    }
}

#[test]
fn paths_under_the_canonical_root_resolve() {
    let json = fixture("post_tool_use_read.json").replace(
        "/home/user/my-project/src/lib.rs",
        "/private/home/user/my-project/src/lib.rs",
    );
    assert_eq!(
        capture(&json),
        vec![EventKind::Read {
            file: "src/lib.rs".into()
        }]
    );
}

#[test]
fn unrelated_events_and_tools_produce_nothing() {
    let pre = fixture("post_tool_use_bash.json").replace("\"PostToolUse\"", "\"PreToolUse\"");
    assert!(capture(&pre).is_empty());
    let glob = fixture("post_tool_use_read.json").replace("\"Read\"", "\"Glob\"");
    assert!(capture(&glob).is_empty());
}

#[test]
fn rejects_bad_payloads() {
    assert!(parse_hook("{", None, "").is_err());
    assert!(parse_hook("[]", None, "").is_err());
    assert!(parse_hook(r#"{"session_id":"","hook_event_name":"Stop"}"#, None, "").is_err());
    assert!(matches!(
        parse_hook(r#"{"session_id":"s"}"#, Some("vim"), ""),
        Err(CaptureError::UnknownAgent(_))
    ));
}

#[test]
fn parses_exit_code_line() {
    assert_eq!(
        parse_exit_code("Exit code 101\nerror: test failed"),
        Some(101)
    );
    assert_eq!(parse_exit_code("Command timed out"), None);
}

#[test]
fn patch_hunk_trims_context_and_handles_deletions() {
    let hunk = PatchHunk {
        old_start: 5,
        new_start: 5,
        old_lines: Some(7),
        new_lines: Some(5),
        lines: Some(
            [" a", " b", " c", "-d", "-e", " f", " g"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        ),
    };
    assert_eq!(
        change_from_patch(&hunk),
        Some(Change {
            start: 8,
            removed: 2,
            added: 0
        })
    );
    let only_context = PatchHunk {
        lines: Some(vec![" a".into()]),
        ..hunk
    };
    assert_eq!(change_from_patch(&only_context), None);
}

#[test]
fn locate_finds_every_occurrence() {
    let content = "a\nfoo\nb\nfoo\n";
    let found = locate(content, "bar", "foo").unwrap();
    assert_eq!(
        found.iter().map(|c| c.start).collect::<Vec<_>>(),
        vec![2, 4]
    );
    assert!(locate(content, "x", "").is_none());
    assert!(locate(content, "x", "zzz").is_none());
}

#[test]
fn counts_lines() {
    assert_eq!(line_count(""), 0);
    assert_eq!(line_count("a"), 1);
    assert_eq!(line_count("a\n"), 1);
    assert_eq!(line_count("a\nb"), 2);
}

// Observed changes and capture plans.

fn hunk(file: &str, old_start: u32, old_count: u32, new_start: u32, new_count: u32) -> Hunk {
    Hunk {
        file: file.into(),
        old_start,
        old_count,
        new_start,
        new_count,
        added: Vec::new(),
        removed: Vec::new(),
        binary: false,
    }
}

#[test]
fn shell_commands_record_observed_edits() {
    let json = fixture("post_tool_use_bash.json").replace("npm test", "sed -i s/a/b/ src/x.rs");
    let observed = Observed {
        via: EditVia::Snapshot,
        hunks: vec![
            hunk("src/x.rs", 3, 1, 3, 1),
            hunk("src/x.rs", 9, 2, 8, 0),
            hunk(".witness/sessions/s.jsonl", 1, 0, 2, 1),
        ],
    };
    let kinds = Run::new().observed(observed).kinds(&json);
    assert_eq!(
        kinds,
        vec![
            command(CommandClass::Other, Some(0), "sed -i s/a/b/ src/x.rs"),
            edit("src/x.rs", 3, 1, 1, "Bash", EditVia::Snapshot),
            edit("src/x.rs", 9, 2, 0, "Bash", EditVia::Snapshot),
        ]
    );
}

#[test]
fn capture_plans() {
    let config = Config::default();
    let ctx = context(&config);
    let plan = |json: &str, agent: Option<&str>| capture_plan(&input(json, agent, ""), &ctx);
    let bash = fixture("post_tool_use_bash.json").replace("\"PostToolUse\"", "\"PreToolUse\"");
    assert_eq!(plan(&bash, None), CapturePlan::Tree);
    assert_eq!(
        plan(&bash.replace("npm test", "git status && ls"), None),
        CapturePlan::None
    );
    // Switching branches changes files, but not as the agent's edits.
    assert_eq!(
        plan(&bash.replace("npm test", "git checkout main"), None),
        CapturePlan::None
    );
    assert_eq!(
        plan(&fixture("post_tool_use_bash_background.json"), None),
        CapturePlan::None
    );
    // Claude Code reports its own patches: no pre-image needed.
    assert_eq!(
        plan(&fixture("post_tool_use_edit.json"), None),
        CapturePlan::None
    );
    assert_eq!(
        plan(&fixture("codex/pre_tool_use_apply_patch.json"), None),
        CapturePlan::Preimages(vec![
            PathBuf::from("/home/user/my-project/src/lib.rs"),
            PathBuf::from("/home/user/my-project/src/new.rs"),
        ])
    );
    let mcp = fixture("post_tool_use_read.json").replace("\"Read\"", "\"mcp__fs__write\"");
    assert_eq!(plan(&mcp, None), CapturePlan::Tree);
}

#[test]
fn call_keys_pair_pre_and_post() {
    let pre = input(&fixture("codex/pre_tool_use_apply_patch.json"), None, "");
    let post = input(&fixture("codex/post_tool_use_apply_patch.json"), None, "");
    assert_eq!(call_key(&pre), call_key(&post));
    assert!(call_key(&pre).is_some());
    // Without a call id, the command text pairs them.
    let a = input(
        &fixture("copilot/post_tool_use_bash.json"),
        None,
        "pre-tool-use",
    );
    let b = input(
        &fixture("copilot/post_tool_use_bash.json"),
        None,
        "post-tool-use",
    );
    assert_eq!(call_key(&a), call_key(&b));
}

// Other agents.

#[test]
fn detects_agents_from_payload_shape() {
    for (name, agent) in [
        ("claude-code-v2/post_tool_use_bash.json", "claude-code"),
        ("codex/post_tool_use_bash.json", "codex"),
        ("copilot/post_tool_use_bash.json", "copilot"),
        ("copilot/post_tool_use_create_vscode.json", "copilot"),
        ("cursor/after_shell_execution.json", "cursor"),
        ("gemini-cli/after_tool_shell.json", "gemini-cli"),
        ("generic/post_tool_shell.json", "generic"),
    ] {
        let v: Value = serde_json::from_str(&fixture(name)).unwrap();
        assert_eq!(detect_agent(&v), agent, "{name}");
    }
}

#[test]
fn codex_payloads() {
    let events = Run::new()
        .agent(None, "")
        .events(&fixture("codex/post_tool_use_bash.json"));
    assert_eq!(events[0].agent, "codex");
    assert_eq!(events[0].turn.as_deref(), Some("turn-3"));
    assert_eq!(events[0].model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(
        events[0].kind,
        command(CommandClass::Test, Some(101), "cargo test")
    );

    // apply_patch: the pre-images say what changed.
    let observed = Observed {
        via: EditVia::Preimage,
        hunks: vec![
            hunk("src/lib.rs", 2, 1, 2, 1),
            hunk("src/new.rs", 0, 0, 1, 1),
        ],
    };
    let kinds = Run::new()
        .agent(None, "")
        .observed(observed)
        .kinds(&fixture("codex/post_tool_use_apply_patch.json"));
    assert_eq!(
        kinds,
        vec![
            edit("src/lib.rs", 2, 1, 1, "apply_patch", EditVia::Preimage),
            edit("src/new.rs", 1, 0, 1, "apply_patch", EditVia::Preimage),
        ]
    );

    let stop = input(&fixture("codex/stop.json"), None, "");
    assert_eq!(
        stop.event,
        HookEvent::Stop {
            hook_active: false,
            last_message: Some("Done. All tests pass.".into())
        }
    );
}

#[test]
fn codex_command_arrays_and_exit_lines() {
    let json = fixture("codex/post_tool_use_bash.json").replace(
        r#""command": "cargo test""#,
        r#""command": ["bash", "-lc", "cargo test -p core"]"#,
    );
    assert_eq!(
        Run::new().agent(Some("codex"), "").kinds(&json),
        vec![command(CommandClass::Test, Some(101), "cargo test -p core")]
    );
    assert_eq!(
        codex::exit_code_in_output("Chunk ID: 1\nProcess exited with code 0\nOutput:"),
        Some(0)
    );
    assert_eq!(codex::exit_code_in_output("just output"), None);
}

#[test]
fn copilot_camel_case_payloads() {
    let run = |name: &str, event: &'static str| {
        Run::new()
            .agent(Some("copilot"), event)
            .events(&fixture(&format!("copilot/{name}.json")))
    };
    let ok = run("post_tool_use_bash", "postToolUse");
    assert_eq!(ok[0].agent, "copilot");
    assert_eq!(ok[0].session_id, "c0p1l07-5e55-4a1b-9c2d-0e1f2a3b4c5d");
    assert_eq!(ok[0].kind, command(CommandClass::Test, Some(0), "npm test"));
    let failed = run("post_tool_use_failure_bash", "post-tool-use-failure");
    assert_eq!(
        failed[0].kind,
        command(CommandClass::Test, Some(1), "npm test")
    );
    // Without an exit marker in the output, success is not assumed.
    let unmarked =
        fixture("copilot/post_tool_use_bash.json").replace("\\n<exited with exit code 0>", "");
    assert_eq!(
        Run::new()
            .agent(Some("copilot"), "postToolUse")
            .kinds(&unmarked),
        vec![command(CommandClass::Test, None, "npm test")]
    );
    let stop = input(
        &fixture("copilot/agent_stop.json"),
        Some("copilot"),
        "agentStop",
    );
    assert!(matches!(
        stop.event,
        HookEvent::Stop {
            hook_active: false,
            ..
        }
    ));
}

#[test]
fn copilot_edits_use_preimages_or_locate() {
    let json = fixture("copilot/post_tool_use_edit.json");
    let config = Config::default();
    let plan = capture_plan(
        &input(&json, Some("copilot"), "preToolUse"),
        &context(&config),
    );
    assert_eq!(
        plan,
        CapturePlan::Preimages(vec![PathBuf::from("/home/user/my-project/src/lib.rs")])
    );
    // No pre-image (the pre-hook didn't run): fall back to locating.
    let kinds = Run::new()
        .agent(Some("copilot"), "postToolUse")
        .file(
            "/home/user/my-project/src/lib.rs",
            "fn a() {\n    1 + 1\n}\n",
        )
        .kinds(&json);
    assert_eq!(
        kinds,
        vec![edit("src/lib.rs", 2, 1, 1, "edit", EditVia::Locate)]
    );
    // The VS Code compatible format, creating a file.
    let kinds = Run::new()
        .agent(None, "")
        .kinds(&fixture("copilot/post_tool_use_create_vscode.json"));
    assert_eq!(
        kinds,
        vec![edit("src/greeting.rs", 1, 0, 3, "create", EditVia::Content)]
    );
}

#[test]
fn cursor_payloads() {
    let events = Run::new()
        .agent(None, "")
        .events(&fixture("cursor/after_shell_execution.json"));
    assert_eq!(events[0].agent, "cursor");
    assert_eq!(events[0].session_id, "cur-2f9a");
    assert_eq!(events[0].turn.as_deref(), Some("gen-71c3"));
    assert_eq!(
        events[0].kind,
        command(CommandClass::Test, None, "pytest -q")
    );
    let kinds = Run::new()
        .agent(None, "")
        .file(
            "/home/user/my-project/src/lib.rs",
            "fn a() {\n    1 + 1\n}\n",
        )
        .kinds(&fixture("cursor/after_file_edit.json"));
    assert_eq!(
        kinds,
        vec![edit("src/lib.rs", 2, 1, 1, "edit", EditVia::Locate)]
    );
}

#[test]
fn gemini_payloads() {
    let events = Run::new()
        .agent(None, "")
        .events(&fixture("gemini-cli/after_tool_shell.json"));
    assert_eq!(events[0].agent, "gemini-cli");
    assert_eq!(
        events[0].kind,
        command(CommandClass::Test, Some(0), "go test ./...")
    );
    let pre = input(&fixture("gemini-cli/before_tool_replace.json"), None, "");
    let config = Config::default();
    assert_eq!(
        capture_plan(&pre, &context(&config)),
        CapturePlan::Preimages(vec![PathBuf::from("/home/user/my-project/src/lib.rs")])
    );
}

#[test]
fn generic_payloads() {
    let events = Run::new()
        .agent(None, "")
        .events(&fixture("generic/post_tool_shell.json"));
    assert_eq!(events[0].agent, "my-agent");
    assert_eq!(events[0].turn.as_deref(), Some("t1"));
    assert_eq!(
        events[0].kind,
        command(CommandClass::Other, Some(2), "make test")
    );
    assert_eq!(
        Run::new()
            .agent(Some("generic"), "")
            .kinds(&fixture("generic/post_tool_edit.json")),
        vec![edit("notes/plan.md", 1, 0, 2, "write", EditVia::Content)]
    );
}

#[test]
fn patch_paths_are_read_from_apply_patch_text() {
    let patch = "*** Begin Patch\n*** Update File: a.rs\n*** Move to: b.rs\n@@\n-x\n+y\n*** Delete File: c.rs\n*** Update File: a.rs\n*** End Patch";
    assert_eq!(patch_paths(patch), vec!["a.rs", "b.rs", "c.rs"]);
}
