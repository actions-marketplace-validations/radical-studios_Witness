# Witness

AI coding agents present every change with the same confidence, but some
parts they verified and some they guessed. Witness runs alongside the agent
while it works and records what it actually checked (tests and builds run
after an edit, files read) versus what it assumed. Then it summarizes the
pull/merge request for human reviewers: *"Review 2 of 9 sections closely,
skim the rest"*, with a one-line reason for each section. The record lives
in your repository under `.witness/`, so the report works from plain git on
any platform.

> Status: early. Capture for Claude Code, Codex, GitHub Copilot, Cursor and
> Gemini CLI, the report, and posting to GitHub and GitLab work end to end.
> See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Install

You need `git` on your `PATH`. Install a prebuilt binary (Linux, macOS):

```bash
curl -fsSL https://raw.githubusercontent.com/radical-studios/witness/main/install.sh | sh
```

On Windows, `irm https://raw.githubusercontent.com/radical-studios/witness/main/install.ps1 | iex`.
Or build from source with Rust 1.88+:

```bash
cargo install --git https://github.com/radical-studios/witness witness --locked
```

Then add the Claude Code plugin, which installs the hooks, a skill that
teaches the agent to verify its edits and record assumptions, and the
`/witness:status` and `/witness:review` commands:

```text
/plugin marketplace add radical-studios/witness
/plugin install witness@radical-studios
```

For a local checkout, `claude --plugin-dir ./plugin/claude-code` loads it
for one session. The hooks call `witness` from your `PATH` and do nothing in
repositories that haven't run `witness init`.

Other agents get their hooks from `witness hooks install`, which writes the
agent's own config file in the repository:

```bash
witness hooks install codex copilot   # also: claude-code, cursor, gemini-cli, --all
```

It also adds a short Witness section to the instruction file the agent
reads (`AGENTS.md`, `GEMINI.md`, `CLAUDE.md`). Agents without hooks can run
commands through `witness exec -- cargo test`
and report edits with `witness record`. See [docs/AGENTS.md](docs/AGENTS.md)
for what each agent supports.

## Quickstart

```bash
cd your-repo
witness init                      # writes witness.toml and .witness/
git add witness.toml .witness && git commit -m "Set up witness"
git switch -c my-feature

claude                            # work with the agent as usual

witness status                    # what's unchecked so far
git add -A && git commit -m "Feature"   # include .witness/sessions/
witness report                    # the plan reviewers get
```

Example output:

```markdown
<!-- witness:report -->
### Witness review plan

**Review 2 of 3 sections closely, skim the rest.**

| File | Lines | Status | Why |
| --- | --- | --- | --- |
| `src/parse.rs` | 1-4 | guessed | Assumed: callers always pass UTF-8 |
| `src/lib.rs` | 10 | unverified | Edited by claude-code; no test or build ran after the last edit |

<details>
<summary>1 section to skim</summary>

- `src/lib.rs` 2: tested, `cargo test` passed after the last edit (12 passed)
</details>
```

Other commands:

- `witness status` shows the plan for your branch including uncommitted
  work, in the terminal: what is failing, guessed or unchecked right now.
- `witness explain src/parse.rs:42` shows who wrote a line, how, and which
  checks ran after it.
- `witness sessions` lists the agent sessions recorded on the branch.
- `witness stats` shows the agent track record: how often agent-written
  sections were rewritten or reverted within 30 days of landing, per agent,
  area and status, and whether sections Witness flagged were rewritten more
  often than tested ones. Reports include it once there is history.
- `witness report --format json|text|sarif|github|gitlab-codequality`
  prints the same data for tools: SARIF for code scanning, `github` for
  inline annotations in GitHub Actions, `gitlab-codequality` for GitLab's
  merge request widget.
- `--fail-on failing,test-changed-after-failure` makes `report`, `status`
  and `post` exit 1 when a section has one of those statuses or signals,
  for CI gates.
- `witness assume --file src/x.rs --lines 10-20 "the API returns UTC"`
  records an assumption (the agent runs this; the skill tells it when).
- `witness post --platform dry-run` prints what would be posted.
- `witness init --here` sets Witness up for one directory (e.g. a package in
  a monorepo) instead of the whole repository.
- `WITNESS_DEBUG=1` makes the hooks log their timings to
  `.witness/state/hook.log`, where errors are always logged.

The base branch defaults to the CI target branch, else origin's default
branch, else `main` or `master`; pass `--base` to choose. Report links
point at the head commit on GitHub or GitLab when the remote is on one.

### When the agent stops

At the end of each turn, Witness checks the sections the session wrote. If
some are failing or unchecked, or the agent says "all tests pass" when the
record disagrees, you see a one-line note and the agent gets the list with
your next prompt. Set `[nudge] mode = "block"` in `witness.toml` to have the
agent keep going until it checks them or explains why it can't, or `"off"`
to turn this off. Contradicted claims also appear in the report.

## In CI

```yaml
# .github/workflows/witness.yml
on: pull_request
permissions: { contents: read, pull-requests: write }
jobs:
  review-plan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: radical-studios/witness@v0
```

GitLab, other CI systems, SARIF and gating: [docs/CI.md](docs/CI.md).

## How sections are classified

Each line of `git diff base...head` is matched to the agent's recorded edits
by content (a short hash of each line the agent wrote), so attribution
survives rebases and later commits. Hunks are split where authors change:

| Status | Meaning | Review |
| --- | --- | --- |
| failing | a test or build that covers the file failed after the agent's last edit | look closely |
| guessed | the agent recorded an assumption about these lines | look closely |
| unrecorded | committed by an agent (e.g. a `Co-authored-by: Claude` trailer) that left no record on the branch | look closely |
| unverified | the agent wrote them, and nothing that covers the file passed afterwards | look closely |
| compiled | only a build passed after the last edit; no test ran | skim |
| tested | a test that covers the file passed after the last edit | skim |
| human | no agent edit; not counted | n/a |

"Covers" is read from the command line: `cargo test -p parser` covers only
the `parser` crate, `go test ./internal/x/...` only that directory, and
`cd web && npm test` only the `web` package. A piped run
(`cargo test | tail`) is judged by the runner's own summary line.

Sections also carry signals, such as a test edited right after it failed,
assertions removed from a test, an edit to a file the agent never read, the
same lines rewritten over and over, a filtered test run, a change in a
`risk = "high"` area, or an agent whose earlier changes in this area were
often rewritten. The first two and the high-risk area put a section up
for close review whatever its status (`[report] escalate` in
`witness.toml`).

Which commands count as tests or builds is configured in `witness.toml`.
Defaults cover cargo, npm/pnpm/yarn/bun, pytest, go, gradle and maven.

## What is recorded

File paths, line ranges, 40-bit hashes of the lines the agent wrote, tool
names, command text (secrets redacted, truncated to 200 characters), exit
codes and the assumptions the agent states. Never tool output, file contents
or environment variables. See
[docs/RECORD_FORMAT.md](docs/RECORD_FORMAT.md).

## Development

This repository records its own agent sessions with Witness. Claude Code
offers the `witness` plugin when you open the repo (from
`.claude/settings.json`); the hooks also need the binary from this checkout:

```bash
cargo install --path crates/witness   # rerun after changing the Rust code
```

Commit `.witness/sessions/` along with your changes.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check            # licenses and advisories (cargo install cargo-deny)
```

## License

Apache-2.0
