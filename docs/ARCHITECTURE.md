# Witness architecture

Witness records what a coding agent verified while it worked and turns that
into a review plan for the pull/merge request. It has three parts: capture,
core, and platforms. Everything except posting works from plain git and files
in the repository.

```
 agent hooks ──▶ witness hook --agent <a> <event> ─┐  (adapters, snapshots)
 agent / script ──▶ witness assume|record|exec ────┤ append
                                               ▼
                           .witness/sessions/<id>.jsonl  (committed)
                                               │
 git diff base...head ──▶ hunks ──┐            │
                                  ▼            ▼
                    witness-core: replay events → attribute lines by content
                                  → classify → report (markdown | json)
                                               │
                         witness-platform: github | gitlab | dry-run
```

## Crates

| Crate               | Role | I/O |
| ------------------- | ---- | --- |
| `witness-core`      | Data model, record format, hook payload parsing, command classification and redaction, config parsing, event→hunk matching, hunk classification, report rendering, track-record types. | None. Callers pass in strings, file contents and a clock. |
| `witness-git`       | `Git` trait over the `git` CLI (`diff`, `rev-parse`, `merge-base`, `ls-tree`, `show`, `log`, `blame`), working-tree snapshots through a private index, and a unified-diff parser that turns `git diff -U0` into `Hunk`s. | Spawns `git`. No libgit2. |
| `witness-platform`  | `Platform` trait with GitHub (issue comments), GitLab (MR notes) and dry-run adapters, over an `Http` trait. | HTTP through the `curl` binary; dry run writes to the given writer. |
| `witness`           | CLI (clap derive): `init`, `hooks`, `hook`, `assume`, `exec`, `record`, `report`, `post`. Owns all filesystem access, snapshots, and wires the crates together. | Filesystem, stdin/stdout, child processes, `git` via `witness-git`. |

Dependency direction: `witness` → `witness-platform`, `witness-git` →
`witness-core`. Core depends on nothing in the workspace.

## Capture

Each agent's hooks run `witness hook --agent <agent> <event>` (see
[AGENTS.md](AGENTS.md) for what `witness hooks install` writes per agent).
The pipeline is the same for every agent:

1. **Adapter** (`witness-core/src/capture/<agent>.rs`, pure): the JSON
   payload becomes a `HookInput`: agent, session, turn, subagent, model, and
   an event (`Tool(ToolCall)`, `SessionEnd`, `Stop`, ...). A `ToolCall` has a
   phase (pre, post, post-failure) and an action: `Shell` (command, exit
   status, output held in memory), `Edit` (files, with any patch,
   replacements or content), `Read`, `NoWrite` or `Unknown`. Without
   `--agent`, the agent is guessed from the payload's shape.
2. **Repo root**: the nearest ancestor of the payload's `cwd` containing
   `.witness/`; if none, the hook exits 0 without writing.
3. **Capture plan** (`capture_plan`, pure): how to observe the call's file
   changes. Nothing for tools that can't write, for read-only or background
   shell commands, for git commands that move the working tree, and for
   edits the agent reports a patch of; file pre-images for edit tools
   without a reliable patch; working-tree snapshots for other shell commands
   and unknown tools.
4. **Snapshots** (`witness/src/snapshot.rs`): the pre-tool hook saves a tree
   id (`git add -A` into a private index, then `git write-tree`) or copies
   the target files; the post-tool hook does the same and diffs (`git
   diff-tree -U0`, or an in-process line diff). Agents with only
   post-tool hooks are compared with the session's previous snapshot. A
   snapshot over the time budget turns snapshots off for the session.
5. **Records** (`events_for`, pure): commands are classified and redacted;
   edits come from the observed diff or, failing that, the agent's patch,
   the new file's content, or the new text located in the file.
6. **Append** to `.witness/sessions/<session>.jsonl` and update
   `.witness/state/current-session`.

It always exits 0, because some agents (Copilot) deny a tool call whose
pre-tool hook fails. Internal errors (including panics and argument errors)
are logged to stderr, which agents don't show the model on exit 0, and,
once the repo is known, to the local `.witness/state/hook.log` (bounded to
64 KiB). With `WITNESS_DEBUG=1`, every hook call also logs its duration
there. There is no network access; git runs only for snapshots.

`witness assume`, `witness record` and `witness exec` write records
directly. They use the session from `--session`, `$WITNESS_SESSION_ID`, or
`.witness/state/current-session`, in that order. The pre-tool hook updates
that file just before each shell command runs, so a `witness assume` run by
the agent lands in the right session even when several are active.

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
   append-only), so a report works before the logs are committed. Only the
   lines each log gained since the merge base of `base` and `head` are
   used: events that were already in the base belong to earlier changes
   and must not match this one's hunks.
3. **Replay.** Events are sorted (timestamp, session, position). Edits and
   assumptions with ranges are kept as live spans per file; each later edit
   with a known `change` shifts the spans below it by `added - removed`.
   Spans are the fallback for edits recorded without line hashes.
4. **Attribute** (`witness-core/src/provenance.rs`). Each added line of a
   hunk is hashed the way capture hashes the lines an edit wrote; its author
   is the latest agent edit whose `line_hashes` contain it, else human.
   Content, not position, decides, so attribution survives rebases, later
   human commits and squash merges. Lines that say nothing about their
   author (blank, `}`, `end`) take their neighbors' author; short lines
   (`return x;`) count only near the edit's replayed range or next to other
   agent lines, since they recur. Pure deletions match the edits'
   `removed_hashes`. Edits without hashes (older logs, notebooks, binary
   files) match by replayed span. A hunk whose lines have different authors
   becomes several segments, so a human tweak inside agent code is its own
   section. Assumptions attach to a segment when they cover the whole file,
   their recorded lines hash the same, or their replayed range overlaps.
