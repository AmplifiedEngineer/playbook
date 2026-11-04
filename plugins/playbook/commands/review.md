---
description: Use to review a PR or your own branch. It recommends a quick or a deep review from the diff, asks you to pick, then runs it. The single review entry point.
allowed-tools: Bash, Read, Grep, Glob, Agent, Skill
argument-hint: "[PR number] [--quick] [--deep] [--self] [--no-post] [--all] [--preset <name>] [--this-pr] [--whole-stack] [--auto] [--ask] [--help]"
model: sonnet
effort: low
---

# Review

One command for every review. For a PR in a stack it settles the scope first (this PR only, or the whole stack). It then looks at the diff of that scope, recommends a quick or a deep review, lets you pick, then runs it. In ask mode you always have the final say. `/playbook:quick-review` and `/playbook:deep-review` stay as direct entry points and do the work: this command only picks between them.

## Help

If the arguments contain `--help`, print the argument-hint line and these lines, then stop:

```
/playbook:review [PR number] [options]

  --quick      Quick review, no depth question
  --deep       Deep review, no depth question
  --this-pr    Review this PR only, no scope question
  --whole-stack  Review the whole stack, no scope question
  --all, --preset <name>   Deep review options (they imply --deep)
  Every other option goes to the review that runs: --self, --no-post,
  --auto, --ask.
```

## Step 0: Read the run mode

Do this first, before Step 1. Read the run mode and the effort ceiling for `review` with `$ARGUMENTS`: read `skills/delegating-subagents/references/run-context.md` in this plugin and follow it. It covers the tool, the plain steps without it, the warning, the ceiling, the flag conflict and any failure.

**Routing.** Where you choose what to spawn, route the `classify` kind. Intent: route-model. If the session's tool map (printed at session start) lists it, use that command and follow its `action` under the `routing.escalate` gate: on `ask` stop and ask the user, on `downgraded` (`deny`) use the lower tier it names, and in auto mode (`auto`) proceed and log the assumption. If the command fails, carry on. Otherwise do this: spawn the agent this file names, with the model its agent file pins.

- **`ask` mode:** behave exactly as this file describes.
- **`auto` mode:** follow the Auto path below.

## Step 1: Read the arguments

Parse `$ARGUMENTS`:

- `--quick` and `--deep` together: stop with `--quick and --deep conflict; pass one.`
- `--this-pr` and `--whole-stack` together: stop with `--this-pr and --whole-stack conflict; pass one.`
- `--all` or `--preset <name>` exist only in the deep review. They mean `--deep`. With `--quick`, stop with `--all and --preset belong to the deep review; drop --quick.`
- The target is the first integer or `#N`, else the first branch name, else the current branch. The word after `--preset` is its value, never a target. A branch name must pass `git check-ref-format --branch "<name>"`; pass it to `gh` as one quoted argument. It resolves to its open PR number with `gh pr list --head "<branch>" --json number -q '.[0].number'`; stop with `error: no open PR for branch <name>; create one first or pass a PR number` when there is none. With no target, resolve the current branch with `gh pr view --json number -q .number`; when it has no PR, review the local branch.
- Every other word goes to the review that runs, unchanged, with `--quick` and `--deep` removed. The review reads `--self`, `--no-post`, `--all`, `--preset`, `--auto` and `--ask` itself, and `--this-pr` and `--whole-stack` too, which Step 1b also reads.

A depth from `--quick`, `--deep`, `--all` or `--preset` is the user's choice: run Step 1b, then skip to Step 5. The scope is still asked with a depth flag.

## Step 1b: Stack scope

Only for a PR target. A local branch with no PR skips this step.

Intent: pr-stack. If the session's tool map lists it, use that command with the arguments as one quoted string: `"<PR> --json"`. Otherwise do this: detect the stack by hand as `skills/grounding-review/references/stack-review.md` section 1 describes. If both fail, or the JSON has `in_stack: false`, say nothing about stacks and go on to Step 2: the scope is `not a stack`.

If the given PR's `role` is `context` or `skip`, say so in one line and stop: there is nothing to review on it.

Otherwise print the same two summary lines as section 2 of `stack-review.md`. Line one: `Stack of <n> PRs (<open_count> open, <merged_count> merged), <open_lines> changed lines in the open PRs.` Line two names this PR's neighbours and its own diff base. Then pick the scope:

- `--this-pr` or `--whole-stack` in the arguments: that scope, no question.
- Auto mode: this PR only. Log `Assumed: this PR only (auto mode).`
- Otherwise ask: `1. This PR only (default). 2. Whole stack.` Accept `1`, `2`, or an empty answer for this PR only.

Merged PRs never count toward the recommendation and are never reviewed or commented on. Remember the scope for Steps 2 and 5.

## Step 2: Recommend a depth

Intent: review-suggest. If the session's tool map lists it, use that command with the PR number from Step 1 (omit it only for a local branch with no PR), `--json`, and `--scope stack` when Step 1b chose the whole stack. It reads git, and `gh` only for a PR number, then prints `recommendation` (`quick` or `deep`), `confidence` (`high` or `low`), up to four `reasons` and the `signals` it used. Otherwise do this: read `skills/grounding-review/references/review-routing.md` in this plugin and apply Part 1 by hand with plain `git` and `gh`. If the command fails (for example an older binary without `--scope` exits 2), do the same by hand; for the whole stack, Part 1 runs on the union of the open PRs' files.

With `high` confidence, go to Step 4.

## Step 3: Get a second opinion when the answer is unsure

Only for `low` confidence. Read `skills/grounding-review/references/review-routing.md` in this plugin and follow Part 2: one cheap `playbook:review-triage` pass, once. Its answer becomes the recommendation. If it cannot run, keep the first recommendation and say so in one line.

## Step 4: Choose

Print the recommendation first, in one line: `Recommended: <depth> (<confidence> confidence): <reasons joined in one short line>`.

**Q1:** "Which review? 1. <recommended depth> review (Recommended): <reasons, one line>. 2. <other depth> review." Ask in ask mode only, with the recommended depth first and marked `(Recommended)`. Accept `1`, `2`, `quick`, `deep`, or an empty answer for the recommendation. Whatever the user picks wins.

### Auto path

A depth from a flag in Step 1 stands, with no recommendation line. Otherwise take the recommendation without asking and print `Auto mode chose <depth>: <reasons in one line>`, so the choice is on record. The review that runs gets `--auto` and keeps its own auto rules. Auto mode does not skip a review's post question: only `--self` or `--no-post` does, so an unattended caller passes one of them.

## Step 5: Run the review

Load the review for the chosen depth with the Skill tool and give it the arguments from Step 1 (the PR number when there is one, plus every other word). Add `--this-pr` or `--whole-stack` for the scope from Step 1b, unless the user already passed one, so the review does not ask the scope again:

- Quick: `/playbook:quick-review`.
- Deep: `/playbook:deep-review`.

From here that review owns everything: the worktree, the reviewers, the sweep and the posting questions. Its report is the result. Add one line at the end naming the depth and scope that ran: `Depth: <depth> (<recommended | chosen by you | from a flag>), scope: <this PR | whole stack | not a stack>.`
