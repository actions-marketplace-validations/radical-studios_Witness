//! Agent-neutral conversion of a [`HookInput`] into records.

use std::path::Path;

use crate::command;
use crate::config::is_witness_path;
use crate::record::{Change, EditVia, Event, EventKind};

use super::{
    CaptureContext, Exit, FileEdit, HookEvent, HookInput, Observed, PatchHunk, Phase, ShellCall,
    ToolAction, ToolCall, resolve,
};

/// Converts a hook call into zero or more records. `read_file` receives an
/// absolute path and returns its contents if readable; `observed` holds file
/// changes the caller saw by comparing files before and after the call.
pub fn events_for(
    input: &HookInput,
    ctx: &CaptureContext<'_>,
    read_file: &dyn Fn(&Path) -> Option<String>,
    observed: Option<&Observed>,
) -> Vec<Event> {
    let kinds = match &input.event {
        HookEvent::Tool(call) => tool_events(input, call, ctx, read_file, observed),
        HookEvent::SessionEnd { reason } => vec![EventKind::SessionEnd {
            reason: reason.clone(),
        }],
        HookEvent::SessionStart | HookEvent::Stop { .. } | HookEvent::Ignored(_) => Vec::new(),
    };
    kinds
        .into_iter()
        .map(|kind| {
            let mut event = Event::new(&input.session_id, &input.agent, ctx.now, kind);
            event.turn.clone_from(&input.turn);
            event.subagent.clone_from(&input.subagent);
            event.model.clone_from(&input.model);
            event
        })
        .collect()
}

fn tool_events(
    input: &HookInput,
    call: &ToolCall,
    ctx: &CaptureContext<'_>,
    read_file: &dyn Fn(&Path) -> Option<String>,
    observed: Option<&Observed>,
) -> Vec<EventKind> {
    if call.phase == Phase::Pre {
        return Vec::new();
    }
    let succeeded = call.phase == Phase::Post;
    let mut out = Vec::new();
    match &call.action {
        ToolAction::Shell(shell) => out.push(command_event(ctx, shell)),
        ToolAction::Read(files) if succeeded => {
            for file in files {
                if let Some((rel, _)) = resolve(input, ctx, Some(file))
                    && !is_witness_path(&rel)
                {
                    out.push(EventKind::Read { file: rel });
                }
            }
        }
        // A failed edit changed nothing, unless snapshots say otherwise.
        ToolAction::Edit(files) if succeeded => {
            for file in files {
                if file.needs_preimage && observed.is_some() {
                    continue;
                }
                out.extend(file_edit_events(input, call, ctx, file, read_file));
            }
        }
        _ => {}
    }
    if let Some(observed) = observed {
        out.extend(observed_events(call, observed));
    }
    out
}

fn command_event(ctx: &CaptureContext<'_>, shell: &ShellCall) -> EventKind {
    let verdict = command::classify(ctx.config, &shell.command);
    let exit_code = if shell.background || shell.interrupted || !verdict.exit_attributable {
        None
    } else {
        match shell.exit {
            Exit::Code(code) => Some(code),
            Exit::Failed | Exit::Unknown => None,
        }
    };
    EventKind::Command {
        classification: verdict.class,
        exit_code,
        command: command::sanitize_command(&shell.command),
    }
}

/// Edits seen by comparing files before and after a tool call.
fn observed_events(call: &ToolCall, observed: &Observed) -> Vec<EventKind> {
    observed
        .hunks
        .iter()
        .filter(|h| !is_witness_path(&h.file))
        .map(|h| {
            let change = (!h.binary).then(|| h.change());
            EventKind::Edit {
                file: h.file.clone(),
                lines: change.map(|c| c.new_range()),
                change,
                tool: call.tool.clone(),
                via: Some(observed.via),
            }
        })
        .collect()
}