5. **Classify** (`witness-core/src/classify.rs`, the swappable module). Runs
   count for a segment when they come after its last edit and cover its
   file (`witness-core/src/scope.rs`: package selectors such as `cargo -p`,
   go package paths, npm workspaces, gradle and maven modules, and a working
   directory that is a package root; package names are resolved with the
   manifests at head). A run's result is its exit code when that is
   attributable to it, else the runner's summary line
   (`witness-core/src/testsum.rs`), else unknown.

   | Status       | Rule | Review (default) |
   | ------------ | ---- | ------ |
   | `failing`    | the last covering test failed, or the last covering build did with no test since | look closely |
   | `guessed`    | otherwise, an assumption covers the segment | look closely |
   | `unrecorded` | no agent edit wrote it, but every line blames to a commit attributed to an agent (`[attribution]`) that has no records on the branch | look closely |
   | `unverified` | written by the agent, nothing above and no covering pass | look closely |
   | `compiled`   | only a covering build passed | skim |
   | `tested`     | a covering test passed | skim |
   | `human`      | no agent edit wrote it | excluded |

   Every segment gets a one-line reason, the run that decided it, and
   signals (`witness-core/src/signals.rs`): `test-changed-after-failure`,
   `assertions-removed`, `high-risk-area`, `blind-edit`, `thrash`,
   `filtered-test-run`, `human-modified`. `[report] close` lists the
   statuses to review closely and `[report] escalate` the signals that add
   a section to them. `lint` commands don't verify behavior and are ignored.
6. **Report.** Sections for close review, highest score first (status
   weight, plus signals, plus size), up to `report.max_flagged`; the rest
   are listed to skim. Markdown and JSON carry the same data
   (`witness_core::report::Report`, `schema_version` 2).

### Report shape

```markdown
<!-- witness:report -->
### Witness review plan

**Review 2 of 9 sections closely, skim the rest.**

Agents wrote 412 lines: 61% tested, 9% only compiled, 30% unchecked.

| File | Lines | Status | Why |
| --- | --- | --- | --- |
| `src/parse.rs` | [12-18](https://github.com/o/r/blob/<sha>/src/parse.rs#L12-L18) | failing | `cargo test` failed (exit 101; 1 failed) after the last edit |
| `tests/parse.rs` | [40-41](…) | tested | `cargo test` passed after the last edit (142 passed)<br><sub>⚑ changed after `cargo test` failed</sub> |

<details><summary>7 sections to skim</summary> … </details>
<details><summary>Agent checks: 6 test and build runs, last passed</summary> … </details>

<sub>claude-code (2 sessions, claude-opus-5-5) · 1 human-written section not counted · Track record: …</sub>
```

When every agent-written section is for close review, the header drops
"skim the rest". The first line is a hidden marker so platform adapters can
find and update their previous comment instead of adding a new one. Lists
are shortened, then dropped, to keep the comment under GitHub's 65,536
character limit.

Other renderings of the same `Report`: `text` (terminal, `witness status`),
`json`, `sarif` (2.1.0; one result per close-review section, fingerprinted
by content), `github` (workflow-command annotations) and
`gitlab-codequality`.

## Platforms

```rust
pub trait Platform {
    fn name(&self) -> &'static str;
    fn post(&self, report: &RenderedReport, target: &PostTarget) -> Result<PostOutcome, PlatformError>;
}
```

`RenderedReport` holds the markdown, which starts with `REPORT_MARKER`.
Adapters find the comment containing the marker on the PR/MR, update it if
found, and otherwise create one; only that differs per platform, the
report is computed identically everywhere from git.

- `github`: issue comments through the REST API (`GITHUB_API_URL` for
  GitHub Enterprise Server).
- `gitlab`: merge request notes through the REST API (`CI_API_V4_URL`).
- `dry-run`: prints what would be posted.

HTTP goes through a small `Http` trait implemented by running `curl`
(proxies and system certificates work as for any other tool; the token
travels on curl's stdin). Tests use a scripted fake. In CI,
`crates/witness/src/ci.rs` reads the repository, PR number, head commit
and job summary file from the environment (`GITHUB_EVENT_PATH`,
`CI_MERGE_REQUEST_IID`, ...). A 401/403 in CI, as for pull requests from
forks, is a warning: the plan stays in the job summary and annotations.
See [CI.md](CI.md).

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

- **Repeated short lines.** Content matching can't tell two identical
  short lines apart; they count as the agent's only near its recorded range
  or next to other agent lines, so such a line moved far away by a rebase
  may show as human.
- **Unread results.** A run whose exit code isn't attributable (piped,
  `|| true`) and whose output has no summary Witness recognizes counts as
  unknown, never as a pass. `witness exec -- <cmd>` records the exact code.
- **Nested roots.** `.witness/` usually sits at the git top level.
  `witness init --here` puts it in a subdirectory instead (one package of a
  monorepo); reports run from inside it cover only changes below it, with
  paths relative to it.
- **Redaction is pattern-based** and can miss secrets with unusual shapes.
