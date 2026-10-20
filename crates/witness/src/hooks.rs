//! `witness hooks install|uninstall|status`: wire `witness hook` into each
//! agent's hook configuration in the repository.
//!
//! Existing configuration is merged, not replaced: Witness's own entries
//! (recognized by their `witness hook --agent` command) are swapped out and
//! everything else is kept.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::cli::{HooksAction, HooksArgs};
use crate::workspace;

/// One agent's hook configuration.
struct AgentHooks {
    agent: &'static str,
    /// Config file, relative to the repo root.
    file: &'static str,
    format: Format,
    /// `(event key in the config, matcher, event argument for witness hook)`.
    events: &'static [(&'static str, Option<&'static str>, &'static str)],
    note: &'static str,
}

#[derive(Clone, Copy)]
enum Format {
    /// `{"hooks": {"Event": [{"matcher": ..., "hooks": [{"type": "command", "command": ...}]}]}}`
    /// (Claude Code, Codex, Gemini CLI).
    Grouped { timeout: Option<u64> },
    /// `{"version": 1, "hooks": {"event": [{"type": "command", "bash": ..., "powershell": ...}]}}`
    Copilot,
    /// `{"version": 1, "hooks": {"event": [{"command": ...}]}}`
    Cursor,
}

const AGENTS: &[AgentHooks] = &[
    AgentHooks {
        agent: "claude-code",
        file: ".claude/settings.json",
        format: Format::Grouped { timeout: Some(10) },
        events: &[
            (
                "PreToolUse",
                Some("Bash|NotebookEdit|mcp__.*"),
                "pre-tool-use",
            ),
            (
                "PostToolUse",
                Some("Edit|Write|MultiEdit|NotebookEdit|Read|Bash|mcp__.*"),
                "post-tool-use",
            ),
            (
                "PostToolUseFailure",
                Some("Bash|NotebookEdit|mcp__.*"),
                "post-tool-use-failure",
            ),
            ("SessionEnd", None, "session-end"),
        ],
        note: "Or install the Claude Code plugin instead: /plugin install witness@radical-studios",
    },
    AgentHooks {
        agent: "codex",
        file: ".codex/hooks.json",
        format: Format::Grouped { timeout: None },
        events: &[
            ("PreToolUse", Some("Bash|apply_patch"), "pre-tool-use"),
            ("PostToolUse", None, "post-tool-use"),
            ("SessionEnd", None, "session-end"),
        ],
        note: "Codex loads project hooks from .codex/ in trusted projects.",
    },
    AgentHooks {
        agent: "copilot",
        file: ".github/hooks/witness.json",
        format: Format::Copilot,
        events: &[
            ("preToolUse", None, "pre-tool-use"),
            ("postToolUse", None, "post-tool-use"),
            ("postToolUseFailure", None, "post-tool-use-failure"),
            ("sessionEnd", None, "session-end"),
        ],
        note: "Used by Copilot CLI and the Copilot cloud agent. The cloud agent needs \
               `witness` installed by .github/workflows/copilot-setup-steps.yml.",
    },
    AgentHooks {
        agent: "cursor",
        file: ".cursor/hooks.json",
        format: Format::Cursor,
        events: &[
            ("afterShellExecution", None, "after-shell-execution"),
            ("afterFileEdit", None, "after-file-edit"),
        ],
        note: "Cursor support is beta.",
    },
    AgentHooks {
        agent: "gemini-cli",
        file: ".gemini/settings.json",
        format: Format::Grouped { timeout: None },
        events: &[
            (
                "BeforeTool",
                Some("run_shell_command|write_file|replace"),
                "before-tool",
            ),
            ("AfterTool", None, "after-tool"),
            ("SessionEnd", None, "session-end"),
        ],
        note: "Gemini CLI support is beta.",
    },
];

pub fn run(args: HooksArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("finding the working directory")?;
    let root = workspace::find_root(&cwd)
        .ok_or_else(|| anyhow::anyhow!("no .witness/ directory found; run `witness init` first"))?;
    match args.action {
        HooksAction::Status => {
            status(&root);
            Ok(())
        }
        HooksAction::Install { agents, all, force } => {
            for hooks in select(&agents, all)? {
                install(&root, hooks, force)?;
            }
            Ok(())
        }
        HooksAction::Uninstall { agents, all } => {
            for hooks in select(&agents, all)? {
                let path = root.join(hooks.file);
                if remove_entries(&path, hooks)? {
                    println!("witness: removed {} hooks from {}", hooks.agent, hooks.file);
                }
            }
            Ok(())
        }
    }
}

