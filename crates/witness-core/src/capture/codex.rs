//! OpenAI Codex CLI hooks (`.codex/hooks.json`).
//!
//! Payloads follow `codex-rs/hooks/src/schema.rs`: Claude-like fields plus
//! `turn_id` and `model`. Shell commands are the `Bash` tool with
//! `tool_input.command`; `PostToolUse` fires for them whatever the exit
//! status, and `tool_response` is the output text, which carries the exit
//! code. File edits go through `apply_patch`, whose `tool_input.command` is
//! the patch; the files it names are diffed before and after.

use serde_json::Value;

use super::{
    CaptureError, Exit, FileEdit, HookEvent, HookInput, Phase, ShellCall, ToolAction, ToolCall,
    bool_at, owned_at, patch_paths, str_at, text_of,
};

pub const NAME: &str = "codex";

const SHELL_TOOLS: &[&str] = &[
    "Bash",
    "shell",
    "local_shell",
    "exec_command",
    "unified_exec",
];
const NO_WRITE: &[&str] = &[
    "update_plan",
    "web_search",
    "view_image",
    "read_file",
    "list_dir",
    "grep_files",
    "write_stdin",
];

pub fn parse(v: &Value) -> Result<HookInput, CaptureError> {
    let event = match str_at(v, &["hook_event_name"]).unwrap_or("") {
        "PreToolUse" => tool_event(v, Phase::Pre),
        "PostToolUse" => tool_event(v, Phase::Post),
        "SessionStart" => HookEvent::SessionStart,
        "UserPromptSubmit" => HookEvent::Prompt,
        "SessionEnd" => HookEvent::SessionEnd {
            reason: owned_at(v, &["reason", "source"]).unwrap_or_else(|| "other".to_owned()),
        },
        "Stop" => HookEvent::Stop {
            hook_active: bool_at(v, &["stop_hook_active"]),
            last_message: owned_at(v, &["last_assistant_message"]),
        },
        other => HookEvent::Ignored(other.to_owned()),
    };
    Ok(HookInput {
        agent: NAME.to_owned(),
        session_id: owned_at(v, &["session_id"]).unwrap_or_default(),
        cwd: owned_at(v, &["cwd"]),
        turn: owned_at(v, &["turn_id"]),
        subagent: owned_at(v, &["agent_type", "agent_id"]),
        model: owned_at(v, &["model"]),
        event,
    })
}

fn tool_event(v: &Value, phase: Phase) -> HookEvent {
    let tool = str_at(v, &["tool_name"]).unwrap_or("").to_owned();
    let input = v.get("tool_input").unwrap_or(&Value::Null);
    let response = v.get("tool_response").unwrap_or(&Value::Null);
    let action = if SHELL_TOOLS.contains(&tool.as_str()) {
        match command_text(input) {
            Some(command) => {
                let output = text_of(response);
                let exit = match phase {
                    Phase::Pre => Exit::Unknown,
                    _ => output
                        .as_deref()
                        .and_then(exit_code_in_output)
                        .map_or(Exit::Unknown, Exit::Code),
                };
                ToolAction::Shell(ShellCall {
                    command,
                    cwd: owned_at(input, &["workdir", "cwd"]),
                    background: false,
                    interrupted: false,
                    exit,
                    output,
                })
            }
            None => ToolAction::Unknown,
        }
    } else if tool == "apply_patch" {
        let patch = command_text(input).unwrap_or_default();
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
    } else if NO_WRITE.contains(&tool.as_str()) {
        ToolAction::NoWrite
    } else {
        ToolAction::Unknown
    };
    HookEvent::Tool(ToolCall {
        phase,
        tool,
        call_id: owned_at(v, &["tool_use_id"]),
        action,
    })
}

/// `tool_input.command` as a string, or an argv array joined with spaces.
fn command_text(input: &Value) -> Option<String> {
    match input.get("command")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            // `["bash", "-lc", "cargo test"]`: the script is what ran.
            match words.as_slice() {
                [shell, flag, script] if is_shell(shell) && flag.starts_with('-') => {
                    Some((*script).to_owned())
                }
                _ => Some(words.join(" ")),
            }
        }
        _ => None,
    }
}

fn is_shell(program: &str) -> bool {
    matches!(
        program.rsplit('/').next(),
        Some("bash" | "sh" | "zsh" | "dash")
    )
}

/// Codex output text starts with `Exit code: N` or says
/// `Process exited with code N`.
pub fn exit_code_in_output(text: &str) -> Option<i32> {
    for line in text.lines().take(8) {
        let line = line.trim();
        let rest = line
            .strip_prefix("Exit code:")
            .or_else(|| line.strip_prefix("Process exited with code"));
        if let Some(code) = rest.and_then(|r| r.trim().parse().ok()) {
            return Some(code);
        }
    }
    None
}
