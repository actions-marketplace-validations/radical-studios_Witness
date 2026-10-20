# Privacy

Witness keeps a record of what coding agents did in your repository. The
record is committed with your code, so anyone who can read the repository
can read it. This page lists exactly what goes in it, what never does, and
what stays on your machine.

## What session logs contain

Session logs (`.witness/sessions/<session>.jsonl`, committed) hold one JSON
record per event. See [RECORD_FORMAT.md](RECORD_FORMAT.md) for the fields.

- **Who and when**: the agent's name, its session id, turn and subagent ids
  where the agent reports them, the model name where reported, and
  timestamps.
- **Files**:
  - paths relative to the repository, and line ranges;
  - for edits, a 40-bit hash (10 hex characters of SHA-256) of each line
    the agent added or removed.

  A hash can confirm a guess at a line's content, but can't be turned back
  into the line.
- **Commands**: the command text, with secrets redacted and truncated to
  200 characters. Also its classification (test, build, lint, other), its
  exit code and outcome, test counts (passed, failed, skipped) and which
  packages or paths it covered.
- **Assumptions**: the sentences the agent records with `witness assume`,
  truncated to 500 characters, with secrets redacted.
- **Claims**: when the agent's final message asserts that tests pass or the
  build succeeds, only that claim's kind (`tests-pass`, `builds`) and
  whether the record supported it.
- **Notes** about capture itself, e.g. that snapshots were turned off for a
  slow session.

## What is never stored

- File contents: neither the agent's edits (`old_string`, `new_string`,
  written content) nor anything it read.
- Tool output: command stdout and stderr, error text, test logs. Witness
  reads the end of a command's output in memory to find the test runner's
  summary line, and keeps only the counts.
- Prompts and agent messages. The end-of-turn hook reads the agent's last
  message in memory to detect claims, and keeps only the claim's kind.
- Environment variables.
- Secrets in commands and assumptions. Redaction replaces them with
  `[REDACTED]`:
  - `KEY=value` and `--token value` pairs whose name mentions a token,
    secret, password, API key, auth, credential or private key;
  - `Bearer` and `Basic` header values;
  - credentials in URLs;
  - well-known token shapes (AWS, GitHub, GitLab, Slack, `sk-` API keys,
    JWTs).

  Redaction is pattern-based and can miss secrets with unusual shapes.
  Don't put secrets on agent command lines.

## What stays local

`.witness/state/` is ignored by git (`.witness/.gitignore`) and holds:

- `index`: the private git index used for working-tree snapshots. The
  snapshot trees are objects in your local git object store; they are never
  referenced by a commit or pushed.
- `pre/`: copies of files taken just before an edit tool runs, deleted by
  the post-tool hook (and by `witness prune` if a tool call never
  finished).
- `last/`: the latest snapshot per session, for agents with only post-tool
  hooks.
- `reminders/`: end-of-turn reminders waiting for the next prompt.
- `seen/`: empty marker files that stop a hook installed twice from
  recording a call twice; removed when the session ends.
- `history.json`: the track record cache, with line hashes of landed
  sections.
- `hook.log`: hook errors, and timings with `WITNESS_DEBUG=1`; bounded to
  64 KiB.
- `current-session`: the session `witness assume` records into.

## Network

The hooks, `witness status`, `report`, `explain`, `sessions`, `stats`,
`doctor` and `prune` make no network requests. Only `witness post` does:
it sends the rendered review plan (file paths, line ranges, statuses,
reasons, command text as recorded) to the GitHub or GitLab API you point it
at, using `curl`, with the token passed on curl's standard input. The
installers download release archives from GitHub and verify their SHA-256
checksums.

## Removing records

Session logs are ordinary files in your repository. Delete one and commit
the deletion to drop it from future reports. `witness prune` removes old
logs whose changes have landed. To remove a log from history entirely, use
your usual history-rewriting tools (for example `git filter-repo`).
