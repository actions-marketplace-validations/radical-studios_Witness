# Witness record format

Status: **schema_version 1**. This document is the contract between capture
(`witness hook`, `witness assume`) and the core (`witness report`). Code in
`crates/witness-core/src/record.rs` implements it.

## Location

```
<repo root>/
├── witness.toml
└── .witness/
    ├── .gitignore            # ignores state/
    ├── sessions/
    │   └── <session-id>.jsonl   # committed with the code
    └── state/                   # local only, never committed
        ├── current-session      # most recent session and agent, for `witness assume`
        ├── hook.log             # hook errors (and timings with WITNESS_DEBUG=1)
        ├── index                # private git index for working-tree snapshots
        ├── pre/                 # snapshot ids and file copies between pre- and post-tool hooks
        ├── last/                # each session's latest snapshot (agents without pre-tool hooks)
        └── snapshots-off        # session whose snapshots exceeded the time budget
```

`<session-id>` is the agent's session id with every character other than
`[A-Za-z0-9_-]` replaced by `_`.

The repo root is the nearest ancestor of the hook's `cwd` that contains a
`.witness/` directory. If there is none, capture does nothing: installing the
plugin globally never writes into repositories that have not run
`witness init`. The root is usually the git top level; `witness init --here`
places it in a subdirectory, and paths in the log are then relative to that
subdirectory.

## Lines

Each file is append-only [JSON Lines](https://jsonlines.org/): one UTF-8 JSON
object per line, `\n` terminated. Writers append whole lines with a single
`write` on a file opened with `O_APPEND`, so concurrent hooks don't interleave.
Readers skip lines they can't parse (and report how many), so a truncated last
line never breaks a report.

### Common fields

| Field            | Type    | Notes |
| ---------------- | ------- | ----- |
| `schema_version` | integer | `1`. Readers ignore lines with a higher major version. |
| `session_id`     | string  | Agent session id, unmodified. |
| `timestamp`      | string  | RFC 3339, UTC, e.g. `2026-10-20T03:37:00.123Z`. |
| `agent`          | string  | `claude-code`, `codex`, `copilot`, `cursor`, `gemini-cli`, or a name given to `witness record`/`exec`/the generic hook format. |
| `turn`           | string, optional | The agent's id for the turn (user prompt): Claude Code's `prompt_id`, Codex's `turn_id`, Cursor's `generation_id`. |
| `subagent`       | string, optional | Subagent type (or id) when a subagent made the call. |
| `model`          | string, optional | Model name, when the agent reports it. |
| `kind`           | string  | One of the kinds below. |

Ordering: events are ordered by `timestamp`, then by `session_id`, then by
their position in the file. Within one session file, position is authoritative.

### Line ranges

`lines` is `{"start": a, "end": b}`: 1-based, inclusive, `a <= b`, in the
file **as it was right after the event**. A pure deletion is recorded as the
two lines around the gap (`{start: s-1, end: s}`, clamped to 1) so it can
overlap the corresponding git hunk.

### Line hashes

A line hash identifies a line's content wherever it moves: the first 10 hex
digits (40 bits) of the SHA-256 of the line with leading and trailing
whitespace removed. Reports hash each line of the diff the same way and
match it to the edits that wrote it, so attribution survives rebases and
squash merges. 40 bits are plenty to tell a file's lines apart and far too
few to recover a line that isn't in the repository anyway.

### `edit`

The agent changed a file.

| Field    | Type            | Notes |
| -------- | --------------- | ----- |
| `file`   | string          | Path relative to the repo root, `/`-separated. |
| `lines`  | range or `null` | `null` means the whole file (range unknown). |
| `change` | object or absent| `{"start": s, "removed": r, "added": n}`: the edit replaced `r` lines starting at line `s` with `n` lines. Used to shift earlier ranges in the same file when later edits move lines. |
| `tool`   | string          | The agent's tool name, e.g. `Edit`, `apply_patch`, `create`, or the shell tool (`Bash`, `bash`, ...) for files a command changed. |
| `via`    | string, optional| How the range was observed: `patch`, `preimage`, `snapshot`, `content`, `locate`, `reported` or `unknown`. Absent in older logs. |
| `line_hashes` | array of strings, optional | Line hash of each line the edit added, in order (see below). Absent when unknown. |
| `removed_hashes` | array of strings, optional | Line hash of each line it removed, when known. |

One tool call may produce several `edit` lines (e.g. `replace_all`,
`MultiEdit`, or a command that changed several places). Within a call they
are in ascending line order, in the file's final coordinates.

How capture derives the range, most precise first:

1. `patch`: the agent's own patch (Claude Code's `structuredPatch`), with
   context lines trimmed;
2. `preimage`: the file copied by the pre-tool hook, diffed with the file
   after the tool (Codex `apply_patch`, Copilot `edit`/`create`, Gemini
   `replace`/`write_file`);
3. `snapshot`: working-tree snapshots before and after a shell command or
   unknown tool, diffed (`sed -i`, formatters, code generators);
4. `content`: for a newly created file, `1..=line count` of its content;
5. `locate`: the new text located in the file after the edit;
6. `reported`: stated with `witness record edit --lines`;
7. otherwise `null` (whole file, `via: "unknown"`).

### `read`

| Field  | Type   | Notes |
| ------ | ------ | ----- |
| `file` | string | Path relative to the repo root. |

### `command`

