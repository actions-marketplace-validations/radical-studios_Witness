# Agent support

Witness records sessions from any coding agent that can run a command on
its tool calls (a "hook"), and from any agent or script through
`witness exec` and `witness record`. Install the hooks for the agents your
team uses with:

```bash
witness hooks install claude-code codex copilot   # or: --all
witness hooks status
```

The hooks run `witness hook --agent <agent> <event>`. They do nothing when
the `witness` binary is missing or the repository has no `.witness/`, and
they always exit 0, so they never block the agent.

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

## Per-agent notes

### Claude Code

Install the plugin (`/plugin install witness@radical-studios`), which
ships the hooks and a skill that teaches the agent to record assumptions,
or run `witness hooks install claude-code` to add the hooks to
`.claude/settings.json`. Don't do both: events would be recorded twice.
The installer refuses when the plugin is enabled in that file.

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
`afterShellExecution` and `afterFileEdit`. Cursor's before-hooks expect a
permission decision, which Witness doesn't make, so shell commands are
compared with the session's previous snapshot.

### Gemini CLI (beta)

`witness hooks install gemini-cli` adds `BeforeTool`, `AfterTool` and
`SessionEnd` hooks to `.gemini/settings.json`.

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
  `session_end` (with `reason`) or `stop`.
- `tool.kind`: `shell` (`command`, `exit_code`, `output`, `background`),
  `edit` (`files: [{path, content?, created?, old?, new?}]`), `read`
  (`files: [{path}]`), `no_write` or `unknown`.
- Send `pre_tool` before an `edit` or `unknown` tool runs and `post_tool`
  after, with the same `call_id`, and Witness diffs the files itself.
