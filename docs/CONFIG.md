# Configuration

Witness reads `witness.toml` next to `.witness/` (the repository root, or
the directory `witness init --here` set up). Every section and key is
optional: a missing key takes its default, and a list you set replaces the
default list. Unknown keys are an error, so typos don't go unnoticed;
`witness doctor` reports them.

`witness init` writes the file with every default spelled out.

## `[report]`

```toml
[report]
max_flagged = 10
close = ["failing", "guessed", "unrecorded", "unverified"]
escalate = ["test-changed-after-failure", "assertions-removed", "high-risk-area"]
```

| Key | Default | |
| --- | --- | --- |
| `max_flagged` | `10` | Cap on sections in the close-review table; the rest go in a collapsed "over the cap" list, highest score first. |
| `close` | failing, guessed, unrecorded, unverified | Statuses to review closely. The others (`compiled`, `tested`) are listed to skim. |
| `escalate` | test-changed-after-failure, assertions-removed, high-risk-area | Signals that put a section up for close review whatever its status. |

Statuses: `failing`, `guessed`, `unrecorded`, `unverified`, `compiled`,
`tested`.

Signals:

- `test-changed-after-failure`: a test file was edited between a failing
  run and the next passing one.
- `assertions-removed`: lines removed from a test file look like
  assertions.
- `high-risk-area`: the section is in an `[[areas]]` entry with
  `risk = "high"`.
- `blind-edit`: the agent edited an existing file it never read in that
  session.
- `thrash`: the same lines were edited many times, or the tests failed
  repeatedly along the way.
- `filtered-test-run`: the passing run was limited by a name filter
  (`cargo test parse`, `pytest -k`, `go test -run`).
- `human-modified`: a human changed lines inside the agent's hunk.
- `rewritten-often`: the agent's earlier sections in this area were
  rewritten or reverted notably more often than the repository's average
  (see `[history]`).

The same names work for `--fail-on`.

## `[tests]`

```toml
[tests]
paths = ["**/tests/**", "**/test/**", "**/__tests__/**", "**/spec/**", "**/*_test.go", ...]
```

Globs for test files, relative to the Witness root. Edits to them get
test-specific signals (`test-changed-after-failure`, `assertions-removed`).

## `[nudge]`

```toml
[nudge]
mode = "remind"
```

What happens when the agent ends a turn with sections that are failing or
were never checked, or claims "all tests pass" against the record:

- `remind`: a one-line note for the user; the agent gets the list with the
  next prompt. Each list is passed once.
- `block`: the agent is asked to continue until it checks the sections or
  says why it can't, at most once in a row.
- `off`: nothing. Claims are still recorded for the report.

Which agents support which reply: [AGENTS.md](AGENTS.md#end-of-turn-nudges).

## `[history]`

```toml
[history]
enabled = true
horizon_days = 30
max_changes = 200
```

The agent track record (`witness stats`, and the track record in reports).

| Key | Default | |
| --- | --- | --- |
| `enabled` | `true` | Build and show the track record. |
| `horizon_days` | `30` | A landed section is judged this many days later: survived (80% of its lines still there), rewritten, or reverted. Changing it rebuilds the cache. |
| `max_changes` | `200` | How many of the latest landed changes (first-parent commits that added session logs) to examine. |

The cache is `.witness/state/history.json`.

## `[capture]`

```toml
[capture]
snapshots = true
snapshot_budget_ms = 500
```

| Key | Default | |
| --- | --- | --- |
| `snapshots` | `true` | Snapshot the working tree around shell commands and tools that report no patch, so the files they change are recorded. |
| `snapshot_budget_ms` | `500` | A snapshot slower than this turns snapshots off for the rest of the session and leaves a `note` in the log. `witness doctor` reports such sessions. |

Snapshots go through a private git index in `.witness/state/`; your index
and working tree are never touched. Read-only commands (`ls`, `cat`, `rg`,
`git status`, ...), background commands and git commands that replace the
working tree (`checkout`, `rebase`, ...) are not snapshotted.

## `[commands]`

```toml
[commands]
test = ['^cargo (\+\S+ )?(test|nextest)\b', '^go test\b', ...]
build = ['^cargo (\+\S+ )?(build|check)\b', '^(npx )?tsc\b', ...]
lint = ['^cargo (clippy|fmt)\b', ...]
```

Regexes matched against each simple command the agent runs. Commands are
split on `&&`, `||`, `;`, `|` and newlines (quotes respected). Leading
`VAR=value` assignments and wrappers (`timeout`, `time`, `env`, `nice`,
`witness exec`) are removed before matching. The strongest class wins:
`test` > `build` > `lint` > other.

- A passing `test` run that covers a file marks the agent's earlier edits
  there `tested`.
- A passing `build` marks them `compiled`.
- `lint` runs are listed but verify nothing.

The defaults cover cargo, npm/pnpm/yarn/bun, jest, vitest, mocha, pytest
(also through uv and poetry), go, gradle, maven, make, just, tox/nox,
rspec, rake, dotnet, deno, mix, phpunit/pest, ctest, swift, flutter, sbt,
bazel and zig.
Add your own, e.g. a project script:

```toml
[commands]
test = ['^\./scripts/test\b', '^cargo (\+\S+ )?(test|nextest)\b']
```

(Setting `test` replaces the default list, so keep the entries you rely
on.)

## `[attribution]`

```toml
[attribution]
agent_commits = [
  { agent = "claude-code", pattern = '(?im)^co-authored-by:\s*claude\b' },
  { agent = "copilot", pattern = '(?im)^co-authored-by:\s*copilot\b|copilot-swe-agent' },
  ...
]
```

Commits recognized as written by an agent, by a regex matched against the
text `author <email>`, `committer <email>`, then the commit message (use
`(?m)^` to anchor at the start of a line). Lines
from such commits that no Witness record covers are reported as
`unrecorded` (reviewed closely), unless the branch has a record from that
agent. The defaults cover Claude Code, Copilot, Codex, Cursor, Gemini, Aider
and Devin trailers and bot authors.

## `[[areas]]`

```toml
[[areas]]
name = "auth"
paths = ["src/auth/**", "migrations/**"]
risk = "high"

[[areas]]
name = "ui"
paths = ["web/**"]
```

Named groups of paths (globs relative to the Witness root). The first
matching area wins; other paths are in `default`.

- The track record is kept per agent and area.
- `risk = "high"` (default `normal`) adds the `high-risk-area` signal,
  which by default puts agent sections there up for close review.

## Environment variables

| Variable | |
| --- | --- |
| `WITNESS_BASE` | Base ref for reports, when `--base` isn't given. |
| `WITNESS_SESSION_ID` | Session for `witness assume`, `exec` and `record`. |
| `WITNESS_DEBUG=1` | Hooks log their timings to `.witness/state/hook.log`. |
| `WITNESS_GITLAB_TOKEN` | Token for `witness post --platform gitlab`. |
| `GITHUB_TOKEN` | Token for `witness post --platform github`. |
| `NO_COLOR` | No color in text output. |
