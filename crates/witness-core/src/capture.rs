//! Turns Claude Code hook payloads into session log records.
//!
//! Pure: the caller supplies the clock, the repo root and a function that
//! reads a file (used only when a line range has to be located).

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;

use crate::command;
use crate::config::Config;
use crate::record::{AGENT_CLAUDE_CODE, Change, Event, EventKind};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("hook payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("hook payload has an empty session_id")]
    MissingSession,
}

/// The fields Witness reads from a hook payload. Everything else (including
/// tool output and file contents beyond what is needed for line ranges) is
/// ignored and never stored.
#[derive(Debug, Clone, Deserialize)]
pub struct HookPayload {
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    pub hook_event_name: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<Value>,
    #[serde(default)]
    pub tool_response: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub is_interrupt: Option<bool>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl HookPayload {
    pub fn parse(json: &str) -> Result<Self, CaptureError> {
        let payload: HookPayload = serde_json::from_str(json)?;
        if payload.session_id.trim().is_empty() {
            return Err(CaptureError::MissingSession);
        }
        Ok(payload)
    }
}

/// Context for converting one payload.
pub struct CaptureContext<'a> {
    pub config: &'a Config,
    /// Absolute repo root (the directory containing `.witness/`).
    pub root: &'a Path,
    pub now: OffsetDateTime,
}

/// Converts a payload into zero or more records. `read_file` receives an
/// absolute path and returns its contents if readable.
pub fn events_from_payload(
    payload: &HookPayload,
    ctx: &CaptureContext<'_>,
    read_file: &dyn Fn(&Path) -> Option<String>,
) -> Vec<Event> {
    let kinds = match payload.hook_event_name.as_str() {
        "PostToolUse" => tool_events(payload, ctx, read_file, true),
        "PostToolUseFailure" => tool_events(payload, ctx, read_file, false),
        "SessionEnd" => vec![EventKind::SessionEnd {
            reason: payload.reason.clone().unwrap_or_else(|| "other".to_owned()),
        }],
        _ => Vec::new(),
    };
    kinds
        .into_iter()
        .map(|kind| Event::new(&payload.session_id, AGENT_CLAUDE_CODE, ctx.now, kind))
        .collect()
}

fn tool_events(
    payload: &HookPayload,
    ctx: &CaptureContext<'_>,
    read_file: &dyn Fn(&Path) -> Option<String>,
    succeeded: bool,
) -> Vec<EventKind> {
    let (Some(tool), Some(input)) = (payload.tool_name.as_deref(), payload.tool_input.as_ref())
    else {
        return Vec::new();
    };
    let response = payload.tool_response.as_ref();
    match tool {
        "Bash" => bash_event(payload, ctx, input, response, succeeded)
            .into_iter()
            .collect(),
        // A failed edit or read changed or showed nothing.
        _ if !succeeded => Vec::new(),
        "Read" => {
            let Some(file) = resolve(payload, ctx, str_field(input, "file_path")) else {
                return Vec::new();
            };
            vec![EventKind::Read { file: file.0 }]
        }
        "Edit" | "Write" | "MultiEdit" => {
            edit_events(payload, ctx, tool, input, response, read_file)
        }
        "NotebookEdit" => {
            let Some(file) = resolve(payload, ctx, str_field(input, "notebook_path")) else {
                return Vec::new();
            };
            vec![EventKind::Edit {
                file: file.0,
                lines: None,
                change: None,
                tool: tool.to_owned(),
            }]
        }
        _ => Vec::new(),
    }
}

