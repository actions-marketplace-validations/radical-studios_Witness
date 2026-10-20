---
name: witness
description: Use when changing code or running tests in a repository that has a .witness/ directory. Run the tests that cover your edits before finishing, record unverified assumptions with `witness assume`, use `witness exec` for piped commands, and check `witness status` before saying you're done.
---

# Witness: say what you checked and what you guessed

This repository uses Witness. Hooks already record the files you read, the
edits you make (including edits made through the shell) and the commands you
run, with their real results. Reviewers get a plan that says which sections
were tested, which only compiled, which failed and which were never checked,
and they review the last kinds closely.

What hooks can't see is what you assumed. Record that yourself.

## Before you finish a task

1. Run the tests or the build that cover the code you changed, after your
   last edit. A run of another package doesn't count for these files, and a
   test-name filter (`cargo test parse`, `pytest -k`) counts as partial.
2. Run `witness status`. It lists this branch's sections that are failing,
   guessed or unchecked, including uncommitted work. Check what you can; for
   the rest, say in your reply why it couldn't be checked.
3. Only say the tests pass or the build succeeds if you ran them after your
   last edit and they did. Witness compares that sentence with the record,
   and a contradiction is shown to the reviewer.

If the turn ends with unchecked edits, Witness shows the user a note and
gives you the list with the next prompt. Treat it as a to-do list, not an
error.

## Commands whose output you pipe

Witness reads the real exit code and the test summary of plain commands.
When you pipe or filter the output, wrap the command so the real result is
recorded:

```bash
witness exec -- cargo test --workspace 2>&1 | tail -40
```

## Record assumptions

Run `witness assume` right after you write code that relies on something you
did not verify in this session, for example:

- how an external API, library function or CLI behaves (return values,
  errors, ordering, idempotency, rate limits);
- the shape or encoding of data (field names, units, time zones, nullability);
- a default value or configuration you didn't look up;
- a business rule or requirement you inferred rather than read.

```bash
witness assume --file src/billing/invoice.rs --lines 40-52 "Stripe returns amounts in cents for all currencies"
```

- `--file` is the file containing the code that depends on the assumption.
- `--lines` is the range in the file as it is now; omit it if the
  assumption applies to the whole file.
- The text is one sentence stating the assumption, not the code.
- Never put secrets, tokens or personal data in it.

If you checked it (read the source or docs, ran a test that exercises it,
inspected real data), don't record it. If a quick test or a read of the
source would settle it, do that instead of assuming.

## Other commands

- `witness explain src/parse.rs:42`: who wrote a line and what checked it.
- `witness report --format text`: the full review plan for the branch.
- `witness sessions`: the agent sessions recorded on this branch.

Commit `.witness/sessions/` together with the code; reviewers' reports are
built from it.
