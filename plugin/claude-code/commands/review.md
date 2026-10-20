---
description: Review this branch with Witness's plan - closely where code is failing, guessed or unchecked, briefly elsewhere
argument-hint: "[base ref]"
allowed-tools: Bash(witness report:*), Bash(witness explain:*), Bash(git diff:*), Bash(git log:*), Read, Grep, Glob
---

Review the changes on this branch using Witness's review plan.

1. Run `witness report --format json` (add `--base $ARGUMENTS` if a base
   ref was given) and read `flagged` (sections to review closely, highest
   score first), `skim`, `summary` and `contradicted_claims`.
2. For each flagged section, read the code at its `file` and `lines` and
   review it for correctness. Use its `reason`, `signals` and `evidence`
   (the last test run, failing runs, recorded assumptions) to decide what to
   check: a guessed section rests on the assumption it lists; a section with
   `test-changed-after-failure` or `assertions-removed` may have had its
   test weakened instead of the code fixed. Use
   `witness explain <file>:<line>` for a line's history.
3. Skim the remaining sections for anything obviously wrong.
4. Report findings per section, most serious first, with file and line.
   Say which flagged sections you found no problem in. Mention any
   contradicted claims.

Don't change code during the review unless the user asks.