| Field            | Type             | Notes |
| ---------------- | ---------------- | ----- |
| `classification` | string           | `test`, `build`, `lint` or `other`, from the patterns in `witness.toml`. |
| `exit_code`      | integer or `null`| `null` when the exit status can't be attributed to the classified command: background commands, interrupted commands, or when the classified part is followed by `\|\|`, `;`, `&`, or by `\|` without an earlier `set -o pipefail` (as in `cargo test \| tail`). |
| `command`        | string           | Command text with secrets redacted, then truncated to 200 characters. |
| `outcome`        | string, optional | `passed` or `failed` for `test`, `build` and `lint` commands: from `exit_code` when it is attributable, else from the runner's summary in the output (read in memory, never stored). Absent when unknown. |
| `tests`          | object, optional | `{"passed": n, "failed": n, "skipped": n}` from the runner's summary; each count is optional. |
| `scope`          | object, optional | What the run covers, from its command line: `cwd` (repo-relative working directory), `packages` (e.g. `cargo -p`, npm workspaces), `paths` (directories: go packages, `--manifest-path`, gradle/maven modules), `filtered` (`true` when only some tests ran). Absent for the whole repository. |

Exit status comes from the agent:

- Claude Code: `PostToolUse` for `Bash` means exit 0; `PostToolUseFailure`
  carries `Exit code N` on the first line of `error`.
- Codex: the output text starts with `Exit code: N` (or says `Process
  exited with code N`).
- Copilot: the output's `<exited with exit code N>` marker, else unknown.
- Gemini CLI: `Exit Code: N` in the output, else unknown.
- `witness exec`: the process's own exit status.

### `assumption`

Recorded by `witness assume`.

| Field   | Type             | Notes |
| ------- | ---------------- | ----- |
| `file`  | string           | Path relative to the repo root. |
| `lines` | range or absent  | Absent means the whole file. |
| `text`  | string           | What the agent relied on without verifying, truncated to 500 characters, secrets redacted. |
| `line_hashes` | array of strings, optional | Line hashes of `lines` when the assumption was recorded. |

### `note`

Something about capture itself, e.g. that working-tree snapshots were turned
off for a session because they were too slow. Informational; reports ignore
it.

| Field  | Type   | Notes |
| ------ | ------ | ----- |
| `text` | string | One sentence. |

### `claim`

Written by the end-of-turn hook when the agent's final message asserts a
result outright ("all tests pass", "it builds cleanly"). The message itself
is never stored, only which claim it made and whether the record backed it
up at that moment.

| Field       | Type    | Notes |
| ----------- | ------- | ----- |
| `claim`     | string  | `tests-pass` or `builds`. |
| `supported` | boolean | False when this session's sections were failing (or, for `tests-pass`, unchecked or only compiled). |

### `session_end`

Written by the `SessionEnd` hook. Marks the log as complete.

| Field    | Type   | Notes |
| -------- | ------ | ----- |
| `reason` | string | The agent's reason, e.g. Claude Code's `clear`, `resume`, `logout`, `prompt_input_exit`, `other`. |

## What is never stored


- Tool output (`tool_response` contents, stdout, stderr, error text).
- Prompts and agent messages. The end-of-turn hook reads the agent's final
  message in memory to detect claims and keeps only the claim's kind.
- File contents, including `old_string`, `new_string` and `content`. Capture
  reads them in memory to compute line ranges and drops them. Pre-tool file
  copies live only under `.witness/state/pre/` (ignored by git) and are
  deleted by the post-tool hook; snapshots are git objects in your local
  object store, never pushed.
- Environment variables.
- Secrets in command or assumption text. Redaction replaces with
  `[REDACTED]`: `KEY=value` / `--token value` style pairs whose key names a
  secret (`token`, `secret`, `password`, `passwd`, `api_key`, `apikey`,
  `auth`, `credential`, `private_key`), `Bearer` / `Basic` header values,
  credentials in URLs (`https://user:pass@host`), and well-known token shapes
  (AWS access keys, GitHub `ghp_`/`gho_`/`ghs_`/`ghu_`/`ghr_`/`github_pat_`,
  GitLab `glpat-`, Slack `xox?-`, `sk-` API keys, JWTs).

## Example

```json
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:37:00.120Z","agent":"claude-code","kind":"read","file":"src/parse.rs"}
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:37:04.512Z","agent":"claude-code","turn":"c05f","kind":"edit","file":"src/parse.rs","lines":{"start":12,"end":18},"change":{"start":12,"removed":3,"added":7},"tool":"Edit","via":"patch","line_hashes":["1d5f0a3b9c","8e21c4f077","c3a9e5d012","4b7f81a6e3","f02d9c3b58","6a1e7d4c90","b5c08f2e71"],"removed_hashes":["9f3c2a1e88","07d4b6c5a2","e8a1f3920d"]}
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:37:09.001Z","agent":"claude-code","kind":"assumption","file":"src/parse.rs","lines":{"start":14,"end":15},"text":"the upstream API returns timestamps in UTC"}
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:37:30.774Z","agent":"claude-code","kind":"command","classification":"test","exit_code":0,"command":"cargo test -p parser","outcome":"passed","tests":{"passed":42,"failed":0,"skipped":1},"scope":{"packages":["parser"]}}
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:40:00.000Z","agent":"claude-code","kind":"session_end","reason":"other"}
```

## Compatibility

Adding optional fields or new kinds is a minor change and keeps
`schema_version` at 1; readers ignore unknown fields and skip unknown kinds.
Removing or changing the meaning of a field bumps `schema_version`.
