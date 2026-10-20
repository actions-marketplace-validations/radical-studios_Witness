---
name: witness
description: Use when changing code or running tests in a repository that has a .witness/ directory. Record unverified assumptions with `witness assume` (an API's behavior, a data format, a default value, a business rule) and run tests without piping their output so the result is recorded.
---

# Witness: say what you checked and what you guessed

This repository uses Witness. Hooks already record the files you read, the
edits you make and the commands you run, including whether tests or builds
passed after your edits. Reviewers get a summary that says which sections
were verified and which were not.

Hooks can't see what you assumed. Record that yourself.

## When to record an assumption

Run `witness assume` right after you write code that relies on something you
did not verify in this session, for example:

- how an external API, library function or CLI behaves (return values,
  errors, ordering, idempotency, rate limits);
- the shape or encoding of data (field names, units, time zones, nullability);
- a default value or configuration you didn't look up;
- a business rule or requirement you inferred rather than read.

If you checked it (read the source or docs, ran a test that exercises it,
inspected real data), don't record it.

## How

```bash
witness assume --file src/billing/invoice.rs --lines 40-52 "Stripe returns amounts in cents for all currencies"
```

- `--file` is the file containing the code that depends on the assumption.
- `--lines` is the range in the file as it is now; omit it if the
  assumption applies to the whole file.
- The text is one sentence stating the assumption, not the code.
- Never put secrets, tokens or personal data in the text.

## Rules

- Never claim something was checked, tested or verified when it wasn't. If
  you didn't run the tests after your last edit, say so.
- Prefer verifying over assuming: if a quick test or a read of the source
  would settle it, do that instead.
- Record each assumption once, close to where you relied on it.
- Let Witness see test and build results: run them without piping into
  `tail`, `head` or `grep`, or start the command with `set -o pipefail;`.
  A pipe otherwise hides the exit code, and the run won't count as a check.
