//! Cursor agent hooks (`.cursor/hooks.json`). Beta: payload shapes are taken
//! from Cursor's documentation and are read leniently.
//!
//! Witness uses the after-hooks only (`afterShellExecution`,
//! `afterFileEdit`), plus `stop`. Cursor's before-hooks expect a permission
//! decision, which Witness has no business making, so shell commands are
//! compared against the session's last working-tree snapshot instead of a
//! pre-command one.

use serde_json::Value;

use super::{
    CaptureError, Exit, FileEdit, HookEvent, HookInput, Phase, ShellCall, ToolAction, ToolCall,
    owned_at, str_at, text_of,
};

pub const NAME: &str = "cursor";

pub fn parse(v: &Value) -> Result<HookInput, CaptureError> {
    let event = match str_at(v, &["hook_event_name"]).unwrap_or("") {
        "afterShellExecution" => shell(v, Phase::Post),
        "afterFileEdit" => file_edit(v),
        "sessionStart" => HookEvent::SessionStart,
        "sessionEnd" => HookEvent::SessionEnd {
            reason: owned_at(v, &["reason"]).unwrap_or_else(|| "other".to_owned()),
        },
        "stop" => HookEvent::Stop {
            hook_active: v
                .get("loop_count")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > 0),
            last_message: None,
        },
        other => HookEvent::Ignored(other.to_owned()),
    };
    let cwd = owned_at(v, &["cwd"]).or_else(|| {
        v.get("workspace_roots")
            .and_then(Value::as_array)
            .and_then(|roots| roots.first())
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    Ok(HookInput {
        agent: NAME.to_owned(),
        session_id: owned_at(v, &["conversation_id", "session_id"]).unwrap_or_default(),
        cwd,
        turn: owned_at(v, &["generation_id"]),
        subagent: None,
        model: owned_at(v, &["model"]),
        event,
    })
}

fn shell(v: &Value, phase: Phase) -> HookEvent {
    let Some(command) = owned_at(v, &["command"]) else {
        return HookEvent::Ignored("afterShellExecution".to_owned());
    };
    let exit = v
        .get("exit_code")
        .or_else(|| v.get("exitCode"))
        .and_then(Value::as_i64)
        .and_then(|c| i32::try_from(c).ok())
        .map_or(Exit::Unknown, Exit::Code);
    HookEvent::Tool(ToolCall {
        phase,
        tool: "shell".to_owned(),
        call_id: None,
        action: ToolAction::Shell(ShellCall {
            command,
            cwd: owned_at(v, &["cwd"]),
            background: false,
            interrupted: false,
            exit,
            output: v.get("output").and_then(text_of),
        }),
    })
}

fn file_edit(v: &Value) -> HookEvent {
    let Some(path) = owned_at(v, &["file_path"]) else {
        return HookEvent::Ignored("afterFileEdit".to_owned());
    };
    let replacements = v
        .get("edits")
        .and_then(Value::as_array)
        .map(|edits| {
            edits
                .iter()
                .filter_map(|e| {
                    Some((
                        str_at(e, &["old_string"])?.to_owned(),
                        str_at(e, &["new_string"])?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    HookEvent::Tool(ToolCall {
        phase: Phase::Post,
        tool: "edit".to_owned(),
        call_id: None,
        action: ToolAction::Edit(vec![FileEdit {
            path,
            replacements,
            // No before-hook: the edit is located in the file afterwards.
            needs_preimage: false,
            ..FileEdit::default()
        }]),
    })
}
