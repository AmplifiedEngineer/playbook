# Slash commands

Every `/playbook:*` command in one table. For the binary's own commands, run `playbook commands` or see the [CLI and config reference](cli-and-config.md).

| Command | What it does |
|---|---|
| `/playbook:setup` | Wires the guards, seeds `settings.json`, installs what you choose. Safe to repeat. |
| `/playbook:doctor` | Checks the seven layers and prints a hint for each miss. |
| `/playbook:plan` | Idea to a verified, parallel-safe plan for `/playbook:implement`. |
| `/playbook:implement` | Runs a plan or ADR blueprint with subagents and TDD. `--auto` opens a PR. |
| `/playbook:fix` | One small bug: failing test, smallest fix, one PR. Hands off to `plan` if it grows. |
| `/playbook:adr` | Writes a fact-checked Architecture Decision Record to `docs/adr/`. |
| `/playbook:commit` | Writes the message from the staged diff, commits signed, pushes. |
| `/playbook:pr-create` | Opens a PR with pre-flight checks and the team template. |
| `/playbook:review` | The review entry point. Recommends a quick or a deep review from the diff, asks you to pick, then runs it. `--quick` and `--deep` skip the question. |
| `/playbook:quick-review` | One-pass PR review, posted as a pending GitHub review. `/playbook:review` runs it, and you can call it directly. |
| `/playbook:deep-review` | Parallel specialist reviewers, consolidated and fact-checked. `/playbook:review` runs it, and you can call it directly. |
| `/playbook:address-pr-comments` | Walks unresolved review threads, fixes or drafts replies. |
| `/playbook:learn-project` | Reads the repo, PRs and tickets, then writes distilled facts to memory. |
| `/playbook:repo-audit` | Read-only audit with severity-rated findings and a task plan. |
| `/playbook:session-handoff` | Writes a handoff note (state, decisions, next steps, open questions) and saves it for the next session. Use it before `/clear` or when you stop. |
| `/playbook:session-start` | Loads the last handoff for this directory. |

A command that was renamed or removed is listed in [Renamed and removed commands](../renamed.md).

Reviews are stack aware. When a PR sits in a stack, `/playbook:review` (through the review it runs) always asks whether to review the whole stack or one PR (`playbook pr stack` finds it, see [ADR-0019](../adr/0019-pr-stack-review.md)).

Skills load on demand from the `playbook` plugin in `plugins/playbook`: review and research discipline, engineering standards, writing style, subagent delegation and debugging. To add your own, see [Commands, skills and hooks](../authoring/01-commands-skills-hooks.md).
