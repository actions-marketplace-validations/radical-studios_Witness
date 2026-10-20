//! Scenario builder for end-to-end tests: a temporary git repository, the
//! `witness` binary, and simulated agent sessions fed through `witness hook`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

/// Runs `witness` in `dir` with `stdin`.
pub fn witness(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_witness"))
        .args(args)
        .current_dir(dir)
        .env_remove("WITNESS_SESSION_ID")
        .env_remove("WITNESS_DEBUG")
        .env("NO_COLOR", "1")
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

/// Runs `git` in `dir` with a fixed identity and no user configuration.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Asserts a command succeeded and returns its stdout.
pub fn stdout_of(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// A temporary git repository on branch `main`.
pub struct TestRepo {
    _tmp: tempfile::TempDir,
    pub path: PathBuf,
}

impl TestRepo {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        git(&path, &["init", "-q", "-b", "main"]);
        Self { _tmp: tmp, path }
    }

    /// Runs `witness init` and checks it wrote the config.
    pub fn init_witness(&self) {
        stdout_of(&self.witness(&["init"], None));
        assert!(self.path.join("witness.toml").is_file());
    }

    pub fn abs(&self, file: &str) -> PathBuf {
        self.path.join(file)
    }

    pub fn write(&self, file: &str, content: &str) {
        let path = self.abs(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    pub fn read(&self, file: &str) -> String {
        fs::read_to_string(self.abs(file)).unwrap()
    }

    pub fn git(&self, args: &[&str]) -> String {
        git(&self.path, args)
    }

    /// Stages everything and commits.
    pub fn commit_all(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    pub fn witness(&self, args: &[&str], stdin: Option<&str>) -> Output {
        witness(&self.path, args, stdin)
    }

    pub fn report_markdown(&self, base: &str, head: &str) -> String {
        stdout_of(&self.witness(&["report", "--base", base, "--head", head], None))
    }

    pub fn report_json(&self, base: &str, head: &str) -> Value {
        let out = self.witness(
            &["report", "--base", base, "--head", head, "--format", "json"],
            None,
        );
        serde_json::from_str(&stdout_of(&out)).unwrap()
    }

    /// A simulated Claude Code session in this repository.
    pub fn claude(&self, session_id: &str) -> ClaudeSession<'_> {
        ClaudeSession {
            repo: self,
            id: session_id.to_owned(),
            cwd: self.path.clone(),
        }
    }
}

/// Feeds Claude Code-shaped hook payloads to `witness hook`.
pub struct ClaudeSession<'a> {
    pub repo: &'a TestRepo,
    pub id: String,
    /// The agent's working directory.
    pub cwd: PathBuf,
}

impl ClaudeSession<'_> {
    /// The same session, working in `dir` (relative to the repo).
    pub fn in_dir(mut self, dir: &str) -> Self {
        self.cwd = self.repo.abs(dir);
        self
    }

    pub fn payload(&self, event: &str, tool: &str, input: Value, response: Value) -> String {
        json!({
            "session_id": self.id,
            "transcript_path": format!("/home/user/.claude/projects/x/{}.jsonl", self.id),
            "cwd": self.cwd.to_str().unwrap(),
            "permission_mode": "acceptEdits",
            "hook_event_name": event,
            "tool_name": tool,
            "tool_input": input,
            "tool_response": response,
            "tool_use_id": "toolu_01",
        })
        .to_string()
    }

    /// Runs `witness hook <event_arg>` and checks it was silent and clean.
    pub fn hook(&self, event_arg: &str, json: &str) {
        let out = self.repo.witness(&["hook", event_arg], Some(json));
        assert!(out.status.success());
        assert!(
            out.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    pub fn read(&self, file: &str) {
        let path = self.repo.abs(file);
        let json = self.payload(
            "PostToolUse",
            "Read",
            json!({"file_path": path.to_str().unwrap()}),
            json!({"type": "text"}),
        );
        self.hook("post-tool-use", &json);
    }

    /// Applies an Edit on disk and reports it the way Claude Code does
    /// (without a structuredPatch, so capture has to locate the new text).
    pub fn edit(&self, file: &str, old: &str, new: &str) {
        let text = self.repo.read(file);
        assert!(text.contains(old), "{old:?} not in {file}");
        self.repo.write(file, &text.replacen(old, new, 1));
        let path = self.repo.abs(file);
        let json = self.payload(
            "PostToolUse",
            "Edit",
            json!({"file_path": path.to_str().unwrap(), "old_string": old, "new_string": new, "replace_all": false}),
            json!({"filePath": path.to_str().unwrap(), "userModified": false}),
        );
        self.hook("post-tool-use", &json);
    }

    /// Creates a file with the Write tool.
    pub fn create(&self, file: &str, content: &str) {
        self.repo.write(file, content);
        let path = self.repo.abs(file);
        let json = self.payload(
            "PostToolUse",
            "Write",
            json!({"file_path": path.to_str().unwrap(), "content": content}),
            json!({"type": "create", "filePath": path.to_str().unwrap(), "structuredPatch": []}),
        );
        self.hook("post-tool-use", &json);
    }

    /// A Bash command that exited 0.
    pub fn bash(&self, command: &str) {
        let json = self.payload(
            "PostToolUse",
            "Bash",
            json!({"command": command, "description": "run"}),
            json!({"stdout": "ok", "stderr": "", "interrupted": false, "isImage": false}),
        );
        self.hook("post-tool-use", &json);
    }

    /// A Bash command that exited with `code`.
    pub fn bash_fail(&self, command: &str, code: i32) {
        let json = json!({
            "session_id": self.id,
            "cwd": self.cwd.to_str().unwrap(),
            "hook_event_name": "PostToolUseFailure",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_use_id": "toolu_02",
            "error": format!("Exit code {code}\nfailed"),
            "is_interrupt": false,
        })
        .to_string();
        self.hook("post-tool-use-failure", &json);
    }

    /// `witness assume`, as the agent would run it.
    pub fn assume(&self, file: &str, lines: Option<&str>, text: &str) {
        let mut args = vec!["assume", "--session", self.id.as_str(), "--file", file];
        if let Some(lines) = lines {
            args.extend(["--lines", lines]);
        }
        args.push(text);
        stdout_of(&self.repo.witness(&args, None));
    }

    pub fn end(&self) {
        let json = json!({
            "session_id": self.id,
            "cwd": self.cwd.to_str().unwrap(),
            "hook_event_name": "SessionEnd",
            "reason": "other",
        })
        .to_string();
        self.hook("session-end", &json);
    }
}
