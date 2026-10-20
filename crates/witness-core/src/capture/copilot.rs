//! GitHub Copilot hooks (`.github/hooks/*.json`), used by Copilot CLI and
//! the Copilot cloud agent.
//!
//! Two payload formats exist, chosen by the event name in the hook config:
//! camelCase (`postToolUse`: `sessionId`, `toolName`, `toolArgs`,
//! `toolResult`, no event name in the payload) and the VS Code compatible
//! one (`PostToolUse`: `session_id`, `tool_name`, `tool_input`,
//! `tool_result`, `hook_event_name`). Both are read. Tool names are the
//! runtime ones (`bash`, `edit`, `create`, `view`, `apply_patch`, ...) or
//! their Claude equivalents. Results carry no structured exit code, so it is
//! read from the result text when present and otherwise left unknown.

use serde_json::Value;

use super::{
    CaptureError, Exit, FileEdit, HookEvent, HookInput, Phase, ShellCall, ToolAction, ToolCall,
    bool_at, object_at, owned_at, parse_exit_code, patch_paths, str_at, text_of,
};

pub const NAME: &str = "copilot";

const NO_WRITE: &[&str] = &[
    "grep",
    "rg",
    "glob",
    "web_fetch",
    "web_search",
    "ask_user",
    "update_todo",
    "task",
    "report_intent",
    "think",
    "read_bash",
    "list_bash",
    "stop_bash",
    "Grep",
    "Glob",
    "WebFetch",
    "WebSearch",
    "AskUserQuestion",
    "TodoWrite",
    "Agent",
    "Task",
];

pub fn parse(v: &Value, event_arg: &str) -> Result<HookInput, CaptureError> {
    let name = str_at(v, &["hook_event_name"]).unwrap_or(event_arg);
    let event = match normalize(name).as_str() {
        "pretooluse" => tool_event(v, Phase::Pre),
        "posttooluse" => tool_event(v, Phase::Post),
        "posttoolusefailure" => tool_event(v, Phase::PostFailure),
        "sessionstart" => HookEvent::SessionStart,
        "sessionend" => HookEvent::SessionEnd {
            reason: owned_at(v, &["reason"]).unwrap_or_else(|| "other".to_owned()),
        },
        "agentstop" | "stop" => HookEvent::Stop {
            hook_active: bool_at(v, &["stop_hook_active", "stopHookActive"]),
            last_message: None,
        },
        _ => HookEvent::Ignored(name.to_owned()),
    };
    Ok(HookInput {
        agent: NAME.to_owned(),
        session_id: owned_at(v, &["sessionId", "session_id"]).unwrap_or_default(),
        cwd: owned_at(v, &["cwd"]),
        turn: None,
        subagent: owned_at(v, &["agentType", "agent_type", "agentId", "agent_id"]),
        model: None,
        event,
    })
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase()
}

fn tool_event(v: &Value, phase: Phase) -> HookEvent {
    let tool = str_at(v, &["toolName", "tool_name"])
        .unwrap_or("")
        .to_owned();
    let args = object_at(v, &["toolArgs", "tool_input"]);
    let result = v
        .get("toolResult")
        .or_else(|| v.get("tool_result"))
        .unwrap_or(&Value::Null);
    let error = str_at(v, &["error"]);
    let path = || owned_at(&args, &["path", "file_path", "filePath", "file"]);
    let action = match tool.as_str() {
        "bash" | "powershell" | "Bash" | "shell" => match owned_at(&args, &["command", "cmd"]) {
            Some(command) => {
                let output = text_of(result).or_else(|| error.map(str::to_owned));
                let marked = output.as_deref().and_then(exit_code_marker);
                let exit = match phase {
                    Phase::Pre => Exit::Unknown,
                    Phase::Post => marked.map_or(Exit::Unknown, Exit::Code),
                    Phase::PostFailure => marked
                        .or_else(|| error.and_then(parse_exit_code))
                        .map_or(Exit::Failed, Exit::Code),
                };
                ToolAction::Shell(ShellCall {
                    command,
                    cwd: None,
                    background: str_at(&args, &["mode"]) == Some("async")
                        || bool_at(&args, &["async", "detach", "run_in_background"]),
                    interrupted: false,
                    exit,
                    output,
                })
            }
            None => ToolAction::Unknown,
        },
        "view" | "Read" | "read_file" => match path() {
            Some(p) => ToolAction::Read(vec![p]),
            None => ToolAction::NoWrite,
        },
        "create" | "Write" => match path() {
            Some(p) => ToolAction::Edit(vec![FileEdit {
                path: p,
                content: owned_at(&args, &["file_text", "content", "contents"]),
                created: tool == "create",
                needs_preimage: true,
                ..FileEdit::default()
            }]),
            None => ToolAction::Unknown,
        },
        "edit" | "str_replace_editor" | "str_replace" | "insert" | "Edit" => match path() {
            Some(p) => {
                let old = owned_at(&args, &["old_str", "old_string"]);
                let new = owned_at(&args, &["new_str", "new_string"]);
                ToolAction::Edit(vec![FileEdit {
                    path: p,
                    replacements: old.zip(new).into_iter().collect(),
                    needs_preimage: true,
                    ..FileEdit::default()
                }])
            }
            None => ToolAction::Unknown,
        },
        "apply_patch" => {
            let patch = owned_at(&args, &["patch", "input", "command"])
                .or_else(|| v.get("toolArgs").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            let files: Vec<FileEdit> = patch_paths(&patch)
                .into_iter()
                .map(|path| FileEdit {
                    path,
                    needs_preimage: true,
                    ..FileEdit::default()
                })
                .collect();
            if files.is_empty() {
                ToolAction::Unknown
            } else {
                ToolAction::Edit(files)
            }
        }
        t if NO_WRITE.contains(&t) => ToolAction::NoWrite,
        _ => ToolAction::Unknown,
    };
    HookEvent::Tool(ToolCall {
        phase,
        tool,
        call_id: owned_at(v, &["toolCallId", "tool_call_id", "tool_use_id"]),
        action,
    })
}

/// `<exited with exit code N>`, as Copilot's shell tool reports it.
fn exit_code_marker(text: &str) -> Option<i32> {
    const MARK: &str = "exited with exit code ";
    let idx = text.rfind(MARK)?;
    let digits: String = text[idx + MARK.len()..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}
