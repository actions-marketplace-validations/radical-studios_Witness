//! Witness's own hook format, for agents without a dedicated adapter and
//! for wrapper scripts. Documented in `docs/AGENTS.md`.
//!
//! ```json
//! {"witness": 1, "agent": "my-agent", "event": "post_tool",
//!  "session_id": "abc", "cwd": "/repo", "turn": "t1",
//!  "tool": {"name": "shell", "kind": "shell", "call_id": "c1",
//!           "command": "cargo test", "exit_code": 0, "output": "..."}}
//! ```
//!
//! `event`: `pre_tool`, `post_tool`, `post_tool_failure`, `session_start`,
//! `prompt`, `session_end` (with `reason`) or `stop`. `tool.kind`: `shell`, `edit`
//! (with `files: [{path, content?, created?, old?, new?}]`), `read` (with
//! `files: [{path}]`), `no_write` or `unknown`.

use serde_json::Value;

use super::{
    CaptureError, Exit, FileEdit, HookEvent, HookInput, Phase, ShellCall, ToolAction, ToolCall,
    bool_at, owned_at, str_at,
};

pub const NAME: &str = "generic";

pub fn parse(v: &Value) -> Result<HookInput, CaptureError> {
    let event = match str_at(v, &["event"]).unwrap_or("") {
        "pre_tool" => tool_event(v, Phase::Pre),
        "post_tool" => tool_event(v, Phase::Post),
        "post_tool_failure" => tool_event(v, Phase::PostFailure),
        "session_start" => HookEvent::SessionStart,
        "prompt" => HookEvent::Prompt,
        "session_end" => HookEvent::SessionEnd {
            reason: owned_at(v, &["reason"]).unwrap_or_else(|| "other".to_owned()),
        },
        "stop" => HookEvent::Stop {
            hook_active: bool_at(v, &["stop_hook_active"]),
            last_message: owned_at(v, &["last_message"]),
        },
        other => HookEvent::Ignored(other.to_owned()),
    };
    Ok(HookInput {
        agent: owned_at(v, &["agent"]).unwrap_or_else(|| NAME.to_owned()),
        session_id: owned_at(v, &["session_id"]).unwrap_or_default(),
        cwd: owned_at(v, &["cwd"]),
        turn: owned_at(v, &["turn"]),
        subagent: owned_at(v, &["subagent"]),
        model: owned_at(v, &["model"]),
        event,
    })
}

fn tool_event(v: &Value, phase: Phase) -> HookEvent {
    let tool = v.get("tool").unwrap_or(&Value::Null);
    let name = owned_at(tool, &["name"]).unwrap_or_else(|| "tool".to_owned());
    let files = || -> Vec<&Value> {
        tool.get("files")
            .and_then(Value::as_array)
            .map(|f| f.iter().collect())
            .unwrap_or_default()
    };
    let action = match str_at(tool, &["kind"]).unwrap_or("unknown") {
        "shell" => match owned_at(tool, &["command"]) {
            Some(command) => {
                let code = tool
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok());
                ToolAction::Shell(ShellCall {
                    command,
                    cwd: owned_at(tool, &["cwd"]),
                    background: bool_at(tool, &["background"]),
                    interrupted: bool_at(tool, &["interrupted"]),
                    exit: match (code, phase) {
                        (Some(c), _) => Exit::Code(c),
                        (None, Phase::PostFailure) => Exit::Failed,
                        (None, _) => Exit::Unknown,
                    },
                    output: owned_at(tool, &["output"]),
                })
            }
            None => ToolAction::Unknown,
        },
        "edit" => ToolAction::Edit(
            files()
                .into_iter()
                .filter_map(|f| {
                    let content = owned_at(f, &["content"]);
                    let created = bool_at(f, &["created"]);
                    let replacement = owned_at(f, &["old"]).zip(owned_at(f, &["new"]));
                    Some(FileEdit {
                        path: owned_at(f, &["path"])?,
                        needs_preimage: !(created && content.is_some()),
                        content,
                        created,
                        replacements: replacement.into_iter().collect(),
                        patch: Vec::new(),
                    })
                })
                .collect(),
        ),
        "read" => ToolAction::Read(
            files()
                .into_iter()
                .filter_map(|f| owned_at(f, &["path"]))
                .collect(),
        ),
        "no_write" => ToolAction::NoWrite,
        _ => ToolAction::Unknown,
    };
    HookEvent::Tool(ToolCall {
        phase,
        tool: name,
        call_id: owned_at(tool, &["call_id"]),
        action,
    })
}