/// Agent names `witness hooks install` accepts.
pub fn agent_names() -> Vec<&'static str> {
    AGENTS.iter().map(|a| a.agent).collect()
}

fn select(names: &[String], all: bool) -> Result<Vec<&'static AgentHooks>> {
    if all {
        return Ok(AGENTS.iter().collect());
    }
    if names.is_empty() {
        bail!(
            "name the agents to set up ({}), or pass --all",
            agent_names().join(", ")
        );
    }
    names
        .iter()
        .map(|name| {
            AGENTS.iter().find(|a| a.agent == name).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown agent {name:?}; expected one of {}",
                    agent_names().join(", ")
                )
            })
        })
        .collect()
}

fn install(root: &Path, hooks: &AgentHooks, force: bool) -> Result<()> {
    let path = root.join(hooks.file);
    if hooks.agent == "claude-code" && plugin_enabled(&path) && !force {
        println!(
            "witness: the witness plugin is enabled in {}; it already installs the Claude Code \
             hooks (pass --force to add them to the settings anyway)",
            hooks.file
        );
        return Ok(());
    }
    let mut config = read_json(&path)?;
    strip_entries(&mut config, hooks);
    add_entries(&mut config, hooks);
    write_json(&path, &config)?;
    println!("witness: wrote {} hooks to {}", hooks.agent, hooks.file);
    println!("         {}", hooks.note);
    Ok(())
}

fn status(root: &Path) {
    let on_path = which("witness");
    println!(
        "witness binary: {}",
        on_path.as_ref().map_or_else(
            || "not on PATH (hooks do nothing until it is installed)".to_owned(),
            |p| p.display().to_string()
        )
    );
    for hooks in AGENTS {
        let path = root.join(hooks.file);
        let text = fs::read_to_string(&path).unwrap_or_default();
        let state = if text.contains(&marker(hooks.agent)) {
            "installed"
        } else if hooks.agent == "claude-code" && plugin_enabled(&path) {
            "via plugin"
        } else {
            "not installed"
        };
        println!("{:<12} {:<14} {}", hooks.agent, state, hooks.file);
    }
}

/// The text that identifies Witness's own entries.
fn marker(agent: &str) -> String {
    format!("hook --agent {agent} ")
}

/// A POSIX shell command that runs `witness hook` if the binary can be
/// found and otherwise does nothing; it always exits 0.
fn sh_command(agent: &str, event: &str) -> String {
    format!(
        r#"W=$(command -v witness || echo "$HOME/.local/bin/witness"); [ -x "$W" ] || W="$HOME/.cargo/bin/witness"; [ -x "$W" ] || exit 0; "$W" hook --agent {agent} {event} || true"#
    )
}

fn powershell_command(agent: &str, event: &str) -> String {
    format!(
        "if (Get-Command witness -ErrorAction SilentlyContinue) {{ witness hook --agent {agent} {event} }}; exit 0"
    )
}

fn add_entries(config: &mut Value, hooks: &AgentHooks) {
    if !config.is_object() {
        *config = json!({});
    }
    let Some(obj) = config.as_object_mut() else {
        return;
    };
    if matches!(hooks.format, Format::Copilot | Format::Cursor) {
        obj.entry("version").or_insert(json!(1));
    }
    let events = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !events.is_object() {
        *events = Value::Object(Map::new());
    }
    let Some(events) = events.as_object_mut() else {
        return;
    };
    for (key, matcher, event) in hooks.events {
        let entry = match hooks.format {
            Format::Grouped { timeout } => {
                let mut command = json!({
                    "type": "command",
                    "command": sh_command(hooks.agent, event),
                });
                if let (Some(t), Some(c)) = (timeout, command.as_object_mut()) {
                    c.insert("timeout".to_owned(), json!(t));
                }
                let mut group = json!({ "hooks": [command] });
                if let (Some(m), Some(g)) = (matcher, group.as_object_mut()) {
                    g.insert("matcher".to_owned(), json!(m));
                }
                group
            }
            Format::Copilot => json!({
                "type": "command",
                "bash": sh_command(hooks.agent, event),
                "powershell": powershell_command(hooks.agent, event),
                "timeoutSec": 10,
            }),
            Format::Cursor => json!({ "command": sh_command(hooks.agent, event) }),
        };
        let list = events
            .entry((*key).to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(list) = list.as_array_mut() {
            list.push(entry);
        } else {
            *list = Value::Array(vec![entry]);
        }
    }
}

/// Removes Witness's entries for this agent; empty event lists go too.
fn strip_entries(config: &mut Value, hooks: &AgentHooks) {
    let marker = marker(hooks.agent);
    let ours = |v: &Value| v.to_string().contains(&marker);
    let Some(events) = config.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for list in events.values_mut() {
        if let Some(items) = list.as_array_mut() {
            items.retain(|item| !ours(item));
        }
    }
    events.retain(|_, list| list.as_array().is_none_or(|l| !l.is_empty()));
}

fn remove_entries(path: &Path, hooks: &AgentHooks) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut config = read_json(path)?;
    let before = config.clone();
    strip_entries(&mut config, hooks);
    if config == before {
        return Ok(false);
    }
    let only_scaffolding = config.as_object().is_some_and(|o| {
        o.iter().all(|(k, v)| match k.as_str() {
            "hooks" => v.as_object().is_some_and(Map::is_empty),
            "version" => true,
            _ => false,
        })
    });
    if matches!(hooks.format, Format::Copilot) && only_scaffolding {
        fs::remove_file(path)?;
    } else {
        write_json(path, &config)?;
    }
    Ok(true)
}

