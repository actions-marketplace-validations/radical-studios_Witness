# Witness architecture

Witness records what a coding agent verified while it worked and turns that
into a review plan for the pull/merge request. It has three parts: capture,
core, and platforms. Everything except posting works from plain git and files
in the repository.

```
 Claude Code ──hooks──▶ witness hook <event> ─┐
 agent ─────────────▶ witness assume ...  ────┤ append
                                               ▼
                           .witness/sessions/<id>.jsonl  (committed)
                                               │
 git diff base...head ──▶ hunks ──┐            │
                                  ▼            ▼
                    witness-core: replay events → match to hunks
                                  → classify → report (markdown | json)
                                               │
                         witness-platform: dry-run | github* | gitlab*
                                                       (* stubs in v0)
```

## Crates

| Crate               | Role | I/O |
| ------------------- | ---- | --- |
| `witness-core`      | Data model, record format, hook payload parsing, command classification and redaction, config parsing, event→hunk matching, hunk classification, report rendering, track-record types. | None. Callers pass in strings, file contents and a clock. |
| `witness-git`       | `Git` trait over the `git` CLI (`diff`, `rev-parse`, `ls-tree`, `show`, `log`, `blame`) and a unified-diff parser that turns `git diff -U0` into `Hunk`s. | Spawns `git`. No libgit2. |
| `witness-platform`  | `Platform` trait. `DryRun` writes to any `io::Write`. `github` and `gitlab` return `NotImplemented`. | Dry run writes to the given writer only. No network in v0. |
| `witness`           | CLI (clap derive): `init`, `hook`, `assume`, `report`, `post`. Owns all filesystem access and wires the crates together. | Filesystem, stdin/stdout, `git` via `witness-git`. |

Dependency direction: `witness` → `witness-platform`, `witness-git` →
`witness-core`. Core depends on nothing in the workspace.

## Capture

The Claude Code plugin (`plugin/claude-code/`) registers command hooks:

| Claude Code event     | Matcher                              | Call |
| --------------------- | ------------------------------------ | ---- |
| `PostToolUse`         | `Edit\|Write\|MultiEdit\|NotebookEdit\|Read\|Bash` | `witness hook post-tool-use` |
| `PostToolUseFailure`  | `Bash`                               | `witness hook post-tool-use-failure` |
| `SessionEnd`          | (all)                                | `witness hook session-end` |

`PostToolUse` only fires for tool calls that succeeded, so a `Bash` event
there means exit 0. Failed commands arrive as `PostToolUseFailure`, whose
`error` starts with `Exit code N`. That is why both are hooked.

`witness hook`:

1. reads the JSON payload from stdin (never waits on a terminal);
2. finds the repo root by walking up from the payload's `cwd` to a directory
   containing `.witness/`; if none, exits 0 without writing;
3. converts the payload to zero or more records (pure, in core; the CLI passes
   a closure that reads a file when a line range has to be located);
4. appends them to `.witness/sessions/<session>.jsonl` and updates
   `.witness/state/current-session`.

It always exits 0. Internal errors (including panics and argument errors) are
logged to stderr, which Claude Code doesn't show to the model on exit 0. Exit
code 2 would block the agent's tool call, so it is never used. There is no
network and no `git` call on this path; the cost is one process start, a
TOML parse and a file append.

