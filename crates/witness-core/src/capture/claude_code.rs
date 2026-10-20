//! Claude Code hooks: <https://code.claude.com/docs/en/hooks>.
//!
//! `PostToolUse` fires only for tool calls that succeeded, so a `Bash`
//! event there means exit 0; failures arrive as `PostToolUseFailure`, whose
//! `error` starts with `Exit code N`.

use serde_json::Value;

use super::{
    CaptureError, Exit, FileEdit, HookEvent, HookInput, PatchHunk, Phase, ShellCall, ToolAction,
    ToolCall, bool_at, owned_at, parse_exit_code, str_at, text_of,
};

pub const NAME: &str = "claude-code";

/// Tools that can't change files.
const NO_WRITE: &[&str] = &[
    "Grep",
    "Glob",
    "LS",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "Task",
    "Agent",
    "ExitPlanMode",
    "EnterPlanMode",
    "AskUserQuestion",
    "ToolSearch",
    "Skill",
    "SlashCommand",
    "BashOutput",
    "KillShell",
    "KillBash",
    "NotebookRead",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
    "TaskCreate",
    "TaskUpdate",
    "TaskList",
    "TaskGet",
    "TaskStop",
    "Monitor",
    "SendMessage",
];

pub fn parse(v: &Value) -> Result<HookInput, CaptureError> {
    let event = match str_at(v, &["hook_event_name"]).unwrap_or("") {
        "PreToolUse" => tool_event(v, Phase::Pre),
        "PostToolUse" => tool_event(v, Phase::Post),
        "PostToolUseFailure" => tool_event(v, Phase::PostFailure),
        "SessionStart" => HookEvent::SessionStart,
        "SessionEnd" => HookEvent::SessionEnd {
            reason: owned_at(v, &["reason"]).unwrap_or_else(|| "other".to_owned()),
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
        turn: owned_at(v, &["prompt_id"]),
        subagent: owned_at(v, &["agent_type", "agent_id"]),
        model: owned_at(v, &["model"]),
        event,
    })
}

fn tool_event(v: &Value, phase: Phase) -> HookEvent {
    let tool = str_at(v, &["tool_name"]).unwrap_or("").to_owned();
    let Some(input) = v.get("tool_input").filter(|i| i.is_object()) else {
        return HookEvent::Ignored(tool);
    };
    let response = v.get("tool_response").unwrap_or(&Value::Null);
    let action = match tool.as_str() {
        "Bash" => match str_at(input, &["command"]) {
            Some(command) => ToolAction::Shell(ShellCall {
                command: command.to_owned(),
                cwd: None,
                background: bool_at(input, &["run_in_background"]),
                interrupted: bool_at(v, &["is_interrupt"]) || bool_at(response, &["interrupted"]),
                exit: match phase {
                    Phase::Pre => Exit::Unknown,
                    Phase::Post => Exit::Code(0),
                    Phase::PostFailure => str_at(v, &["error"])
                        .and_then(parse_exit_code)
                        .map_or(Exit::Failed, Exit::Code),
                },
                output: match phase {
                    Phase::PostFailure => owned_at(v, &["error"]),
                    _ => text_of(response),
                },
            }),
            None => ToolAction::Unknown,
        },
        "Edit" | "Write" | "MultiEdit" => file_edit(&tool, input, response),
        "NotebookEdit" => match str_at(input, &["notebook_path"]) {
            Some(path) => ToolAction::Edit(vec![FileEdit {
                path: path.to_owned(),
                needs_preimage: true,
                ..FileEdit::default()
            }]),
            None => ToolAction::Unknown,
        },
        "Read" => match str_at(input, &["file_path"]) {
            Some(path) => ToolAction::Read(vec![path.to_owned()]),
            None => ToolAction::NoWrite,
        },
        t if NO_WRITE.contains(&t) => ToolAction::NoWrite,
        _ => ToolAction::Unknown,
    };
    HookEvent::Tool(ToolCall {
        phase,
        tool,
        call_id: owned_at(v, &["tool_use_id"]),
        action,
    })
}

fn file_edit(tool: &str, input: &Value, response: &Value) -> ToolAction {
    let Some(path) = str_at(input, &["file_path"]) else {
        return ToolAction::Unknown;
    };
    let pair = |e: &Value| -> Option<(String, String)> {
        Some((
            str_at(e, &["old_string"])?.to_owned(),
            str_at(e, &["new_string"])?.to_owned(),
        ))
    };
    let replacements = if tool == "MultiEdit" {
        input
            .get("edits")
            .and_then(Value::as_array)
            .map(|edits| edits.iter().filter_map(pair).collect())
            .unwrap_or_default()
    } else {
        pair(input).into_iter().collect()
    };
    let patch = response
        .get("structuredPatch")
        .and_then(Value::as_array)
        .map(|hunks| hunks.iter().filter_map(patch_hunk).collect())
        .unwrap_or_default();
    ToolAction::Edit(vec![FileEdit {
        path: path.to_owned(),
        patch,
        replacements,
        content: (tool == "Write")
            .then(|| str_at(input, &["content"]).map(str::to_owned))
            .flatten(),
        created: str_at(response, &["type"]) == Some("create"),
        // Claude Code reports a patch of every edit.
        needs_preimage: false,
    }])
}

fn patch_hunk(h: &Value) -> Option<PatchHunk> {
    let num = |k: &str| {
        h.get(k)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    Some(PatchHunk {
        old_start: num("oldStart")?,
        new_start: num("newStart")?,
        old_lines: num("oldLines"),
        new_lines: num("newLines"),
        lines: h.get("lines").and_then(Value::as_array).map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        }),
    })
}