/// Edits to one file, from what the agent reported, most precise first.
fn file_edit_events(
    input: &HookInput,
    call: &ToolCall,
    ctx: &CaptureContext<'_>,
    file: &FileEdit,
    read_file: &dyn Fn(&Path) -> Option<String>,
) -> Vec<EventKind> {
    let Some((rel, abs)) = resolve(input, ctx, Some(&file.path)) else {
        return Vec::new();
    };
    if is_witness_path(&rel) {
        return Vec::new();
    }
    let make = |change: Option<Change>, via: EditVia| EventKind::Edit {
        file: rel.clone(),
        lines: change.map(|c| c.new_range()),
        change,
        tool: call.tool.clone(),
        via: Some(via),
    };

    // 1. The agent's own patch of the edit.
    let patched: Vec<Change> = file.patch.iter().filter_map(change_from_patch).collect();
    if !patched.is_empty() {
        return patched
            .into_iter()
            .map(|c| make(Some(c), EditVia::Patch))
            .collect();
    }

    // 2. A newly created file covers all its lines.
    if file.created
        && let Some(content) = &file.content
    {
        let added = line_count(content);
        if added > 0 {
            let change = Change {
                start: 1,
                removed: 0,
                added,
            };
            return vec![make(Some(change), EditVia::Content)];
        }
    }

    // 3. Locate the new text in the file as it is now.
    if !file.replacements.is_empty()
        && let Some(content) = read_file(&abs)
    {
        let mut changes = Vec::new();
        for (old, new) in &file.replacements {
            match locate(&content, old, new) {
                Some(found) => changes.extend(found),
                None => return vec![make(None, EditVia::Unknown)],
            }
        }
        // Ranges are in the final file's lines; replay expects them in
        // ascending order.
        changes.sort_by_key(|c| c.start);
        changes.dedup();
        return changes
            .into_iter()
            .map(|c| make(Some(c), EditVia::Locate))
            .collect();
    }

    vec![make(None, EditVia::Unknown)]
}

/// Finds where `new` sits in `content`. Every occurrence is returned: with
/// `replace_all` they were all edited, and otherwise we can't tell which one
/// was. Returns `None` if it can't be found or is empty (a deletion leaves
/// nothing to find).
pub fn locate(content: &str, old: &str, new: &str) -> Option<Vec<Change>> {
    if new.is_empty() {
        return None;
    }
    let added = line_count(new);
    let removed = line_count(old);
    let mut changes = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = content[search_from..].find(new) {
        let offset = search_from + rel;
        let start = u32::try_from(content[..offset].matches('\n').count() + 1).ok()?;
        changes.push(Change {
            start,
            removed,
            added,
        });
        search_from = offset + new.len();
    }
    if changes.is_empty() {
        None
    } else {
        Some(changes)
    }
}

/// Number of lines in a text fragment (a trailing newline doesn't start a
/// new line).
pub fn line_count(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let n = text.matches('\n').count() + usize::from(!text.ends_with('\n'));
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Converts one agent patch hunk to the change it describes, with context
/// lines trimmed.
pub fn change_from_patch(hunk: &PatchHunk) -> Option<Change> {
    let Some(lines) = &hunk.lines else {
        // No line detail: use the header as is.
        return Some(Change {
            start: hunk.new_start,
            removed: hunk.old_lines?,
            added: hunk.new_lines?,
        });
    };
    let (mut old, mut new) = (hunk.old_start, hunk.new_start);
    let mut first: Option<(u32, u32)> = None;
    let mut last_end = (old, new);
    for line in lines {
        match line.chars().next() {
            Some('-') => {
                first.get_or_insert((old, new));
                old += 1;
                last_end = (old, new);
            }
            Some('+') => {
                first.get_or_insert((old, new));
                new += 1;
                last_end = (old, new);
            }
            Some('\\') => {}
            _ => {
                old += 1;
                new += 1;
            }
        }
    }
    let (old_first, new_first) = first?;
    Some(Change {
        start: new_first,
        removed: last_end.0 - old_first,
        added: last_end.1 - new_first,
    })
}
