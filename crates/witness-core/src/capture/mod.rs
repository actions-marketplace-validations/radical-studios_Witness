//! Turns coding-agent hook payloads into session log records.
//!
//! Each agent has an adapter that parses its payload into a [`HookInput`],
//! an agent-neutral description of what happened. [`events_for`] then turns
//! that into records. Everything here is pure: the caller supplies the
//! clock, the repo root, a function that reads files, and any file changes
//! it observed with snapshots (see [`capture_plan`]).

use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use time::OffsetDateTime;

use crate::config::Config;
use crate::hunk::Hunk;
use crate::record::EditVia;

mod claude_code;
mod codex;
mod copilot;
mod cursor;
mod events;
mod gemini;
mod generic;
pub mod textdiff;

pub use events::{change_from_patch, events_for, line_count, locate};

/// Agent names as recorded in logs and accepted by `witness hook --agent`.
pub const AGENTS: &[&str] = &[
    claude_code::NAME,
    codex::NAME,
    copilot::NAME,
    cursor::NAME,
    gemini::NAME,
    generic::NAME,
];

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("hook payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("hook payload has no session id")]
    MissingSession,
    #[error("unknown agent {0:?}; expected one of {AGENTS:?}")]
    UnknownAgent(String),
    #[error("hook payload is not a JSON object")]
    NotAnObject,
}

/// One hook call, in agent-neutral terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInput {
    /// Agent name, e.g. `claude-code`.
    pub agent: String,
    pub session_id: String,
    /// The agent's working directory, used to find the repo and to resolve
    /// relative paths.
    pub cwd: Option<String>,
    /// Turn (user prompt) id.
    pub turn: Option<String>,
    /// Subagent type or id, when a subagent made the call.
    pub subagent: Option<String>,
    pub model: Option<String>,
    pub event: HookEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    Tool(ToolCall),
    SessionStart,
    SessionEnd {
        reason: String,
    },
    /// The agent finished a turn.
    Stop {
        /// The agent is already continuing because a stop hook blocked.
        hook_active: bool,
        /// The agent's final message. Read in memory, never stored.
        last_message: Option<String>,
    },
    /// An event Witness doesn't use.
    Ignored(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before the tool runs.
    Pre,
    /// After it succeeded.
    Post,
    /// After it failed.
    PostFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub phase: Phase,
    /// The agent's tool name, as given.
    pub tool: String,
    /// Pairs a pre-tool call with its post-tool call.
    pub call_id: Option<String>,
    pub action: ToolAction,
}

/// What a tool call does, as far as Witness cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAction {
    Shell(ShellCall),
    /// Edits to known files.
    Edit(Vec<FileEdit>),
    Read(Vec<String>),
    /// A tool that can't change files (search, web, planning, ...).
    NoWrite,
    /// A tool Witness doesn't know; it may change files.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCall {
    pub command: String,
    /// Directory the command runs in, when it differs from the session's.
    pub cwd: Option<String>,
    pub background: bool,
    pub interrupted: bool,
    pub exit: Exit,
    /// Combined output, read in memory to find test results. Never stored.
    pub output: Option<String>,
}

/// What is known about a command's exit status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    /// It failed, with an unknown code.
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileEdit {
    /// As given by the agent: absolute, or relative to the cwd.
    pub path: String,
    /// The agent's own patch of the edit, if it reports one.
    pub patch: Vec<PatchHunk>,
    /// `(old, new)` text replacements, used to locate the edit.
    pub replacements: Vec<(String, String)>,
    /// Full content written.
    pub content: Option<String>,
    /// The tool created the file.
    pub created: bool,
    /// Whether the edit's range can only be found by comparing the file
    /// before and after (no reliable patch from the agent).
    pub needs_preimage: bool,
}

