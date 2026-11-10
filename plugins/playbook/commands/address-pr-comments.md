---
description: Deprecated, use /playbook:pr-comments. Removed in 1.0.0.
disable-model-invocation: true
allowed-tools: Bash, Read, Edit, Write, Grep, Glob, Skill, AskUserQuestion
argument-hint: "[PR number] [--bots] [--dry-run] [-y|--yes] [--auto] [--ask]"
model: opus
effort: medium
---

Print this line first: `/playbook:address-pr-comments is deprecated and will be removed in 1.0.0. Use /playbook:pr-comments.`

Then read `${CLAUDE_PLUGIN_ROOT}/commands/pr-comments.md` and follow it with these arguments: `$ARGUMENTS`.
