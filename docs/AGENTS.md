# Agent support

Witness records sessions from any coding agent that can run a command on
its tool calls (a "hook"), and from any agent or script through
`witness exec` and `witness record`. `witness init` installs hooks for the
agents it detects: their config directory or instruction file in the
repository, or their CLI on `PATH` (`--agents` chooses them, `--no-hooks`
skips this). Afterwards, manage them with:

```bash
witness hooks install claude-code codex copilot   # or: --all
witness hooks status
witness doctor                                     # is everything recording?
```

The hooks run `witness hook --agent <agent> <event>`. They do nothing when
the `witness` binary is missing or the repository has no `.witness/`, and
they always exit 0, so they never block the agent.

`witness hooks install` also adds a short Witness section (between
`<!-- witness:begin -->` and `<!-- witness:end -->`) to the instruction file
the agent reads: `AGENTS.md` for Codex, Copilot and Cursor, `GEMINI.md` for
Gemini CLI, `CLAUDE.md` for Claude Code without the plugin. It asks the
agent to run the checks that cover its edits, use `witness exec` for piped
commands, record assumptions and run `witness status` before finishing.
Pass `--no-guidance` to skip it; `witness hooks uninstall` removes it once
no installed agent reads that file.

## Support matrix

| Agent | Config written | Edits | Shell commands | Exit codes | Status |
| --- | --- | --- | --- | --- | --- |
| Claude Code | plugin, or `.claude/settings.json` | agent's own patch | snapshots | exact | stable |
| Codex CLI | `.codex/hooks.json` | `apply_patch`: pre-images | snapshots | from output (`Exit code: N`) | stable |
| GitHub Copilot (CLI, cloud agent) | `.github/hooks/witness.json` | pre-images | snapshots | from output (`<exited with exit code N>`), else unknown | stable |
| Cursor | `.cursor/hooks.json` | located from old/new text | compared with the last snapshot | reported, else unknown | beta |
| Gemini CLI | `.gemini/settings.json` | pre-images | snapshots | from output (`Exit Code: N`) | beta |
| Anything else | your wrapper | `witness record edit` or snapshots | `witness exec` | exact | stable |

"Unknown" exit codes never count as a passing check. Beta adapters were
built from each agent's documentation; please report payloads they misread.

### How changes are observed

- **Agent patches.** Claude Code reports a patch of every `Edit`, `Write`
  and `MultiEdit`; Witness uses it as is.
- **Pre-images.** For edit tools without a reliable patch (Copilot's `edit`
  and `create`, Codex's `apply_patch`, Gemini's `replace`), the pre-tool
  hook copies the target files to `.witness/state/pre/` and the post-tool
  hook diffs them with the result. The copies are deleted right after.
- **Snapshots.** Around shell commands and unknown tools, Witness records
  the working tree in a tree object through a private git index
  (`.witness/state/index`) and diffs the two trees. That catches files
  changed by `sed -i`, formatters, code generators and scripts. Read-only
  commands (`ls`, `cat`, `rg`, `git status`, ...) and background commands
  are skipped. A snapshot slower than `[capture] snapshot_budget_ms`
  (default 500 ms) turns snapshots off for that session and leaves a
  `note` in the log; `[capture] snapshots = false` turns them off entirely.
  Git commands that replace the working tree with other content
  (`checkout`, `switch`, `reset`, `stash`, `pull`, `merge`, `rebase`,
  `restore`, ...) are not snapshotted: what they change isn't the agent's
  edit.

Only paths, line ranges, 40-bit line hashes and redacted command text are
stored; file contents, tool output and environment variables never are. See
[RECORD_FORMAT.md](RECORD_FORMAT.md).

## End-of-turn nudges

When a turn ends, Witness looks at the sections this session wrote on the
branch (including uncommitted work). If any are failing or were never
checked, or the agent's last message claims "all tests pass" or "it builds"
when the record disagrees, `[nudge] mode` in `witness.toml` decides what
happens:

- `remind` (default): the user sees a one-line note, and the agent gets the
  list with the next prompt (or when the session resumes). The same list is
  passed to the agent only once.
- `block`: the agent is asked to continue and either check the sections or
  say why it can't. Witness blocks at most once in a row; if the agent
  stops again right away, it falls back to a reminder.
- `off`: nothing is shown. Claims are still recorded for the report.