fn plugin_enabled(settings: &Path) -> bool {
    fs::read_to_string(settings)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("enabledPlugins").cloned())
        .and_then(|p| {
            p.as_object().map(|o| {
                o.iter()
                    .any(|(k, v)| k.starts_with("witness@") && v.as_bool() != Some(false))
            })
        })
        .unwrap_or(false)
}

fn read_json(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON; fix or remove it", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)? + "\n";
    fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

fn which(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        [program.to_owned(), format!("{program}.exe")]
            .into_iter()
            .map(|name| dir.join(name))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str) -> &'static AgentHooks {
        AGENTS.iter().find(|a| a.agent == name).unwrap()
    }

    #[test]
    fn merges_into_existing_settings_idempotently() {
        let mut config = json!({
            "permissions": {"allow": ["Bash(ls)"]},
            "hooks": {
                "PostToolUse": [{"matcher": "Write", "hooks": [{"type": "command", "command": "prettier --write"}]}]
            }
        });
        let hooks = agent("claude-code");
        for _ in 0..2 {
            strip_entries(&mut config, hooks);
            add_entries(&mut config, hooks);
        }
        assert_eq!(config["permissions"]["allow"][0], "Bash(ls)");
        let post = config["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2, "{post:#?}");
        assert_eq!(post[0]["hooks"][0]["command"], "prettier --write");
        assert!(
            post[1]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("hook --agent claude-code post-tool-use")
        );
        assert_eq!(post[1]["hooks"][0]["timeout"], 10);
        assert_eq!(
            config["hooks"]["PreToolUse"][0]["matcher"],
            "Bash|NotebookEdit|mcp__.*"
        );

        strip_entries(&mut config, hooks);
        assert_eq!(config["hooks"]["PostToolUse"].as_array().unwrap().len(), 1);
        assert!(config["hooks"].get("PreToolUse").is_none());
    }

    #[test]
    fn copilot_and_cursor_formats() {
        let mut copilot = json!({});
        add_entries(&mut copilot, agent("copilot"));
        assert_eq!(copilot["version"], 1);
        let pre = &copilot["hooks"]["preToolUse"][0];
        assert_eq!(pre["type"], "command");
        assert!(
            pre["bash"]
                .as_str()
                .unwrap()
                .ends_with("pre-tool-use || true")
        );
        assert!(pre["powershell"].as_str().unwrap().contains("exit 0"));

        let mut cursor = json!({"version": 1, "hooks": {"afterFileEdit": [{"command": "fmt.sh"}]}});
        add_entries(&mut cursor, agent("cursor"));
        assert_eq!(
            cursor["hooks"]["afterFileEdit"].as_array().unwrap().len(),
            2
        );
        assert!(
            cursor["hooks"]["afterShellExecution"][0]["command"]
                .as_str()
                .unwrap()
                .contains("--agent cursor after-shell-execution")
        );
    }

    /// The Claude Code plugin ships the same hooks `witness hooks install
    /// claude-code` writes.
    #[test]
    fn plugin_hooks_match_the_installer() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin/claude-code/hooks/hooks.json");
        let plugin: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let mut ours = json!({});
        add_entries(&mut ours, agent("claude-code"));
        // The plugin's SessionEnd hook has no timeout; compare the rest.
        let strip = |mut v: Value| {
            if let Some(end) = v.pointer_mut("/hooks/SessionEnd/0/hooks/0") {
                end.as_object_mut().unwrap().remove("timeout");
            }
            v
        };
        assert_eq!(strip(plugin), strip(ours));
    }

    #[test]
    fn hook_commands_never_fail() {
        let cmd = sh_command("codex", "post-tool-use");
        assert!(cmd.contains("|| exit 0"));
        assert!(cmd.ends_with("|| true"));
    }
}
