# Contributing

Thanks for helping. Bug reports with a reproduction (the agent, its version,
and the hook payload or session log line that was misread) are as valuable
as code.

## Setup

You need Rust 1.88 or newer and git.

```bash
cargo build
cargo install --path crates/witness   # the hooks run the witness on PATH
```

This repository records its own agent sessions. Claude Code offers the
`witness` plugin when you open the repository. Rerun `cargo install` after
changing the Rust code so the hooks run your build. Commit
`.witness/sessions/` with your changes.

## Checks

CI runs these on Linux, macOS and Windows, plus an MSRV build:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check            # licenses and advisories (cargo install cargo-deny)
```

`cargo insta review` updates report snapshots after an intended change.

## Layout

- `crates/witness-core`: pure logic.
  - `capture/`: one adapter per agent, plus the shared event building.
  - Attribution (`provenance.rs`), classification (`classify.rs`,
    `signals.rs`), test summaries and scopes, the track record
    (`history.rs`).
  - Report rendering (`report/`).
- `crates/witness-git`: the git CLI wrapper.
- `crates/witness-platform`: GitHub and GitLab posting over `curl`.
- `crates/witness`: the CLI. Hooks, snapshots, the report pipeline,
  installers, `doctor`, `prune`, the history builder.
- `plugin/claude-code`: the Claude Code plugin. Its `hooks/hooks.json`
  must equal `witness hooks install claude-code`'s output; a test checks
  it.
- `docs/`: user and design documentation.

[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) explains how the pieces fit.

## Adding or fixing an agent adapter

1. Capture real payloads: point the agent's hook at
   `tee -a /tmp/payloads.jsonl` for a short session.
2. Add them, with secrets and paths scrubbed, under
   `crates/witness-core/tests/fixtures/hooks/<agent>/` and extend
   `capture/tests.rs`.
3. Map the payload in `crates/witness-core/src/capture/<agent>.rs`, and the
   hook config in `crates/witness/src/hooks.rs`.
4. Update the support matrix in [docs/AGENTS.md](docs/AGENTS.md).

Hooks must always exit 0, print nothing on stdout except a reply to a turn
event, make no network requests and stay fast. On a non-shell tool, the
median should stay under 10 ms.

## Compatibility

- Session logs are `schema_version: 1`. Adding optional fields or new
  kinds is fine; changing a field's meaning bumps the version (see
  [docs/RECORD_FORMAT.md](docs/RECORD_FORMAT.md)).
- The report JSON has its own `schema_version`.

## Releasing

1. Set the version in `Cargo.toml` (`workspace.package.version`) and
   `plugin/claude-code/.claude-plugin/plugin.json`. A test and the release
   workflow check that they match.
2. Move the `Unreleased` heading in `CHANGELOG.md` to the version and date.
3. Merge, then tag the merge commit and push the tag:

   ```bash
   git tag v0.2.0 && git push origin v0.2.0
   ```

4. The release workflow then:
   - builds the binaries;
   - publishes them with `SHA256SUMS`, `install.sh` and `install.ps1`;
   - for a stable version, moves the major tag (`v0`) that
     `uses: radical-studios/witness@v0` follows.

   Tags with a suffix (`v0.3.0-rc.1`) are published as pre-releases and
   don't move it.
