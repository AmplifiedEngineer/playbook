---
description: Deprecated, use /playbook:learn. Removed in 1.0.0.
disable-model-invocation: true
allowed-tools: Bash, Read, Grep, Glob, Write, Agent, Skill, WebFetch
argument-hint: "[--refresh] [--graph-only] [--stage] [--from-staged] [--max-prs N] [--max-commits N] [--auto] [--ask]"
model: opus
effort: medium
---

Print this line first: `/playbook:learn-project is deprecated and will be removed in 1.0.0. Use /playbook:learn.`

Then read `${CLAUDE_PLUGIN_ROOT}/commands/learn.md` and follow it with these arguments: `$ARGUMENTS`.