`witness assume --file <path> [--lines a-b] "<text>"` is run by the agent
itself (taught by the plugin's skill). It finds the session from
`--session`, `$WITNESS_SESSION_ID`, or `.witness/state/current-session`, in
that order, and appends an `assumption` record.

See [RECORD_FORMAT.md](RECORD_FORMAT.md) for the records.

## Core pipeline (`witness report`)

1. **Hunks.** `git diff --no-color --no-ext-diff -U0 -M <base>...<head>`
   (three dots: changes on head since the merge base, which is what a PR
   shows). Parsed into `Hunk { file, old_start, old_count, new_start,
   new_count, added_lines }`. Paths under `.witness/` and `witness.toml` are
   dropped. Binary files become one whole-file hunk.
2. **Session logs.** Every `.witness/sessions/*.jsonl` in the head commit's
   tree. When `head` resolves to the checked-out `HEAD`, the working-tree
   copy of each log is used instead (it is a superset, since logs are
   append-only), so a report works before the logs are committed.
3. **Replay.** Events are sorted (timestamp, session, position). Edits and
   assumptions with ranges are kept as live spans per file; each later edit
   with a known `change` shifts the spans below it by `added - removed`.
   This corrects for the agent's own later edits moving lines. It does not
   correct for human commits made after the session (see Limitations).
4. **Match.** A hunk's span at head is `new_start..new_start+new_count-1`;
   a pure deletion becomes the two lines around the gap. An event matches a
   hunk when it names the same file and its span overlaps, or it has no span
   (whole file).
5. **Classify** (`witness-core/src/classify.rs`, the swappable module):

   | Status       | Rule | Review |
   | ------------ | ---- | ------ |
   | `guessed`    | an assumption overlaps the hunk | look closely |
   | `human`      | otherwise, no agent edit overlaps the hunk | excluded |
   | `checked`    | a `test` or `build` command exited 0 after the hunk's last overlapping edit, and no `test`/`build` failed after that pass | skim |
   | `unverified` | edited by the agent, none of the above | look closely |

   Every hunk gets a one-line reason. Commands with `exit_code: null` count
   as neither pass nor fail. `lint` commands don't verify behavior and
   are ignored by v0.
6. **Report.** Flagged hunks (guessed first, then unverified, then by path
   and line) up to `report.max_flagged`; the rest are listed as skimmable.
   Markdown and JSON carry the same data (`witness_core::report::Report`).

### Report shape

```markdown
<!-- witness:report -->
### Witness review plan

**Review 2 of 9 sections closely, skim the rest.**

| File | Lines | Status | Why |
| --- | --- | --- | --- |
| `src/parse.rs` | 12–18 | guessed | Assumed: the upstream API returns timestamps in UTC |
| `src/cli.rs` | 40–41 | unverified | Edited by claude-code; no test or build ran after the last edit |

<details>
<summary>7 sections to skim</summary>

- `src/lib.rs` 3–9: checked, `cargo test` passed after the last edit
</details>

Track record: No track record yet.
```

When every agent-written section is flagged, the header drops "skim the
rest". The first line is a hidden marker so platform adapters can find and update
their previous comment instead of adding a new one.

## Platforms

```rust
pub trait Platform {
    fn name(&self) -> &'static str;
    fn post(&self, report: &RenderedReport, target: &PostTarget) -> Result<PostOutcome, PlatformError>;
}
```

`RenderedReport` holds the markdown and the `REPORT_MARKER`. Adapters are
expected to: find an existing comment containing the marker on the PR/MR,
update it if found, otherwise create one. Only that differs per platform; the
report is computed identically everywhere from git. In v0, `github` and
`gitlab` return `PlatformError::NotImplemented`; `dry-run` prints what would
be posted.

## Track record (interfaces only)

Later, Witness learns how each agent's changes fare per area of the codebase
from git history: hunks that are reverted or rewritten soon after merge count
against the agent in that area, and flags are weighted accordingly.

- `HunkFingerprint { path, content_hash }`: `content_hash` is SHA-256 of the
  hunk's added lines with each line trimmed, blank lines dropped, joined with
  `\n`. History is keyed by this, never by commit SHA, so squash and rebase
  merges (which change SHAs but not content) don't break it.
- `Area`: named path globs from `witness.toml`; a hunk's area is the first
  matching entry, else `default`.
- `HistoryEntry { fingerprint, agent, area, outcome, observed_at }` with
  `Outcome::{Survived, Rewritten, Reverted}`.
- `TrackRecord { agent, area, samples, rewritten, reverted }`.
- `trait HistoryStore { fn record(..); fn track_record(agent, area) }`;
  `NullHistoryStore` returns nothing, so the report says
  "No track record yet."

`witness-git` already exposes `log` and `blame` for the future history
builder.

## Configuration (`witness.toml`)

```toml
[report]
max_flagged = 10          # cap on sections flagged for close review

[commands]                # regexes, matched against each simple command
test  = ['^cargo (\+\S+ )?(test|nextest)\b', ...]
build = ['^cargo (\+\S+ )?(build|check)\b', ...]
lint  = ['^cargo (clippy|fmt)\b', ...]

[[areas]]
name = "core"
paths = ["crates/witness-core/**"]
```

Omitted keys take built-in defaults (cargo, npm/pnpm/yarn/bun, pytest,
go test, gradle, maven). Command text is split on `&&`, `||`, `;`, `|` and
newlines (quote-aware); leading `VAR=value` assignments and wrappers like
`timeout`, `time`, `env`, `nice` are stripped before matching; the strongest
class wins (`test` > `build` > `lint` > `other`).

## Limitations of v0

- **Line drift from human commits.** Ranges are corrected for the agent's
  own later edits but not for commits made after the session. A human
  inserting lines above an agent edit can shift it off its hunk; it then
  shows as `human`. A future version can re-map ranges through
  `git diff <session commit> head`.
- **Pipes hide exit codes.** `cargo test | tail` records `exit_code: null`,
  so such runs never count as checks (unless the line starts with
  `set -o pipefail`). Agents pipe test output often; the skill asks them not
  to. A later version could read the test runner's summary line instead.
- **One repo root.** `.witness/` must be at the git top level.
- **Redaction is pattern-based** and can miss secrets with unusual shapes.
