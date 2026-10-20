# Witness roadmap: from prototype to a trustworthy, agent-agnostic 1.0

## Status

Milestones M0 to M8 below are implemented in 0.2.0 (see
[CHANGELOG.md](../CHANGELOG.md)). Where the implementation differs from the
plan:

- **HTTP:** posting uses the `curl` binary instead of `ureq`. That keeps TLS
  out of the binary, and proxies and certificates behave as for other CI
  tools.
- **Reverts:** a revert is detected only from the "This reverts commit"
  message. The "≤20% survival within a revert window" rule was dropped:
  a section whose lines are gone is reported as *rewritten*.
- **`witness init`:** there is no `--yes`, because `init` never prompts.
  Agent detection replaces stack detection: the default command patterns
  already cover the common stacks.

What remains before 1.0 is in [1.0 criteria](#10-criteria) at the end.

## Context

Witness records what a coding agent verified versus what it guessed, then turns
`git diff base...head` into a review plan ("review 2 of 9 sections closely").
Today it is a clean, well-tested prototype (~5k lines of Rust: `witness-core`,
`witness-git`, `witness-platform`, `witness` CLI, plus a Claude Code plugin). It
is not yet trustworthy or broadly usable. Reading the code turned up these gaps:

- **Attribution breaks easily.** Hunks are matched to agent edits by line
  ranges, corrected only for the agent's own later edits (`timeline.rs`). A
  rebase onto a changed base, a human commit above the agent's code, or a
  squash can shift ranges, and agent code then shows as `human`.
- **Edits made through the shell are invisible.** Code written with
  `sed -i`, `cat > file`, formatters, codegen or scripts is never recorded, so
  it shows as `human` and drops out of the report.
- **Every historical session is replayed.** `pipeline::compute` loads every
  log in `.witness/sessions/`. An old agent edit can match a new human change
  at the same lines, and reports slow down as logs pile up.
- **Verification is too coarse.** Any passing test anywhere counts as
  "checked": `cargo test -p other-crate` marks unrelated files as checked, and
  build-only counts the same as tested. Piped runs (`cargo test | tail`, the
  most common agent habit) never count.
- **Only Claude Code is supported**, and `witness assume` guesses the session
  from a single `current-session` file, which races between sessions.
- **The report is minimal:** no links, no signals (blind edits, tests weakened
  after a failure), no CI annotations. GitHub and GitLab posting are stubs, and
  the track record is an interface only.
- **Adoption friction:** no prebuilt binaries, no CI action, no `doctor`. On
  macOS, `/private/var` vs `/var` symlinks can make payload paths look outside
  the repo, so events are silently dropped.

**Decisions:** be as agent-agnostic as reasonable (Claude Code,
Codex, GitHub Copilot, and more). The Stop-hook nudge defaults to *remind*.
Session logs stay committed in the repo. Ship prebuilt binaries.

**Outcome:** reviewers get an accurate, linked, prioritized review plan on
every PR, whichever agent wrote the code. Agents are nudged to verify before
they stop, and the tool can show over time that its flags predict code that
later gets rewritten.

## Architecture changes (overview)

1. **Capture becomes adapter-based.** Each agent's hook payload goes through a
   pure adapter and becomes a normalized `ToolCall`. Shared logic turns
   `ToolCall`s into records. Adapters: `claude_code`, `codex`, `copilot` (both
   the camelCase and the VS Code-style snake_case payloads), `cursor`,
   `gemini`, and `generic` (Witness's own documented JSON). Researched so far:
   - Codex `PostToolUse`/`Stop` use Claude-like fields (`session_id`,
     `turn_id`, `tool_name`, `tool_input`, `tool_response`, `tool_use_id`,
     `model`, `stop_hook_active`, `last_assistant_message`).
   - Copilot reads `.github/hooks/*.json` (`version: 1`) for both its CLI and
     its cloud agent. Its tool names include `bash`, `edit`, `create`,
     `apply_patch` and `str_replace_editor`. **Copilot `preToolUse` command
     hooks are fail-closed**, so `witness hook` must always exit 0.
2. **Universal change capture.** Edits are taken from the most precise source
   available:
   - the agent's own patch (Claude Code `structuredPatch`);
   - a per-file pre-image saved at the pre-tool hook and diffed in-process
     after the tool runs;
   - a git tree snapshot (private index + `write-tree`) before and after
     shell or unknown tools.

   With this, shell edits and agents without patch data are captured exactly.
3. **Content-anchored provenance.** Edits store short hashes of the lines they
   added and removed. The report attributes each diff line by hash. Line spans
   remain only as a fallback, for old logs and notebooks.
4. **Evidence-based verification.** Each command records:
   - its outcome, from the exit code or from the test runner's summary line;
   - per-runner test counts;
   - its scope (which packages or paths it covers).

   The status model gains `failing`, `compiled`, `tested` and `unrecorded`,
   plus signals and a priority score.
5. **Record schema stays `schema_version: 1`.** Only optional fields and new
   kinds are added, which `docs/RECORD_FORMAT.md` already allows. Old logs keep
   working. The report JSON moves to `REPORT_SCHEMA_VERSION = 2`, because
   `checked` splits into `tested` and `compiled`.

## Milestones

Ordered by dependency. Each milestone is a series of commits that leaves
`fmt`/`clippy`/`test` green.

### M0: Foundations and correctness fixes
- **Only this branch's sessions.** Add `Git::merge_base`. In
  `crates/witness/src/pipeline.rs::compute`, use only the events each log
  gained since the merge base: the head copy minus the base copy as a line
  prefix (logs are append-only), falling back to the whole log if the base copy
  isn't a prefix. Keep the existing working-tree override.
- **Path canonicalization.** `capture::resolve` matches against both the
  lexical and the canonical root (`CaptureContext` gains `canonical_root`,
  filled in by `crates/witness/src/hook.rs`). Fixes the macOS symlink drop.
- **`.witness/` below the git top level** (monorepo packages). Compute the
  prefix from `git rev-parse --show-prefix` in the pipeline, keep only hunks
  under it and strip it. This removes the "one repo root" limitation.
- **Hook diagnostics.** Write errors and timings (when `WITNESS_DEBUG=1`) to a
  bounded `.witness/state/hook.log`. Stderr stays as it is.
- **Test infrastructure:**
  - Move the helpers from `crates/witness/tests/end_to_end.rs` (`git`,
    `payload`, `hook`, `edit`, `bash`) into `tests/support/mod.rs` as a
    scenario builder.
  - Add `proptest` for `timeline::shift`, `command::split` and
    `witness_git::parse_unified_diff`.
  - CI: ubuntu, macOS and Windows; an MSRV 1.88 job; `cargo-deny`.
- **Dogfood:** run `cargo install --path crates/witness` in each
  implementation session, so this repo's own `.witness/sessions/` records the
  work.

### M1: Agent-agnostic capture
- **Normalized model** in `crates/witness-core/src/capture/mod.rs`:
  - `ToolCall { agent, session_id, turn_id, subagent, model, phase: Pre|Post|PostFailure, action }`.
  - `ToolAction::{Shell{command, background, exit, output}, Edit{files}, Read{file}, Other{name}}`.
  - Tool output is held in memory only and never stored.
- **Adapters:** move today's code into `capture/claude_code.rs` and add
  `codex.rs`, `copilot.rs`, `cursor.rs`, `gemini.rs` and `generic.rs`.
  - Agent comes from the `--agent` flag in installed hook configs; otherwise it
    is sniffed from the payload shape (`turn_id`+`model` → codex,
    `sessionId`/`toolArgs` → copilot, and so on).
  - Each adapter gets real-payload fixtures under
    `tests/fixtures/hooks/<agent>/`, captured with a temporary `tee` hook.
  - Generalize `bash_event`/`edit_events` from `capture.rs` into a shared
    `capture/events.rs`.
- **Change capture (I/O, in the CLI crate)** in a new
  `crates/witness/src/snapshot.rs`:
  - *Pre-images:* for edit tools with known paths, save files to
    `.witness/state/pre/<call-key>` at the pre-hook, diff after the tool with
    the `similar` crate, then delete them.
  - *Tree snapshots* for shell and unknown tools: a private index
    `.witness/state/index` seeded from `.git/index`, then `git add -A` +
    `write-tree`, then `git diff-tree -U0 -M pre post`, parsed by the existing
    `parse_unified_diff`.
  - *Skipped* for tools that can't write, and for read-only commands (a new
    allowlist in `witness-core/src/command.rs`: `ls`, `cat`, `rg`,
    `git status/log/diff/show`, …).
  - *Time budget:* `[capture] snapshot_budget_ms`, default 500. Once it's
    exceeded, snapshots stop for that session and a `note` record says so.
  - Agents with only post-hooks diff against the session's last snapshot.
  - New `Git` methods in `crates/witness-git/src/lib.rs`: `snapshot_tree`,
    `diff_trees`, `merge_base`, `show_prefix`.
- **Records** (optional fields): `turn`, `subagent` and `model` on every
  event; `edit.via` = `patch|preimage|snapshot|locate`. New kinds:
  `turn_end`, `note`.
- **Generic entry points:**
  - `witness record edit|command|read|assumption …` and `witness hook --agent generic`.
  - `witness exec -- <cmd>`: runs the command with output untouched and
    records the exact exit code, test summary and scope. Classification strips
    the `witness exec` wrapper so the outer shell event isn't double-counted.
- **`witness assume`:** the pre-tool hook writes the current session per agent
  just before each shell command, which fixes the race between concurrent
  sessions. Assumptions are recorded with that agent's name.
- **Hook installers:** `witness hooks install <agent>|--all` and
  `witness hooks status`, using templates in `crates/witness/src/hooks/`:
  - Claude Code: `.claude/settings.json`, as an alternative to the plugin.
  - Codex: `.codex/hooks.json`.
  - Copilot: `.github/hooks/witness.json`, plus a `copilot-setup-steps.yml`
    snippet that installs the binary for the cloud agent.
  - Cursor: `.cursor/hooks.json`.
  - Gemini CLI: `.gemini/settings.json`.

  Every template guards on the binary being present and always exits 0.
- **Agent commits with no hooks:** a `[attribution] agent_commits` config of
  trailer and author patterns, used in M2. Defaults: `Co-authored-by: Claude`,
  `Co-authored-by: Copilot`, `copilot-swe-agent[bot]`, the `(aider)` author
  suffix, and others.

### M2: Attribution engine (content-anchored provenance)
- **Capture side.** Every edit stores `line_hashes` and `removed_hashes`:
  40-bit truncated SHA-256 of each trimmed line, the same normalization as
  `history::HunkFingerprint::new`. `witness assume` reads the file and stores
  `line_hashes` for its range.
- **`Hunk` keeps removed lines.** Add `removed: Vec<String>` to `Hunk`
  (`crates/witness-core/src/hunk.rs`); `crates/witness-git/src/diff.rs`
  collects `-` lines the way it already collects `+` lines.
- **New `crates/witness-core/src/provenance.rs`:**
  - Every added line in a hunk gets an origin: the latest agent edit whose
    hashes contain it, or human.
  - "Neutral" lines (blank, punctuation-only, ≤2 characters) take their
    neighbors' origin. A common line that matches by hash also needs agreement
    from its neighbors or position.
  - Deletion hunks match on `removed_hashes`.
  - Events without hashes (old logs, notebooks, binaries) fall back to the
    existing `timeline.rs` span replay.
- **Mixed hunks split into sections** (agent runs vs human runs), so a human
  tweak to agent code becomes its own human section.
- **`unrecorded` status.** Lines with no recorded origin whose blame commit (at
  head) matches `agent_commits` become `unrecorded`. Blame runs once per file
  over the hunk ranges, using the existing `Git::blame`.
- **Result:** attribution survives rebases, later human commits, squash merges
  and cherry-picks. Remove the "line drift" limitation from
  `docs/ARCHITECTURE.md`.

### M3: Verification model
- **Test summaries:** new `crates/witness-core/src/testsum.rs`.
  - Parsers for cargo test/nextest, pytest, jest, vitest, mocha, go test,
    gradle, maven surefire, rspec, dotnet test, bun and deno.
  - Input is the in-memory tool output: Claude `stdout`, Copilot
    `textResultForLlm`, Codex `tool_response`, Cursor `output`.
  - Stores only `outcome: passed|failed|unknown` and
    `tests: {passed, failed, skipped}`.
  - Precedence: an attributable exit code, then the summary, then unknown. A
    piped run is now judged by its real result.
- **Scope:** new `crates/witness-core/src/scope.rs`.
  - Inputs: the cwd (the payload's cwd plus a leading `cd x &&`), and package
    or path arguments: cargo `-p/--workspace/--manifest-path`, go package
    patterns, pytest/jest/vitest paths, npm/pnpm/yarn workspace filters,
    gradle `:module:test`.
  - Name filters (`cargo test FILTER`, `-k`, `-t`, `-run`) mark the run
    `filtered`.
  - At report time, package names resolve to directories through manifests at
    head (`Cargo.toml`, `package.json`, `go.mod`, read with `git show`).
  - An unknown scope counts as the whole repo, and the reason says so.
- **Status model v2.** `classify.rs` becomes the v1 rules; the `Classifier`
  trait stays. Statuses: `failing`, `guessed`, `unrecorded`, `unverified`,
  `compiled`, `tested`, `human`. Close review by default covers failing,
  guessed, unrecorded and unverified; configurable with `[review] close`.
- **Signals**, each a short badge plus a reason:
  - `blind-edit`: the file was edited but never read, and is not new.
  - `test-changed-after-failure`: a test file was edited between a failing run
    and the next pass.
  - `assertions-removed`: deleted lines in test files match assertion
    patterns.
  - `thrash`: many edits to the same lines, or many failing runs.
  - `filtered-test-run`.
  - `human-modified`.
  - `high-risk-area`: `[[areas]] risk = "high"`.

  `[review] escalate` lists signals that lift a skim section into close review.
  `[tests] paths` holds default globs for test files.
- **Priority score:** status weight + signals + size + area risk. It orders
  the flagged list, and its output is deterministic.
- **Report JSON v2:** sections carry `status`, `signals`, `score`, `evidence`
  (last edit, verifying run, test counts) and `lines_added`.

### M4: Reports and local commands
- **Split `report.rs`** into `report/{mod,markdown,text,sarif,annotations}.rs`.
- **Markdown v2:**
  - a headline, plus a coverage line ("412 agent-written lines: 61% tested, 9% compiled only, …");
  - a flagged table with permalinks (`…/blob/<head-sha>/path#L12-L18`) and signal badges;
  - collapsible over-cap, skim (grouped by file) and "agent test runs" timeline sections;
  - a footer covering agents, models, sessions and turns;
  - truncation that respects platform limits (GitHub 65,536 characters).
- **New formats:** `text` (colored, honors `NO_COLOR`), `sarif` 2.1.0, `github`
  (`::warning file=…,line=…,endLine=…::` annotations) and
  `gitlab-codequality`.
- **New commands:**
  - `witness status`: the branch plus uncommitted work against the merge base.
    It also warns about uncommitted session logs.
  - `witness explain <file>:<line>`: the provenance and evidence timeline for
    one line.
  - `witness sessions`: sessions on this branch, with stats.
- **CI options:** `--fail-on <statuses|signals>` for gating. The base is
  auto-detected (`origin/HEAD` → `main` → `master`, or CI variables) instead
  of the hard-coded `main` in `cli.rs`.

### M5: Platforms and CI
- **HTTP layer.** `witness-platform` gets an `Http` trait with a `ureq`
  (rustls) implementation and a fake for tests. Tokens are never logged.
- **GitHub:**
  - Find the comment containing `REPORT_MARKER` (paginated issue comments),
    then `PATCH` it or `POST` a new one.
  - `GITHUB_API_URL` supports GitHub Enterprise Server.
  - On fork PRs (a 403), fall back to annotations and print a clear message.
- **GitLab:** the MR notes API (`CI_API_V4_URL`, `CI_PROJECT_ID`,
  `CI_MERGE_REQUEST_IID`, a `PRIVATE-TOKEN` from `WITNESS_GITLAB_TOKEN`).
- **`--platform auto`** plus `crates/witness/src/ci.rs`, which infers the repo,
  PR number (from `GITHUB_EVENT_PATH`), base, head and link base. A shallow
  clone gets an actionable error (`fetch-depth: 0`).
- **Packaging for CI:**
  - a composite `action.yml` at the repo root, which downloads the release
    binary and posts the comment and annotations;
  - `ci/gitlab/witness.yml` and `docs/CI.md`.
  - The release workflow (M8) lands here so the Action has binaries.
- **Dogfood:** `.github/workflows/witness.yml` runs the Action on this repo's
  own PRs.

### M6: Agent feedback loop
- **Stop hook** (Claude Code `Stop`, Codex `Stop`, Copilot `agentStop`, Cursor
  `stop`). It reuses the M4 `status` engine, scoped to the session.
  - `[nudge] mode = "remind"` (the default) returns a non-blocking
    `additionalContext` listing failing, unverified and guessed edits.
  - `"block"` returns `decision: "block"`, only when `stop_hook_active` is
    false, at most `max_blocks` times per turn.
  - `"off"` does nothing.
  - Each adapter renders the agent's own output format.
- **Claims check.** Conservative patterns on `last_assistant_message` ("all
  tests pass", "builds successfully", …) are compared with the evidence. Only
  `{kind: "claim", claim, supported}` is stored. The report and the nudge call
  out contradictions.
- **SessionStart:**
  - if the binary is missing, the hook template prints a one-line install
    hint;
  - on resume, it adds context summarizing unverified work.
- **Claude Code plugin 0.2:**
  - hooks for `PreToolUse`, `PostToolUse` (all tools), `PostToolUseFailure`,
    `Stop`, `SessionStart` and `SessionEnd`;
  - a rewritten skill, and `commands/status.md` and `commands/review.md`;
  - a version handshake with the binary.
- **Agents without skills:** `witness init` writes an agent-neutral
  `AGENTS.md` section with the same guidance (record assumptions, use
  `witness exec`, run `witness status` before finishing). Codex, Copilot and
  Cursor read that file.

### M7: Track record and calibration
- **Outcomes** in `witness-core/src/history.rs`:
  - `Survived`: ≥80% of the section's line hashes are still present at the
    horizon.
  - `Rewritten`: otherwise.
  - `Reverted`: a revert commit, or ≤20% survival within the revert window.
- **Builder** in a new `crates/witness/src/history.rs`:
  - Walk first-parent history for landing commits that added session-log
    lines.
  - Rebuild each change's sections with the M2–M3 pipeline on
    `parent..commit`.
  - Evaluate them at `history.horizon_days` (default 30).
  - Cache the results in `.witness/state/history.json`, which CI can cache;
    `FileHistoryStore` implements the existing `HistoryStore` trait.
- **Report:**
  - a per-(agent, area) track record replaces "No track record yet";
  - a calibration line ("unverified sections were rewritten 3.1× as often as tested ones");
  - score weighting for weak areas.
- **`witness stats`**, as a table or JSON.

### M8: Distribution, onboarding, operations, docs
- **Releases.** `.github/workflows/release.yml` runs on `v*` tags and builds
  linux x86_64/aarch64 (musl), macOS x86_64/arm64 and windows x86_64. It
  publishes archives with `SHA256SUMS`, plus `install.sh` and `install.ps1`
  (which verify checksums). Hook templates look for the binary on `PATH`, in
  `~/.local/bin` and in `~/.cargo/bin`.
- **`witness init` v2:**
  - detects agents (`.claude/`, `.codex/`, `.github/`, `.cursor/`, `.gemini/`)
    and installs their hooks;
  - detects stacks and tailors the command patterns;
  - writes `.gitattributes` (`.witness/sessions/** linguist-generated=true merge=union`);
  - `--ci github|gitlab` writes a workflow;
  - `--git-hooks` installs a pre-commit hook that stages `.witness/sessions/`;
  - `--yes` runs it non-interactively.
- **`witness doctor`** checks:
  - binary and plugin versions;
  - hooks installed and the last event seen, per agent;
  - config validity and log health;
  - shallow clones;
  - snapshot timings;
  - recent hook errors.
- **`witness prune --older-than 90d`** removes merged, old session logs.
- **Docs:**
  - a README rewrite: pitch, example report, per-agent quickstart, CI;
  - new `docs/AGENTS.md` (support matrix and capture fidelity per agent),
    `docs/CONFIG.md`, `docs/CI.md` and `docs/PRIVACY.md`;
  - updates to `ARCHITECTURE.md` and `RECORD_FORMAT.md`;
  - `CHANGELOG.md` and `CONTRIBUTING.md`;
  - version 0.2.0 at M5, 1.0 after M8.

## Cross-cutting rules
- **Hook invariants:**
  - always exit 0, including for Copilot's fail-closed `preToolUse`;
  - stdout is only ever a single JSON object, and only for Stop and
    SessionStart output;
  - no network access;
  - git only for snapshots, within the time budget.
- **Performance budgets:**
  - hook for non-shell tools: p50 under 10 ms;
  - shell snapshot: p50 under 150 ms on a 10k-file repo;
  - report on a 1k-hunk PR: under 2 s.
- **Privacy:**
  - still no file contents, tool output or environment variables;
  - line hashes are truncated to 40 bits; test results are counts only;
  - assistant messages are never stored;
  - pre-images live only under the gitignored `.witness/state/` and are
    deleted after use;
  - documented in `docs/PRIVACY.md`.
- **Keep reusing** `command::redact`/`sanitize_command`, `record::parse_log`
  (skip-unknown semantics), `HunkFingerprint` normalization,
  `workspace::append_events` (a single `O_APPEND` write), the `Git` trait and
  `FakeGit` in tests, and the `Platform` trait with `REPORT_MARKER`.

## Verification
- **Every commit:** `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo test --workspace`. CI runs them on Linux, macOS and Windows, plus
  MSRV.
- **Scenario suite** (insta snapshots of markdown and JSON):
  - each agent adapter, end to end from fixtures;
  - edits via `sed -i`, a formatter and codegen;
  - a rebase onto a changed base, a later human commit, a squash merge;
  - a human edit of an agent line;
  - a piped failing test;
  - a scoped test that doesn't cover the file;
  - failing tests at the end of a session;
  - a test changed after a failure;
  - an unrecorded co-authored commit;
  - parallel tool calls, subagents and multiple sessions.
- **Real-agent smoke test:** install hooks in a sample repo for Claude Code,
  plus Codex and Copilot CLI where available. Run a short task, then
  `witness status`, `witness report --format text`, `witness doctor`, and
  check `hook.log` timings.
- **CI dogfood:** PRs to this repo get the Witness comment and annotations
  from the Action. A tagged pre-release produces installable binaries.

## Verify before building
- The **Cursor and Gemini CLI payload shapes** are unverified: their docs
  weren't reachable from this environment. Capture real payloads into fixtures
  first, and label those adapters beta until then.
- **Codex `apply_patch` and shell `tool_response` shapes:** read the schemas
  in `openai/codex` (`codex-rs/hooks/src/schema.rs`) and capture fixtures.
- **Copilot cloud agent:** confirm that hook-written files in the repo are
  included in the agent's commits. If not, its code is still caught as
  `unrecorded` through the `copilot-swe-agent[bot]` author.
- **Snapshot cost on very large repos:** the time budget and
  `[capture] snapshots = false` are the escape hatches. Measure on a large
  open-source repo during M1.

## 1.0 criteria

1.0 promises a stable session log format, report JSON, config and CLI.
Before tagging it:

- **Real payloads for every adapter.** Cursor and Gemini CLI adapters were
  built from documentation. They leave beta once fixtures captured from
  real sessions pass. The same goes for Copilot's cloud agent end to end,
  including whether hook-written files land in its commits.
- **Dogfooding.** At least a month of this repository's own pull requests
  reviewed with the Action, with every misattribution or misclassification
  fixed or documented.
- **Calibration on real history.** On at least one repository with 100+
  landed agent changes, `witness stats` shows flagged sections rewritten
  noticeably more often than tested ones. If not, the rules change before
  the format freezes.
- **Platforms.** The release builds and the Action verified on Linux, macOS
  and Windows runners, and the GitLab template on a real merge request.
- **Performance budgets** from the cross-cutting rules measured on a
  10k-file repository and written down in `docs/ARCHITECTURE.md`.
- **Format review.** A last pass over record kinds, report fields and config
  keys, since after 1.0 removing or renaming any of them needs a major
  version.