/// One hunk of an agent-reported patch: unified-diff lines with context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHunk {
    pub old_start: u32,
    pub new_start: u32,
    pub old_lines: Option<u32>,
    pub new_lines: Option<u32>,
    /// Lines prefixed with ` `, `-`, `+` or `\`; `None` if not reported.
    pub lines: Option<Vec<String>>,
}

/// Parses a hook payload. `agent` comes from `witness hook --agent`; when
/// absent it is guessed from the payload's shape. `event_arg` is the event
/// named on the command line, used when the payload doesn't name its event
/// (Copilot's camelCase payloads).
pub fn parse_hook(
    json: &str,
    agent: Option<&str>,
    event_arg: &str,
) -> Result<HookInput, CaptureError> {
    let value: Value = serde_json::from_str(json)?;
    if !value.is_object() {
        return Err(CaptureError::NotAnObject);
    }
    let agent = match agent {
        Some(a) => a,
        None => detect_agent(&value),
    };
    let input = match agent {
        claude_code::NAME => claude_code::parse(&value),
        codex::NAME => codex::parse(&value),
        copilot::NAME => copilot::parse(&value, event_arg),
        cursor::NAME => cursor::parse(&value),
        gemini::NAME => gemini::parse(&value),
        generic::NAME => generic::parse(&value),
        other => return Err(CaptureError::UnknownAgent(other.to_owned())),
    }?;
    if input.session_id.trim().is_empty() {
        return Err(CaptureError::MissingSession);
    }
    Ok(input)
}

/// Guesses the agent from a payload's shape.
pub fn detect_agent(v: &Value) -> &'static str {
    let has = |k: &str| v.get(k).is_some();
    let event = v.get("hook_event_name").and_then(Value::as_str);
    if has("witness") {
        generic::NAME
    } else if has("conversation_id") || has("generation_id") || has("workspace_roots") {
        cursor::NAME
    } else if has("sessionId") || has("toolArgs") || has("tool_result") {
        copilot::NAME
    } else if has("turn_id") {
        codex::NAME
    } else if event.is_some_and(gemini::is_event) {
        gemini::NAME
    } else {
        claude_code::NAME
    }
}

/// Context for converting one payload.
pub struct CaptureContext<'a> {
    pub config: &'a Config,
    /// Absolute repo root (the directory containing `.witness/`).
    pub root: &'a Path,
    /// The root with symlinks resolved, when that differs from `root`. Agents
    /// may report either form (e.g. `/var/...` and `/private/var/...` on
    /// macOS), so paths are matched against both.
    pub canonical_root: Option<&'a Path>,
    pub now: OffsetDateTime,
}

/// File changes the caller observed around a tool call by comparing the
/// files before and after it. Paths are relative to the repo root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub via: EditVia,
    pub hunks: Vec<Hunk>,
}

/// The I/O the caller should do around a tool call so that its file
/// changes can be observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapturePlan {
    /// Nothing: the call can't change files, or reports its own patch.
    None,
    /// Copy these files (absolute paths) before the tool runs and diff them
    /// after.
    Preimages(Vec<PathBuf>),
    /// Snapshot the working tree before and after.
    Tree,
}

/// Decides how to observe a tool call's file changes.
pub fn capture_plan(input: &HookInput, ctx: &CaptureContext<'_>) -> CapturePlan {
    let HookEvent::Tool(call) = &input.event else {
        return CapturePlan::None;
    };
    match &call.action {
        ToolAction::Shell(shell) => {
            if shell.background
                || crate::command::is_read_only(&shell.command)
                || crate::command::moves_worktree(&shell.command)
            {
                CapturePlan::None
            } else {
                CapturePlan::Tree
            }
        }
        ToolAction::Edit(files) => {
            let paths: Vec<PathBuf> = files
                .iter()
                .filter(|f| f.needs_preimage)
                .filter_map(|f| resolve(input, ctx, Some(&f.path)).map(|(_, abs)| abs))
                .collect();
            if paths.is_empty() {
                CapturePlan::None
            } else {
                CapturePlan::Preimages(paths)
            }
        }
        ToolAction::Unknown => CapturePlan::Tree,
        ToolAction::Read(_) | ToolAction::NoWrite => CapturePlan::None,
    }
}

