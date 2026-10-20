//! Gemini CLI hooks (`.gemini/settings.json`). Beta: payload shapes are
//! taken from Gemini CLI's documentation and are read leniently.
//!
//! `BeforeTool`/`AfterTool` carry `tool_name`, `tool_input` and, after the
//! call, `tool_response`; shell output reports `Exit Code: N`.

use serde_json::Value;

use super::{
    CaptureError, Exit, FileEdit, HookEvent, HookInput, Phase, ShellCall, ToolAction, ToolCall,
    bool_at, owned_at, str_at, text_of,
};

pub const NAME: &str = "gemini-cli";

const NO_WRITE: &[&str] = &[
    "read_many_files",
    "glob",
    "search_file_content",
    "list_directory",
    "web_fetch",
    "google_web_search",
    "save_memory",
    "write_todos",
];

/// Event names only Gemini CLI uses (`SessionStart` and the like are
/// shared with other agents).
pub fn is_event(name: &str) -> bool {
    matches!(
        name,
        "BeforeTool"
            | "AfterTool"
            | "BeforeAgent"
            | "AfterAgent"
            | "BeforeModel"
            | "AfterModel"
            | "BeforeToolSelection"
            | "PreCompress"
    )
}

pub fn parse(v: &Value) -> Result<HookInput, CaptureError> {
    let event = match str_at(v, &["hook_event_name"]).unwrap_or("") {
        "BeforeTool" => tool_event(v, Phase::Pre),
        "AfterTool" => tool_event(v, Phase::Post),
        "SessionStart" => HookEvent::SessionStart,
        "SessionEnd" => HookEvent::SessionEnd {
            reason: owned_at(v, &["reason"]).unwrap_or_else(|| "other".to_owned()),
        },
        "AfterAgent" => HookEvent::Stop {
            hook_active: bool_at(v, &["stop_hook_active"]),
            last_message: owned_at(v, &["prompt_response", "last_assistant_message"]),
        },
        other => HookEvent::Ignored(other.to_owned()),
    };
    Ok(HookInput {
        agent: NAME.to_owned(),
        session_id: owned_at(v, &["session_id"]).unwrap_or_default(),
        cwd: owned_at(v, &["cwd"]),
        turn: None,
        subagent: None,
        model: None,
        event,
    })
}

fn tool_event(v: &Value, phase: Phase) -> HookEvent {
    let tool = str_at(v, &["tool_name"]).unwrap_or("").to_owned();
    let input = v.get("tool_input").unwrap_or(&Value::Null);
    let response = v.get("tool_response").unwrap_or(&Value::Null);
    let path = || owned_at(input, &["file_path", "absolute_path", "path"]);
    let action = match tool.as_str() {
        "run_shell_command" => match owned_at(input, &["command"]) {
            Some(command) => {
                let output = text_of(response);
                let exit = match phase {
                    Phase::Pre => Exit::Unknown,
                    _ => output
                        .as_deref()
                        .and_then(exit_code_line)
                        .map_or(Exit::Unknown, Exit::Code),
                };
                ToolAction::Shell(ShellCall {
                    command,
                    cwd: owned_at(input, &["directory", "dir_path"]),
                    background: bool_at(input, &["is_background"]),
                    interrupted: false,
                    exit,
                    output,
                })
            }
            None => ToolAction::Unknown,
        },
        "write_file" => match path() {
            Some(p) => ToolAction::Edit(vec![FileEdit {
                path: p,
                content: owned_at(input, &["content"]),
                needs_preimage: true,
                ..FileEdit::default()
            }]),
            None => ToolAction::Unknown,
        },
        "replace" | "edit" => match path() {
            Some(p) => {
                let old = owned_at(input, &["old_string"]);
                let new = owned_at(input, &["new_string"]);
                ToolAction::Edit(vec![FileEdit {
                    path: p,
                    replacements: old.zip(new).into_iter().collect(),
                    needs_preimage: true,
                    ..FileEdit::default()
                }])
            }
            None => ToolAction::Unknown,
        },
        "read_file" => match path() {
            Some(p) => ToolAction::Read(vec![p]),
            None => ToolAction::NoWrite,
        },
        t if NO_WRITE.contains(&t) => ToolAction::NoWrite,
        _ => ToolAction::Unknown,
    };
    HookEvent::Tool(ToolCall {
        phase,
        tool,
        call_id: owned_at(v, &["tool_call_id", "call_id"]),
        action,
    })
}

/// `Exit Code: N` on a line of shell output.
fn exit_code_line(text: &str) -> Option<i32> {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Exit Code:"))
        .and_then(|rest| rest.trim().parse().ok())
}
