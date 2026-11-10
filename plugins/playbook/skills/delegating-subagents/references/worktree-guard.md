# Worktree guard

A commit or PR skill (`/playbook:commit-and-push`, `/playbook:create-pull-request`) run by a subagent starts in the session's checkout. That is not always the worktree of the agent that called it. Run this guard as the first step, before any other command, and again by `cd` at the top of every later bash block (each block is a new shell).

The caller passes two values in the subagent's prompt as "Worktree root: <path>, branch: <name>": the absolute worktree root (its `git rev-parse --show-toplevel`) and the expected branch (its `git branch --show-current`, or `(detached)` when that prints nothing). Call them `root` and `branch`.

1. `cd -- "<root>"`, quoted. If `root` is not an absolute path, or the `cd` fails, stop as in step 4 with `<toplevel>` and `<found-branch>` both `none`. When the caller gave `--dir <path>` (create-pull-request), that path is the root.
2. Run `git rev-parse --show-toplevel`.
3. Run `git branch --show-current`. It prints the name on an unborn branch too, and prints nothing only on a detached HEAD, which counts as `(detached)`. Callers use the same rule, so both sides compare equal. Never use `git rev-parse --abbrev-ref HEAD`, which fails on an unborn branch and is ambiguous for a branch named like a tag.
4. Compare both values to what the caller passed. On any mismatch stop with one line and nothing else:

   `WRONG_DIRECTORY: expected <root> on <branch>, found <toplevel> on <found-branch>`

   Commit nothing, push nothing, open nothing. Do not retry in another checkout, do not fix it with `git checkout`, and do not tell the caller to commit by hand.

When the caller passed no root and branch, run steps 2 and 3 anyway and name the top level and branch you found in your final message, so the caller sees where you acted.

A caller that gets a `WRONG_DIRECTORY` line back stops and reports it. It does not retry and does not commit by hand.