/// A key that pairs a pre-tool call with its post-tool call: the agent's
/// call id, or else a hash of what the call does.
pub fn call_key(input: &HookInput) -> Option<String> {
    let HookEvent::Tool(call) = &input.event else {
        return None;
    };
    let raw = match (&call.call_id, &call.action) {
        (Some(id), _) => format!("{}\0{id}", input.session_id),
        (None, ToolAction::Shell(shell)) => {
            format!("{}\0{}\0{}", input.session_id, call.tool, shell.command)
        }
        (None, ToolAction::Edit(files)) => {
            let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            format!("{}\0{}\0{}", input.session_id, call.tool, paths.join("\0"))
        }
        (None, _) => format!("{}\0{}", input.session_id, call.tool),
    };
    let digest = <sha2::Sha256 as sha2::Digest>::digest(raw.as_bytes());
    Some(digest.iter().take(12).map(|b| format!("{b:02x}")).collect())
}

/// Resolves a tool's file path to (repo-relative `/` path, absolute path).
/// Returns `None` for paths outside the repo root.
pub(crate) fn resolve(
    input: &HookInput,
    ctx: &CaptureContext<'_>,
    path: Option<&str>,
) -> Option<(String, PathBuf)> {
    let path = Path::new(path?);
    let abs = if path.is_absolute() {
        normalize(path)
    } else {
        let base = input.cwd.as_deref().map_or(ctx.root, Path::new);
        normalize(&base.join(path))
    };
    let rel = relative_to(&abs, &normalize(ctx.root))
        .or_else(|| relative_to(&abs, &normalize(ctx.canonical_root?)))?;
    Some((rel, abs))
}

/// Lexically normalizes `.` and `..` components.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `path` relative to `root`, `/`-separated, or `None` if outside it.
pub fn relative_to(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Reads `Exit code N` from the first line of a failure message.
pub fn parse_exit_code(error: &str) -> Option<i32> {
    let first = error.lines().next()?.trim();
    first.strip_prefix("Exit code ")?.trim().parse().ok()
}

// Payload helpers shared by adapters.

fn str_at<'v>(v: &'v Value, keys: &[&str]) -> Option<&'v str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

fn owned_at(v: &Value, keys: &[&str]) -> Option<String> {
    str_at(v, keys).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn bool_at(v: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_bool))
        .unwrap_or(false)
}

/// A field that may hold an object or a JSON-encoded string of one.
fn object_at(v: &Value, keys: &[&str]) -> Value {
    for k in keys {
        match v.get(*k) {
            Some(Value::String(s)) => {
                if let Ok(parsed @ Value::Object(_)) = serde_json::from_str::<Value>(s) {
                    return parsed;
                }
            }
            Some(obj @ Value::Object(_)) => return obj.clone(),
            _ => {}
        }
    }
    Value::Null
}

/// A text result that may be a string or nested in an object.
fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => {
            let parts: Vec<String> = [
                "stdout",
                "stderr",
                "output",
                "textResultForLlm",
                "text_result_for_llm",
                "llmContent",
                "returnDisplay",
                "content",
            ]
            .iter()
            .filter_map(|k| v.get(*k).and_then(text_of))
            .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(text_of).collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}

/// Paths named in an `apply_patch`-style patch (`*** Update File: x`).
fn patch_paths(patch: &str) -> Vec<String> {
    const MARKERS: [&str; 4] = [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ];
    let mut paths: Vec<String> = Vec::new();
    for line in patch.lines() {
        for m in MARKERS {
            if let Some(p) = line.trim_end().strip_prefix(m)
                && !p.is_empty()
                && !paths.iter().any(|x| x == p)
            {
                paths.push(p.to_owned());
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests;
