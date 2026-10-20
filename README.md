# Witness

AI coding agents present every change with the same confidence, but some
parts they verified and some they guessed. Witness runs alongside the agent
while it works and records what it actually checked (tests and builds run
after an edit, files read) versus what it assumed. Then it summarizes the
pull/merge request for human reviewers: *"Review 2 of 9 sections closely,
skim the rest"*, with a one-line reason for each section. The record lives
in your repository under `.witness/`, so the report works from plain git on
any platform.

> Status: early. Capture for Claude Code, the report, and dry-run posting
> work end to end. Posting to GitHub/GitLab and per-agent track records are
> interfaces only. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Install

You need Rust 1.88+ and `git` on your `PATH`.

```bash
cargo install --git https://github.com/radical-studios/witness witness
```

Then add the Claude Code plugin, which installs the hooks and a skill that
teaches the agent to record assumptions:

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

Agents without hooks can run commands through `witness exec -- cargo test`
and report edits with `witness record`. See [docs/AGENTS.md](docs/AGENTS.md)
for what each agent supports.

## Quickstart

```bash
cd your-repo
witness init                      # writes witness.toml and .witness/
git add witness.toml .witness && git commit -m "Set up witness"
git switch -c my-feature

claude                            # work with the agent as usual

git add -A && git commit -m "Feature"   # include .witness/sessions/
witness report --base main --head HEAD
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

- `src/lib.rs` 2: checked, `cargo test` passed after the last edit
</details>
```

Other commands:

- `witness report --format json` prints the same data as JSON.
- `witness assume --file src/x.rs --lines 10-20 "the API returns UTC"` records
  an assumption (the agent runs this; the skill tells it when).
- `witness post --platform dry-run` prints what would be posted.
- `witness init --here` sets Witness up for one directory (e.g. a package in
  a monorepo) instead of the whole repository.
- `WITNESS_DEBUG=1` makes the hooks log their timings to
  `.witness/state/hook.log`, where errors are always logged.

## How sections are classified

Each line of `git diff base...head` is matched to the agent's recorded edits
by content (a short hash of each line the agent wrote), so attribution
survives rebases and later commits. Hunks are split where authors change:

| Status | Meaning | Review |
| --- | --- | --- |
| guessed | the agent recorded an assumption about these lines | look closely |
| unverified | the agent edited them, and no test or build passed afterwards | look closely |
| unrecorded | committed by an agent (e.g. a `Co-authored-by: Claude` trailer) that left no record on the branch | look closely |
| checked | a test or build passed after the last edit | skim |
| human | no agent edit; not counted | n/a |

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
