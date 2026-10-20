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
        ├── current-session      # id of the most recent session, for `witness assume`
        └── hook.log             # hook errors (and timings with WITNESS_DEBUG=1)
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
| `agent`          | string  | `claude-code` for this pass. |
| `kind`           | string  | One of the kinds below. |

Ordering: events are ordered by `timestamp`, then by `session_id`, then by
their position in the file. Within one session file, position is authoritative.

### Line ranges

`lines` is `{"start": a, "end": b}`: 1-based, inclusive, `a <= b`, in the
file **as it was right after the event**. A pure deletion is recorded as the
two lines around the gap (`{start: s-1, end: s}`, clamped to 1) so it can
overlap the corresponding git hunk.

### `edit`

The agent changed a file.

| Field    | Type            | Notes |
| -------- | --------------- | ----- |
| `file`   | string          | Path relative to the repo root, `/`-separated. |
| `lines`  | range or `null` | `null` means the whole file (range unknown). |
| `change` | object or absent| `{"start": s, "removed": r, "added": n}`: the edit replaced `r` lines starting at line `s` with `n` lines. Used to shift earlier ranges in the same file when later edits move lines. |
| `tool`   | string          | Tool name, e.g. `Edit`, `Write`, `MultiEdit`, `NotebookEdit`. |

One tool call may produce several `edit` lines (e.g. `replace_all`, `MultiEdit`).

How capture derives the range, most precise first:

1. `tool_response.structuredPatch` from Claude Code, with context lines trimmed;
2. locating `tool_input.new_string` in the file after the edit;
3. for `Write` of a new file, `1..=line count` of `tool_input.content`;
4. otherwise `null` (whole file).

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

Exit status comes from the hook event: `PostToolUse` for `Bash` means exit 0;
`PostToolUseFailure` carries `Exit code N` on the first line of `error`.

### `assumption`

Recorded by `witness assume`.

| Field   | Type             | Notes |
| ------- | ---------------- | ----- |
| `file`  | string           | Path relative to the repo root. |
| `lines` | range or absent  | Absent means the whole file. |
| `text`  | string           | What the agent relied on without verifying, truncated to 500 characters, secrets redacted. |

### `session_end`

Written by the `SessionEnd` hook. Marks the log as complete.

| Field    | Type   | Notes |
| -------- | ------ | ----- |
| `reason` | string | Claude Code's `reason` (`clear`, `resume`, `logout`, `prompt_input_exit`, `other`). |

## What is never stored

- Tool output (`tool_response` contents, stdout, stderr, error text).
- File contents, including `old_string`, `new_string` and `content`. Capture
  reads them in memory to compute line ranges and drops them.
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
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:37:04.512Z","agent":"claude-code","kind":"edit","file":"src/parse.rs","lines":{"start":12,"end":18},"change":{"start":12,"removed":3,"added":7},"tool":"Edit"}
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:37:09.001Z","agent":"claude-code","kind":"assumption","file":"src/parse.rs","lines":{"start":14,"end":15},"text":"the upstream API returns timestamps in UTC"}
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:37:30.774Z","agent":"claude-code","kind":"command","classification":"test","exit_code":0,"command":"cargo test -p parser"}
{"schema_version":1,"session_id":"3f1c","timestamp":"2026-10-20T03:40:00.000Z","agent":"claude-code","kind":"session_end","reason":"other"}
```

## Compatibility

Adding optional fields or new kinds is a minor change and keeps
`schema_version` at 1; readers ignore unknown fields and skip unknown kinds.
Removing or changing the meaning of a field bumps `schema_version`.
