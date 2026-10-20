# Witness

AI coding agents present every change with the same confidence, but some
parts they tested and some they guessed. Witness runs alongside the agent
and records what it actually checked (tests and builds that ran after each
edit and what they covered, files it read) and what it assumed. On every
pull or merge request it tells reviewers where to look: *"Review 3 of 4
sections closely, skim the rest"*, with the reason for each.

It works with Claude Code, Codex, GitHub Copilot, Cursor and Gemini CLI, and
with any other agent through a command wrapper. The record is committed with
the code under `.witness/`, so reports come from plain git on any platform.

```markdown
### Witness review plan

**Review 3 of 4 sections closely, skim the rest.**

Agents wrote 4 lines: 25% tested, 75% unchecked.

⚠ claude-code ended a turn saying the tests pass, but its record didn't back that up.

| File | Lines | Status | Why |
| --- | --- | --- | --- |
| `src/billing.rs` | 2-3 | guessed | Assumed: the sales tax rate is 8% for every region |
| `src/util.rs` | 2 | unverified | Edited by claude-code; no test or build ran after the last edit |
| `tests/parse.rs` | after 3 (deletion) | tested | `cargo test` passed after the last edit (3 passed)<br><sub>⚑ changed after `cargo test` failed · removes 1 assertion</sub> |

<details>
<summary>1 section to skim</summary>

- `src/parse.rs` 2: tested, `cargo test` passed after the last edit (3 passed)
</details>
```

The test the agent loosened right after it failed is flagged even though the
tests then passed. The line edited after the last test run is flagged too,
and the agent's "all tests pass" is called out.

## Quickstart

1. **Install the binary** (Linux, macOS):

   ```bash
   curl -fsSL https://raw.githubusercontent.com/radical-studios/witness/main/install.sh | sh
   ```

   Windows: `irm https://raw.githubusercontent.com/radical-studios/witness/main/install.ps1 | iex`.
   From source (Rust 1.88+):
   `cargo install --git https://github.com/radical-studios/witness witness --locked`.
   Witness needs `git` on `PATH`.

2. **Set up the repository:**

   ```bash
   cd your-repo
   witness init --ci github --git-hooks
   git add -A && git commit -m "Set up Witness"
   ```

   `init` writes `witness.toml` and `.witness/`, installs hooks for the
   agents it finds (their config in the repository or their CLI on your
   `PATH`; choose with `--agents codex,copilot`), adds a short section to
   their instruction files (`AGENTS.md`, `CLAUDE.md`, `GEMINI.md`) and
   marks session logs as generated in `.gitattributes`. `--ci` adds the
   pull request job. `--git-hooks` adds a pre-commit hook that stages
   session logs, so they never get left behind.

   With Claude Code, also add the plugin. It brings a skill that teaches
   the agent to verify and record assumptions, plus the `/witness:status`
   and `/witness:review` commands:

   ```text
   /plugin marketplace add radical-studios/witness
   /plugin install witness@radical-studios
   ```

3. **Work as usual.** Before you open the pull request:

   ```bash
   witness status          # what is failing, guessed or unchecked right now
   witness doctor          # if anything seems off
   ```

   Commit `.witness/sessions/` with your changes; CI posts the plan on the
   pull request.

## Agents

| Agent | Set up with | Edits | Shell commands | Status |
| --- | --- | --- | --- | --- |
| Claude Code | plugin, or `witness hooks install claude-code` | agent's patch | snapshots | stable |
| Codex CLI | `witness hooks install codex` | `apply_patch` pre-images | snapshots | stable |
| GitHub Copilot (CLI, cloud agent) | `witness hooks install copilot` | pre-images | snapshots | stable |
| Cursor | `witness hooks install cursor` | located from old/new text | snapshots | beta |
| Gemini CLI | `witness hooks install gemini-cli` | pre-images | snapshots | beta |
| Anything else | `witness exec -- <cmd>`, `witness record` | snapshots or reported | exact | stable |

Edits made through the shell (`sed -i`, formatters, code generators) are
caught by working-tree snapshots taken through a private git index. Hooks
never block the agent and always exit 0. Details, per-agent notes and the
generic hook format: [docs/AGENTS.md](docs/AGENTS.md).

## How sections are classified

Each line of `git diff base...head` is matched to the agent edit that wrote
it by content (a short hash of each line), so attribution survives rebases,
squashes and later commits. Hunks are split where authors change.

