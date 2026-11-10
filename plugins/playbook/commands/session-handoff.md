---
description: Deprecated, use /playbook:handoff-save. Removed in 1.0.0.
disable-model-invocation: true
allowed-tools: Bash, Read
argument-hint: "[path]"
model: haiku
effort: low
---

Print this line first: `/playbook:session-handoff is deprecated and will be removed in 1.0.0. Use /playbook:handoff-save.`

Then read `${CLAUDE_PLUGIN_ROOT}/commands/handoff-save.md` and follow it with these arguments: `$ARGUMENTS`.
