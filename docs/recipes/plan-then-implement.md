# How to go from an idea to a built feature

<!-- verify: names -->

Turn an idea into a checked plan, build it in small pull requests, then review the result.

**You need:** a Claude Code session in your repo with the playbook plugin installed. Type these commands in that session, not in a shell.

1. Plan the idea. The command interviews you and checks the plan before it saves it: `/playbook:plan add a retry to the sync job`.
2. Build the plan. Each segment of the plan becomes one small pull request: `/playbook:implement`.
3. Review your branch. The command picks a quick or a deep review, says why, and runs it: `/playbook:review`.

## What you see

Playbook saves the plan outside your checkout. With no argument, `/playbook:implement` lists your saved plans and asks which one to build. It builds one segment at a time and opens each pull request as a draft. `/playbook:review` names the review it recommends before it runs it.

## Next

- [Plan and implement](../guides/01-plan-and-implement.md): the full flow, with auto mode.
- [Review and PR flow](../guides/02-review-and-pr-flow.md): commit and review.