fn bash_event(
    payload: &HookPayload,
    ctx: &CaptureContext<'_>,
    input: &Value,
    response: Option<&Value>,
    succeeded: bool,
) -> Option<EventKind> {
    let text = str_field(input, "command")?;
    let verdict = command::classify(ctx.config, text);
    let background = input
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let interrupted = payload.is_interrupt.unwrap_or(false)
        || response
            .and_then(|r| r.get("interrupted"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let exit_code = if background || interrupted || !verdict.exit_attributable {
        None
    } else if succeeded {
        Some(0)
    } else {
        payload.error.as_deref().and_then(parse_exit_code)
    };
    Some(EventKind::Command {
        classification: verdict.class,
        exit_code,
        command: command::sanitize_command(text),
    })
}

/// Reads `Exit code N` from the first line of a `PostToolUseFailure` error.
pub fn parse_exit_code(error: &str) -> Option<i32> {
    let first = error.lines().next()?.trim();
    first.strip_prefix("Exit code ")?.trim().parse().ok()
}

fn edit_events(
    payload: &HookPayload,
    ctx: &CaptureContext<'_>,
    tool: &str,
    input: &Value,
    response: Option<&Value>,
    read_file: &dyn Fn(&Path) -> Option<String>,
) -> Vec<EventKind> {
    let Some((file, abs)) = resolve(payload, ctx, str_field(input, "file_path")) else {
        return Vec::new();
    };
    let make = |change: Option<Change>| EventKind::Edit {
        file: file.clone(),
        lines: change.map(|c| c.new_range()),
        change,
        tool: tool.to_owned(),
    };

    // 1. Claude Code's own patch of the edit.
    let patched = response
        .and_then(|r| r.get("structuredPatch"))
        .and_then(Value::as_array)
        .map(|hunks| {
            hunks
                .iter()
                .filter_map(change_from_patch_hunk)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !patched.is_empty() {
        return patched.into_iter().map(|c| make(Some(c))).collect();
    }

    // 2. A newly created file covers all its lines.
    if tool == "Write" {
        let created = response
            .and_then(|r| r.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|t| t == "create");
        if let (true, Some(content)) = (created, str_field(input, "content")) {
            let added = line_count(content);
            if added > 0 {
                return vec![make(Some(Change {
                    start: 1,
                    removed: 0,
                    added,
                }))];
            }
        }
        return vec![make(None)];
    }

    // 3. Locate the new text in the file as it is now.
    let pairs: Vec<(&str, &str)> = if tool == "MultiEdit" {
        input
            .get("edits")
            .and_then(Value::as_array)
            .map(|edits| edits.iter().filter_map(edit_pair).collect())
            .unwrap_or_default()
    } else {
        edit_pair(input).into_iter().collect()
    };
    let content = if pairs.is_empty() {
        None
    } else {
        read_file(&abs)
    };
    let mut changes = Vec::new();
    if let Some(content) = content {
        for (old, new) in pairs {
            match locate(&content, old, new) {
                Some(found) => changes.extend(found),
                None => return vec![make(None)],
            }
        }
    }
    if changes.is_empty() {
        return vec![make(None)];
    }
    changes.into_iter().map(|c| make(Some(c))).collect()
}

fn edit_pair(v: &Value) -> Option<(&str, &str)> {
    Some((str_field(v, "old_string")?, str_field(v, "new_string")?))
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

/// Converts one `structuredPatch` hunk to the change it describes, with
/// context lines trimmed.
pub fn change_from_patch_hunk(hunk: &Value) -> Option<Change> {
    let as_u32 = |k: &str| {
        hunk.get(k)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
    };
    let old_start = as_u32("oldStart")?;
    let new_start = as_u32("newStart")?;
    let lines = hunk.get("lines").and_then(Value::as_array);
    let Some(lines) = lines else {
        // No line detail: use the header as is.
        let old_lines = as_u32("oldLines")?;
        let new_lines = as_u32("newLines")?;
        return Some(Change {
            start: new_start,
            removed: old_lines,
            added: new_lines,
        });
    };
    let (mut old, mut new) = (old_start, new_start);
    let mut first: Option<(u32, u32)> = None;
    let mut last_end = (old, new);
    for line in lines.iter().filter_map(Value::as_str) {
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

fn str_field<'v>(v: &'v Value, key: &str) -> Option<&'v str> {
    v.get(key).and_then(Value::as_str)
}

/// Resolves a tool's file path to (repo-relative `/` path, absolute path).
/// Returns `None` for paths outside the repo root.
fn resolve(
    payload: &HookPayload,
    ctx: &CaptureContext<'_>,
    path: Option<&str>,
) -> Option<(String, PathBuf)> {
    let path = Path::new(path?);
    let abs = if path.is_absolute() {
        normalize(path)
    } else {
        let base = payload.cwd.as_deref().map_or(ctx.root, Path::new);
        normalize(&base.join(path))
    };
    let rel = relative_to(&abs, &normalize(ctx.root))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{CommandClass, LineRange};
    use time::macros::datetime;

    const ROOT: &str = "/home/user/my-project";

    fn capture(json: &str, files: &[(&str, &str)]) -> Vec<EventKind> {
        let config = Config::default();
        let ctx = CaptureContext {
            config: &config,
            root: Path::new(ROOT),
            now: datetime!(2026-10-20 03:37:00 UTC),
        };
        let files: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(p, c)| (PathBuf::from(p), (*c).to_owned()))
            .collect();
        let read = move |p: &Path| files.iter().find(|(fp, _)| fp == p).map(|(_, c)| c.clone());
        let payload = HookPayload::parse(json).unwrap();
        events_from_payload(&payload, &ctx, &read)
            .into_iter()
            .map(|e| {
                assert_eq!(e.session_id, payload.session_id);
                assert_eq!(e.agent, "claude-code");
                e.kind
            })
            .collect()
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/hooks")
            .join(name);
        std::fs::read_to_string(&path).unwrap()
    }

    /// Payloads captured from a real Claude Code 2.x session, with paths and
    /// ids anonymized.
    #[test]
    fn real_claude_code_payloads() {
        let real = |name: &str| capture(&fixture(&format!("claude-code-v2/{name}.json")), &[]);
        assert_eq!(
            real("post_tool_use_read"),
            vec![EventKind::Read {
                file: "main.rs".into()
            }]
        );
        assert_eq!(
            real("post_tool_use_edit"),
            vec![EventKind::Edit {
                file: "main.rs".into(),
                lines: Some(LineRange::new(2, 2)),
                change: Some(Change {
                    start: 2,
                    removed: 1,
                    added: 1
                }),
                tool: "Edit".into(),
            }]
        );
        assert_eq!(
            real("post_tool_use_write"),
            vec![EventKind::Edit {
                file: "notes.txt".into(),
                lines: Some(LineRange::new(1, 2)),
                change: Some(Change {
                    start: 1,
                    removed: 0,
                    added: 2
                }),
                tool: "Write".into(),
            }]
        );
        assert_eq!(
            real("post_tool_use_bash"),
            vec![EventKind::Command {
                classification: CommandClass::Other,
                exit_code: Some(0),
                command: "echo hi && exit 0".into(),
            }]
        );
        assert_eq!(
            real("post_tool_use_failure_bash"),
            vec![EventKind::Command {
                classification: CommandClass::Other,
                exit_code: Some(1),
                command: "false".into(),
            }]
        );
        assert_eq!(
            real("session_end"),
            vec![EventKind::SessionEnd {
                reason: "other".into()
            }]
        );
    }

    #[test]
    fn bash_success_is_exit_zero() {
        let kinds = capture(&fixture("post_tool_use_bash.json"), &[]);
        assert_eq!(
            kinds,
            vec![EventKind::Command {
                classification: CommandClass::Test,
                exit_code: Some(0),
                command: "npm test".into(),
            }]
        );
    }

    #[test]
    fn bash_failure_reads_exit_code() {
        let kinds = capture(&fixture("post_tool_use_failure_bash.json"), &[]);
        assert_eq!(
            kinds,
            vec![EventKind::Command {
                classification: CommandClass::Test,
                exit_code: Some(1),
                command: "npm test".into(),
            }]
        );
    }

    #[test]
    fn bash_background_and_piped_have_no_exit_code() {
        let kinds = capture(&fixture("post_tool_use_bash_background.json"), &[]);
        assert!(matches!(
            kinds[0],
            EventKind::Command {
                exit_code: None,
                ..
            }
        ));
        let piped =
            fixture("post_tool_use_bash.json").replace("npm test", "cargo test 2>&1 | tail -5");
        let kinds = capture(&piped, &[]);
        assert!(matches!(
            kinds[0],
            EventKind::Command {
                classification: CommandClass::Test,
                exit_code: None,
                ..
            }
        ));
    }

    #[test]
    fn bash_command_text_is_redacted() {
        let json = fixture("post_tool_use_bash.json").replace(
            "npm test",
            "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz012345 npm test",
        );
        let kinds = capture(&json, &[]);
        assert!(matches!(
            &kinds[0],
            EventKind::Command { command, .. } if command == "GITHUB_TOKEN=[REDACTED] npm test"
        ));
    }

    #[test]
    fn write_create_covers_whole_new_file() {
        let kinds = capture(&fixture("post_tool_use_write.json"), &[]);
        assert_eq!(
            kinds,
            vec![EventKind::Edit {
                file: "src/greeting.rs".into(),
                lines: Some(LineRange::new(1, 3)),
                change: Some(Change {
                    start: 1,
                    removed: 0,
                    added: 3
                }),
                tool: "Write".into(),
            }]
        );
    }

    #[test]
    fn edit_uses_structured_patch_without_context() {
        let kinds = capture(&fixture("post_tool_use_edit.json"), &[]);
        assert_eq!(
            kinds,
            vec![EventKind::Edit {
                file: "src/lib.rs".into(),
                lines: Some(LineRange::new(12, 13)),
                change: Some(Change {
                    start: 12,
                    removed: 1,
                    added: 2
                }),
                tool: "Edit".into(),
            }]
        );
    }

    #[test]
    fn edit_without_patch_locates_new_string_in_file() {
        let json = fixture("post_tool_use_edit.json");
        let mut v: Value = serde_json::from_str(&json).unwrap();
        v["tool_response"] = serde_json::json!({ "filePath": "/home/user/my-project/src/lib.rs" });
        let mut content = String::new();
        for i in 1..=11 {
            content.push_str(&format!("line {i}\n"));
        }
        content.push_str("    let total = items.iter().sum::<u32>();\n    total * 2\n}\n");
        let kinds = capture(
            &v.to_string(),
            &[("/home/user/my-project/src/lib.rs", content.as_str())],
        );
        assert_eq!(
            kinds,
            vec![EventKind::Edit {
                file: "src/lib.rs".into(),
                lines: Some(LineRange::new(12, 13)),
                change: Some(Change {
                    start: 12,
                    removed: 1,
                    added: 2
                }),
                tool: "Edit".into(),
            }]
        );
    }

    #[test]
    fn edit_that_cannot_be_located_covers_whole_file() {
        let json = fixture("post_tool_use_edit.json");
        let mut v: Value = serde_json::from_str(&json).unwrap();
        v["tool_response"] = Value::Null;
        let kinds = capture(&v.to_string(), &[]);
        assert!(matches!(
            &kinds[0],
            EventKind::Edit {
                lines: None,
                change: None,
                ..
            }
        ));
    }

    #[test]
    fn read_records_relative_path() {
        let kinds = capture(&fixture("post_tool_use_read.json"), &[]);
        assert_eq!(
            kinds,
            vec![EventKind::Read {
                file: "src/lib.rs".into()
            }]
        );
    }

    #[test]
    fn files_outside_repo_are_ignored() {
        let json = fixture("post_tool_use_read.json")
            .replace("/home/user/my-project/src/lib.rs", "/etc/hosts");
        assert!(capture(&json, &[]).is_empty());
        let json = fixture("post_tool_use_read.json").replace(
            "/home/user/my-project/src/lib.rs",
            "/home/user/my-project/../other/x",
        );
        assert!(capture(&json, &[]).is_empty());
    }

    #[test]
    fn session_end_records_reason() {
        let kinds = capture(&fixture("session_end.json"), &[]);
        assert_eq!(
            kinds,
            vec![EventKind::SessionEnd {
                reason: "other".into()
            }]
        );
    }

    #[test]
    fn unrelated_events_and_tools_produce_nothing() {
        let pre = fixture("post_tool_use_bash.json").replace("\"PostToolUse\"", "\"PreToolUse\"");
        assert!(capture(&pre, &[]).is_empty());
        let glob = fixture("post_tool_use_read.json").replace("\"Read\"", "\"Glob\"");
        assert!(capture(&glob, &[]).is_empty());
    }

    #[test]
    fn rejects_bad_payloads() {
        assert!(HookPayload::parse("{").is_err());
        assert!(HookPayload::parse(r#"{"session_id":"","hook_event_name":"Stop"}"#).is_err());
    }

    #[test]
    fn parses_exit_code_line() {
        assert_eq!(
            parse_exit_code("Exit code 101\nerror: test failed"),
            Some(101)
        );
        assert_eq!(parse_exit_code("Command timed out"), None);
    }

    #[test]
    fn patch_hunk_trims_context_and_handles_deletions() {
        let hunk = serde_json::json!({
            "oldStart": 5, "oldLines": 7, "newStart": 5, "newLines": 5,
            "lines": [" a", " b", " c", "-d", "-e", " f", " g"]
        });
        assert_eq!(
            change_from_patch_hunk(&hunk),
            Some(Change {
                start: 8,
                removed: 2,
                added: 0
            })
        );
        let only_context = serde_json::json!({"oldStart": 1, "newStart": 1, "lines": [" a"]});
        assert_eq!(change_from_patch_hunk(&only_context), None);
    }

    #[test]
    fn locate_finds_every_occurrence() {
        let content = "a\nfoo\nb\nfoo\n";
        let found = locate(content, "bar", "foo").unwrap();
        assert_eq!(
            found.iter().map(|c| c.start).collect::<Vec<_>>(),
            vec![2, 4]
        );
        assert!(locate(content, "x", "").is_none());
        assert!(locate(content, "x", "zzz").is_none());
    }

    #[test]
    fn counts_lines() {
        assert_eq!(line_count(""), 0);
        assert_eq!(line_count("a"), 1);
        assert_eq!(line_count("a\n"), 1);
        assert_eq!(line_count("a\nb"), 2);
    }
}
