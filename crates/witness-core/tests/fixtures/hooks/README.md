# Hook payload fixtures

- `claude-code-v2/`: captured from a real Claude Code 2.x session, with paths
  and ids anonymized. The files directly in this directory are older,
  hand-written Claude Code payloads.
- `codex/`: built from the payload structs in `openai/codex`
  (`codex-rs/hooks/src/schema.rs`) and the shell/apply_patch handlers.
- `copilot/`: built from GitHub's Copilot hooks reference (camelCase and
  VS Code compatible formats).
- `cursor/`, `gemini-cli/`: built from each agent's hook documentation.
  These adapters are beta until the fixtures are replaced by captured
  payloads.
- `generic/`: Witness's own format (`docs/AGENTS.md`).

Replace a built fixture with a captured one whenever possible: add a
temporary hook that runs `tee /tmp/payload.json`, trigger the event, then
anonymize paths and ids.
