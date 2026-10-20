//! Scenario builder for end-to-end tests: a temporary git repository, the
//! `witness` binary, and simulated agent sessions fed through `witness hook`.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Value, json};

/// Environment variables that change what `witness` does (CI detection,
/// tokens, base branch); tests start without them.
const AMBIENT: &[&str] = &[
    "WITNESS_SESSION_ID",
    "WITNESS_DEBUG",
    "WITNESS_BASE",
    "WITNESS_GITHUB_TOKEN",
    "WITNESS_GITLAB_TOKEN",
    "GITHUB_ACTIONS",
    "GITHUB_BASE_REF",
    "GITHUB_EVENT_PATH",
    "GITHUB_REF",
    "GITHUB_REPOSITORY",
    "GITHUB_SERVER_URL",
    "GITHUB_API_URL",
    "GITHUB_STEP_SUMMARY",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GITLAB_CI",
    "GITLAB_TOKEN",
    "CI_API_V4_URL",
    "CI_PROJECT_ID",
    "CI_PROJECT_URL",
    "CI_MERGE_REQUEST_IID",
    "CI_MERGE_REQUEST_DIFF_BASE_SHA",
    "CI_MERGE_REQUEST_TARGET_BRANCH_NAME",
];

/// Runs `witness` in `dir` with `stdin`.
pub fn witness(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    witness_env(dir, args, stdin, &[])
}

/// Runs `witness` with extra environment variables.
pub fn witness_env(dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_witness"));
    for var in AMBIENT {
        cmd.env_remove(var);
    }
    let mut child = cmd
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
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
    git_env(dir, args, &[])
}

/// [`git`] with extra environment variables.
pub fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
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
        .envs(env.iter().copied())
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

    /// Runs `witness init` (without agent hooks) and checks it wrote the
    /// config.
    pub fn init_witness(&self) {
        stdout_of(&self.witness(&["init", "--no-hooks"], None));
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

    /// Runs git with the author and committer dates set to `time` (Unix
    /// seconds).
    pub fn git_at(&self, time: i64, args: &[&str]) -> String {
        let date = format!("@{time} +0000");
        git_env(
            &self.path,
            args,
            &[("GIT_AUTHOR_DATE", &date), ("GIT_COMMITTER_DATE", &date)],
        )
    }

    /// [`Self::commit_all`] at `time`.
    pub fn commit_all_at(&self, time: i64, message: &str) {
        self.git(&["add", "-A"]);
        self.git_at(time, &["commit", "-q", "-m", message]);
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

    /// A new tool id: Claude Code gives each call a unique one.
    pub fn tool_id(&self) -> String {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        format!("toolu_{}", CALLS.fetch_add(1, Ordering::Relaxed))
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
            "tool_use_id": self.tool_id(),
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

    /// A Bash command that changes files: `PreToolUse`, then `effect` (what
    /// the command did), then `PostToolUse` with exit 0.
    pub fn bash_with(&self, command: &str, effect: impl FnOnce(&TestRepo)) {
        let id = self.tool_id();
        let pre = json!({
            "session_id": self.id,
            "cwd": self.cwd.to_str().unwrap(),
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_use_id": id,
        })
        .to_string();
        self.hook("pre-tool-use", &pre);
        effect(self.repo);
        let post = json!({
            "session_id": self.id,
            "cwd": self.cwd.to_str().unwrap(),
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": {"stdout": "", "stderr": "", "interrupted": false},
            "tool_use_id": id,
        })
        .to_string();
        self.hook("post-tool-use", &post);
    }

    /// A Bash command that exited 0 and printed `stdout`.
    pub fn bash_output(&self, command: &str, stdout: &str) {
        let json = self.payload(
            "PostToolUse",
            "Bash",
            json!({"command": command, "description": "run"}),
            json!({"stdout": stdout, "stderr": "", "interrupted": false, "isImage": false}),
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
            "tool_use_id": self.tool_id(),
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

    /// The agent ends its turn with `message`; returns the hook's stdout.
    pub fn stop(&self, message: &str, hook_active: bool) -> String {
        let json = json!({
            "session_id": self.id,
            "cwd": self.cwd.to_str().unwrap(),
            "hook_event_name": "Stop",
            "stop_hook_active": hook_active,
            "last_assistant_message": message,
        })
        .to_string();
        stdout_of(&self.repo.witness(&["hook", "stop"], Some(&json)))
    }

    /// The user sends a prompt; returns the hook's stdout.
    pub fn prompt(&self) -> String {
        let json = json!({
            "session_id": self.id,
            "cwd": self.cwd.to_str().unwrap(),
            "hook_event_name": "UserPromptSubmit",
            "prompt": "next",
        })
        .to_string();
        stdout_of(
            &self
                .repo
                .witness(&["hook", "user-prompt-submit"], Some(&json)),
        )
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

/// Sends a hook payload as `witness hook <args...>` and checks it was
/// silent and clean.
pub fn send_hook(repo: &TestRepo, args: &[&str], payload: &Value) {
    let mut full = vec!["hook"];
    full.extend_from_slice(args);
    let out = repo.witness(&full, Some(&payload.to_string()));
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A one-thread HTTP server answering requests with scripted responses, for
/// testing posting through `curl`.
pub struct FakeApi {
    pub url: String,
    handle: Option<std::thread::JoinHandle<Vec<(String, String)>>>,
}

impl FakeApi {
    /// Serves `responses` (status, JSON body) in order, then stops.
    pub fn start(responses: Vec<(u16, String)>) -> Self {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                }
                let mut request_body = vec![0; length];
                reader.read_exact(&mut request_body).unwrap();
                seen.push((
                    request_line.trim().to_owned(),
                    String::from_utf8(request_body).unwrap(),
                ));
                let mut stream = reader.into_inner();
                write!(
                    stream,
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            seen
        });
        Self {
            url,
            handle: Some(handle),
        }
    }

    /// The requests served: request line and body.
    pub fn requests(mut self) -> Vec<(String, String)> {
        self.handle.take().unwrap().join().unwrap()
    }
}
