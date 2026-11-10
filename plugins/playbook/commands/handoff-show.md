---
description: Use at the start of a session after /clear, or when the saved handoff did not load. Loads the last handoff for this directory.
allowed-tools: Bash, Read
argument-hint: "[--all]"
model: haiku
effort: low
---

# Handoff Show

Pick up where the last session in this directory stopped. `/playbook:handoff-save` saves a handoff. When the session-start hook is installed, it loads that handoff on its own after `/clear`. Run this command when no handoff appeared, or to read it again.

## Run this now

Run every step immediately and do not ask for confirmation.

1. Load the handoff without deleting it. Intent: handoff-show. If the session's tool map (printed at session start) lists it, use that command, with `--all` when `$ARGUMENTS` has it. A handoff the hook already loaded still prints, labelled "already loaded by the last session start". If the tool map lacks it, follow `without-binary.md#handoff-show` in `skills/delegating-subagents/references/without-binary.md` in this plugin.
2. If nothing was found, say `No handoff saved for this directory` in one line and stop. When the folder holds handoffs for other directories, name the one that looks closest and tell the user to `cd` into that directory and run this command again.

3. Orient in three to five lines: where we are, the decisions that still matter, and the next steps in order. Use only what the handoff says.

4. Check it against reality with `git branch --show-current` and `git status --short`. Compare the branch and the working tree with the handoff's "Where we are now". Name each difference in one line. Do not fix anything.

5. Finish with the first next step as the proposed action. Do not start it.