| Agent | End of turn | Note for the user | Context for the agent | Block |
| --- | --- | --- | --- | --- |
| Claude Code | `Stop` | yes | `UserPromptSubmit`, `SessionStart` | yes |
| Codex CLI | `Stop` | yes | `UserPromptSubmit`, `SessionStart` | yes |
| GitHub Copilot | `agentStop` | no | no | yes |
| Cursor | `stop` | no | no | yes (`followup_message`) |
| Gemini CLI | `AfterAgent` | yes | `BeforeAgent`, `SessionStart` | yes |

A contradicted claim is also listed in the report, under the headline.

## Per-agent notes

### Claude Code

Install the plugin (`/plugin install witness@radical-studios`), which
ships the hooks, a skill that teaches the agent to verify its edits and
record assumptions, and two commands: `/witness:status` (what is failing or
unchecked right now) and `/witness:review` (review the branch following
Witness's plan). `witness init` (or `witness hooks install claude-code`)
also adds the hooks to the project's `.claude/settings.json`, so
teammates without the plugin are recorded too. Having both is fine: each
tool call is recorded once. The installer skips the settings when the
plugin is enabled in that file.

### Codex CLI

`witness hooks install codex` writes `.codex/hooks.json`. Codex loads
project hooks in trusted projects. Codex's `PostToolUse` fires for every
shell command whatever its exit status, and the status is read from the
output text.

### GitHub Copilot

`witness hooks install copilot` writes `.github/hooks/witness.json`, which
both Copilot CLI and the Copilot cloud agent read. The cloud agent runs in a
fresh sandbox, so `witness` must be installed there by
`.github/workflows/copilot-setup-steps.yml`, for example:

```yaml
- name: Install Witness
  run: cargo install --git https://github.com/radical-studios/witness witness --locked
```

Copilot reports no structured exit code; Witness reads the
`<exited with exit code N>` marker from the output and otherwise treats the
result as unknown.

### Cursor (beta)

`witness hooks install cursor` writes `.cursor/hooks.json` with
`afterShellExecution`, `afterFileEdit` and `stop`. Cursor's before-hooks expect a
permission decision, which Witness doesn't make, so shell commands are
compared with the session's previous snapshot.

### Gemini CLI (beta)

`witness hooks install gemini-cli` adds `BeforeTool`, `AfterTool`,
`BeforeAgent`, `AfterAgent`, `SessionStart` and `SessionEnd` hooks to
`.gemini/settings.json`.

## Agents without hooks

### `witness exec`

```bash
witness exec -- cargo test            # records the exact exit code
witness exec -- cargo test | tail -20 # still exact: exec sees cargo's code
witness exec "make test && make lint" # one argument runs through sh -c
```

`witness exec` passes the command's output through unchanged, exits with
its code, and records a `command` event plus any files it changed
(snapshots). It records into `--session`, `$WITNESS_SESSION_ID`, or the
session the hooks saw last. Outside a repository with `.witness/` it just
runs the command. The shell hook ignores the `witness exec` wrapper itself,
so nothing is counted twice.

### `witness record`

```bash
witness record --agent aider edit --file src/x.rs --lines 10-24
witness record read --file src/y.rs
witness record command --exit-code 0 -- cargo test -p core
```

### The generic hook format

Wrapper scripts can also pipe Witness's own JSON format into
`witness hook --agent generic <event>`:

```json
{"witness": 1, "agent": "my-agent", "event": "post_tool",
 "session_id": "abc", "cwd": "/repo", "turn": "t1",
 "tool": {"name": "shell", "kind": "shell", "call_id": "c1",
          "command": "cargo test", "exit_code": 0, "output": "..."}}
```

- `event`: `pre_tool`, `post_tool`, `post_tool_failure`, `session_start`,
  `prompt`, `session_end` (with `reason`) or `stop` (with `last_message`
  and `stop_hook_active`). `stop` and `prompt` may print a JSON reply on
  stdout (`systemMessage`, or `decision: "block"` with a `reason`).
- `tool.kind`: `shell` (`command`, `exit_code`, `output`, `background`),
  `edit` (`files: [{path, content?, created?, old?, new?}]`), `read`
  (`files: [{path}]`), `no_write` or `unknown`.
- Send `pre_tool` before an `edit` or `unknown` tool runs and `post_tool`
  after, with the same `call_id`, and Witness diffs the files itself.