| Status | Meaning | Review |
| --- | --- | --- |
| failing | a test or build that covers the file failed after the agent's last edit | closely |
| guessed | the agent recorded an assumption about these lines | closely |
| unrecorded | committed by an agent (e.g. a `Co-authored-by: Claude` trailer) that left no record | closely |
| unverified | written by the agent; nothing that covers the file passed afterwards | closely |
| compiled | only a build passed after the last edit | skim |
| tested | a test that covers the file passed after the last edit | skim |
| human | no agent wrote it | not counted |

"Covers" comes from the command line. `cargo test -p parser` covers only
the `parser` crate, `go test ./internal/x/...` only that directory, and
`cd web && npm test` only the `web` package. Piped runs
(`cargo test | tail`) are judged by the runner's own summary line. Test
commands for cargo, npm/pnpm/yarn/bun, pytest, go, gradle, maven, dotnet,
rspec, mocha, deno and others are recognized out of the box.

Signals add weight and can force close review:

- a test changed right after it failed;
- assertions removed;
- a high-risk area (`[[areas]] risk = "high"`);
- an edit to a file the agent never read;
- the same lines rewritten over and over;
- a filtered test run;
- a human edit inside agent code;
- an agent whose earlier changes in this area were often rewritten.

## The feedback loop

**At the end of each turn**, Witness checks what the session wrote. If
something is failing or unchecked, or the agent says "all tests pass" when
the record disagrees, you see a one-line note, and the agent gets the list
with your next prompt. `[nudge] mode = "block"` has the agent keep going
until it checks or explains; `"off"` turns this off.

**After changes land**, Witness follows what happened to them.

- `witness stats` shows how often each agent's sections were rewritten or
  reverted within 30 days, per area and per status.
- It also shows whether flagged sections really were rewritten more often
  than tested ones.
- Reports include the track record once there is history, and an agent's
  weak areas raise its sections' priority.

## Commands

| Command | |
| --- | --- |
| `witness init` | Set up the repository (see above). |
| `witness status` | The plan for your branch, uncommitted work included. |
| `witness report` | The plan for `base...head`: `--format markdown\|text\|json\|sarif\|github\|gitlab-codequality`. |
| `witness post` | Post or update the plan on the GitHub pull request or GitLab merge request. |
| `witness explain src/a.rs:42` | Who wrote a line, how, and which checks ran after it. |
| `witness sessions` | Agent sessions recorded on the branch. |
| `witness stats` | The agents' track record. |
| `witness doctor` | Check the setup: binary, hooks, plugin, logs, hook errors and timings. |
| `witness assume` | Record an assumption (the agent runs this). |
| `witness exec -- <cmd>` | Run a command and record its exact result, for piped commands and agents without hooks. |
| `witness hooks install\|uninstall\|status` | Manage agent hooks. |
| `witness prune` | Remove old session logs that have landed. |

`--fail-on failing,test-changed-after-failure` makes `report`, `status` and
`post` exit 1 on a match, for CI gates. The base defaults to the CI target
branch, else origin's default branch, else `main` or `master`.

## In CI

```yaml
# .github/workflows/witness.yml (witness init --ci github writes it)
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

The action posts one comment (updated on each push), annotates the lines to
review closely and writes the job summary. Pull requests from forks get the
summary and annotations. GitLab, SARIF and other CI systems:
[docs/CI.md](docs/CI.md).

## Privacy

Session logs hold paths, line ranges, 40-bit line hashes, tool names,
redacted command text, exit codes, test counts and the assumptions the agent
states. They never hold file contents, tool output, prompts, agent messages
or environment variables. Nothing is sent anywhere except the report you
post. See [docs/PRIVACY.md](docs/PRIVACY.md).

## Documentation

- [docs/AGENTS.md](docs/AGENTS.md): agent support, capture and nudges.
- [docs/CONFIG.md](docs/CONFIG.md): every `witness.toml` setting.
- [docs/CI.md](docs/CI.md): GitHub Actions, GitLab and other CI.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it works.
- [docs/RECORD_FORMAT.md](docs/RECORD_FORMAT.md): the session log format.
- [docs/PRIVACY.md](docs/PRIVACY.md): what is recorded and what isn't.
- [CHANGELOG.md](CHANGELOG.md), [CONTRIBUTING.md](CONTRIBUTING.md),
  [docs/ROADMAP.md](docs/ROADMAP.md).

## License

Apache-2.0
