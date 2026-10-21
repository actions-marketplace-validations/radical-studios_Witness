# Changelog

All notable changes to Witness. Versions follow [Semantic
Versioning](https://semver.org); before 1.0, minor versions may change
behavior and the report format.

## 0.2.0 (2026-10-21)

A rebuild of the 0.1 prototype into an agent-agnostic tool. Session logs
stay at `schema_version: 1`, and 0.1 logs keep working. The report JSON
moves to schema 2.

### Capture

- Hooks for Codex CLI, GitHub Copilot (CLI and cloud agent), Cursor (beta)
  and Gemini CLI (beta) next to Claude Code, installed with
  `witness hooks install <agent>`.
- `witness exec -- <cmd>` runs a command and records its exact result.
  `witness record` and a generic JSON hook format cover agents without
  hooks.
- Edits made through the shell (`sed -i`, formatters, code generators) are
  recorded from working-tree snapshots taken with a private git index,
  within a time budget.
- Edit tools without a patch are diffed against a copy taken before the
  tool ran.
- Paths through symlinks (macOS `/var` → `/private/var`) are no longer
  dropped.
- `witness assume` records into the right session when several run at
  once.

### Attribution

- Diff lines are matched to agent edits by content hashes, so attribution
  survives rebases, squash merges, cherry-picks and later human commits.
- Hunks split where the author changes.
- Only the branch's own session events count; older sessions no longer
  claim new changes.
- `.witness/` can live in a package below the git top level
  (`witness init --here`).
- Lines from commits that name an agent (e.g. `Co-authored-by: Claude`)
  with no Witness record are reported as `unrecorded`.

### Verification

- Statuses `failing`, `guessed`, `unrecorded`, `unverified`, `compiled` and
  `tested` replace `checked`/`unverified`.
- A run counts only for the files it covers (`cargo test -p x`,
  `go test ./pkg/...`, `cd web && npm test`).
- Test runners' summary lines are parsed in memory, so piped runs
  (`cargo test | tail`) count. Only the counts are kept.
- Signals: test changed after a failure, assertions removed, high-risk
  area, blind edit, thrash, filtered test run, human-modified,
  rewritten-often. Sections are ordered by a priority score.

### Reports

- Markdown with a coverage line, permalinks, signal badges, collapsible
  skim and runs sections, and GitHub's size limit respected.
- New formats: `text`, `sarif`, `github` (annotations) and
  `gitlab-codequality`.
- `--fail-on` for CI gates.
- `witness status` (the branch plus uncommitted work), `witness explain
  <file>:<line>` and `witness sessions`.
- The base branch is detected from CI variables or origin's default branch.

### Platforms and CI

- `witness post` creates or updates one comment on GitHub pull requests
  (Enterprise Server too) and GitLab merge requests. On fork pull requests
  it falls back to the job summary.
- A GitHub Action (`uses: radical-studios/witness@v0`), a GitLab CI
  template, and prebuilt binaries for Linux, macOS and Windows with
  checksum-verifying installers.

### Feedback loop

- At the end of each turn the agent is reminded, or optionally blocked,
  when its edits are failing or unchecked.
- "All tests pass" claims are checked against the record; contradictions
  appear in the report.
- The Claude Code plugin 0.2: a rewritten skill, `/witness:status` and
  `/witness:review`, and a release check against the binary.
- Agent instruction files (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`) get a
  short Witness section.

### Track record

- `witness stats` and reports show how often each agent's sections were
  rewritten or reverted after landing, per area and status.
- A calibration line says whether flagged sections really were rewritten
  more often than tested ones.

### Setup and operations

- `witness init` detects agents and installs their hooks.
- `init` also marks session logs in `.gitattributes`, and with `--ci` and
  `--git-hooks` sets up CI and a pre-commit hook that stages logs.
- `witness doctor` checks the setup.
- `witness prune` removes old, landed session logs.

## 0.1.1

- Plugin hooks find `witness` in `~/.cargo/bin` when it isn't on `PATH`.

## 0.1.0

- First prototype: Claude Code hooks, session logs, and a markdown review
  plan from `git diff`.
