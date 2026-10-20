---
description: Show what Witness recorded for this branch, including uncommitted work - what is failing, guessed or unchecked
allowed-tools: Bash(witness status:*)
---

## Witness status

!`witness status --format markdown`

Summarize this for the user in a few lines: how much of the agent-written
code was tested, and which sections need attention. For failing or unchecked
sections that came from this session, offer to run the tests or build that
cover them; for guessed ones, offer to verify the assumption. Don't run
anything until the user agrees.
