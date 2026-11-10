---
description: Use when a PR has unresolved review comments. Walks them one at a time, applies fixes or drafts replies, then posts the replies.
allowed-tools: Bash, Read, Edit, Write, Grep, Glob, Skill, AskUserQuestion
argument-hint: "[PR number] [--bots] [--dry-run] [-y|--yes] [--auto] [--ask]"
model: opus
effort: medium
---

# Address PR Comments

Iterate through unresolved review-thread comments and PR-level comments on a pull request. For each one: read the code, propose a fix or reply, get user approval, apply the edit (or post the reply), and move on. At the end, hand off to `/playbook:commit-and-push -A` and then post any queued thread replies.

## Step 0: Read the run mode

Do this first. Read the run mode. Intent: run-mode. If the session's tool map (printed at session start) lists it, use that command with this command's arguments, and follow the `run-context` reference of the `playbook:delegating-subagents` skill for its warning, exit 2 and failures. Otherwise do this: if `$ARGUMENTS` has both `--auto` and `--ask`, stop with one line: "--auto and --ask conflict; pass one." If it has `--auto`, the mode is `auto`. In every other case the mode is `ask`, the default.

If the mode is `auto`, stop here. Print one line and nothing else: "/playbook:address-pr-comments needs a person to run it, because it replies on GitHub in your name and each reply needs your approval." Do not run any later step.

In `ask` mode, behave exactly as this file describes.


## Discipline: receiving review feedback

Code review is technical evaluation, not emotional performance. Apply this loop to every comment before deciding fix/reply/skip:

1. **Read** the comment without reacting.
2. **Understand** the ask. Restate it in your head; if unclear, surface that to the user before acting.
3. **Verify** the claim against the actual code (use `Read`). Reviewers and bots can be wrong.
4. **Evaluate** whether the suggestion is right for THIS codebase, not in the abstract. Check for legacy reasons, YAGNI (grep for callers if the suggestion is "add feature X"), and conflicts with prior architectural decisions.
5. **Decide**: fix, reply explaining, push back with technical reasoning, or ask the user.
6. **Apply** one item at a time. Don't batch-edit and discover regressions later.

Push back when the suggestion breaks existing functionality, the reviewer lacks context, it violates YAGNI, or it's technically wrong for the stack. Push back with technical reasoning, not defensiveness. If you can't verify a claim without extra work, say so in the proposed action ("I can't verify this without running X; investigate, ask, or skip?").

Forbidden internal monologue and forbidden in draft reply text:

- "You're absolutely right" / "Great point" / "Good catch" / "Nice catch" / "Thanks for catching that"
- "Let me implement that now" (before verification)
- Any gratitude expression. The diff is the acknowledgement.

## Discipline: writing the reply

Invoke the `playbook:writing-style` skill before drafting any reply. It governs voice, banned words, banned openers, reply brevity (`LEN-REPLY`: one sentence ideal, two max, a paragraph is never acceptable), active voice only, and the GitHub-specific rules (no commit hashes in reply text, no markdown emphasis in inline comments, no em or en dashes). The GitHub-specific rules live in its references: read `github-review-comments`, `github-prohibited`, `review-examples`, `phrases` and `structures` (each is `references/<name>.md` in that skill's folder). Load them once at the start of Step 4 and apply it to every reply in the loop; do not re-derive these rules from memory.

Shell-quoting gotcha for `gh api -f body=`: apostrophes in double quotes are literal (`"I don't"` is correct, NEVER escape to `"I don\'t"` which posts as `don'''t`). If the body contains complex quoting, write it to a temp file and use `-F body=@/tmp/reply.txt`.

## Argument parsing

Parse `$ARGUMENTS` token-by-token:

- Integer or `#<integer>` -> explicit `PR_NUMBER`.
- `--bots` -> `INCLUDE_BOTS=true` (default: skip CodeRabbit, Copilot review, Greptile, github-actions, etc).
- `--dry-run` -> `DRY_RUN=true` (list everything but never edit files, never post replies, never commit).
- `--yes` or `-y` -> `AUTO_COMMIT=true` (skip the final "proceed with commit?" gate; per-comment gates still apply).
- `--auto` or `--ask` -> the run mode, already read in Step 0.
- Empty or unmatched -> resolve PR from the current branch.

## Execution rules

1. Run every bash block for real with the `Bash` tool. Don't simulate.
2. Read the file at the PR's head SHA before proposing a fix. The reviewer's `line` field refers to the post-diff line number.
3. Combine independent `Bash` calls into a single tool call.
4. **NEVER resolve threads.** That's the reviewer's call. Reply only.
5. Never auto-commit until every queued comment has been triaged (or skipped) and the user has reviewed the staged diff.
6. If `--dry-run`, stop after Step 3 with a printed plan.
7. Skip bot authors unless `--bots`. Bot login list: `coderabbitai`, `coderabbitai[bot]`, `copilot-pull-request-reviewer[bot]`, `greptile-apps[bot]`, `github-actions[bot]`, `sonarqubecloud[bot]`, `codecov[bot]`, `dependabot[bot]`, `renovate[bot]`.

## Step 1: Resolve PR and capture context

Resolve the PR and fetch its review threads and PR-level comments. Intent: pr-comments. If the session's tool map lists it, use that command with `"$ARGUMENTS"`. It parses the flags and the PR number, resolves the PR from the current branch when none is given, and prints `PR: owner/name#N`, `Head SHA:`, `Me:` and `Flags: bots=... dry-run=... auto-commit=...`. It exits 1 with `error: no PR for current branch; pass a PR number` when it cannot resolve a PR. It writes both sources to `/tmp/pr-comments-<PR>-threads.json` and `/tmp/pr-comments-<PR>-issues.json`. Otherwise do this:

1. `PR_NUMBER`: the number from the arguments, else `gh pr view --json number -q .number`. If neither gives one, stop with `error: no PR for current branch; pass a PR number`.
2. `OWNER/NAME`: `gh repo view --json nameWithOwner -q .nameWithOwner`. `HEAD_SHA`: `gh pr view "$PR_NUMBER" --json headRefOid -q .headRefOid`. `ME`: `gh api /user -q .login`.
3. Write the review threads to `/tmp/pr-comments-<PR>-threads.json`:

   ```bash
   gh api graphql -F owner="$OWNER" -F name="$NAME" -F pr="$PR_NUMBER" -f query='
     query($owner: String!, $name: String!, $pr: Int!) {
       repository(owner: $owner, name: $name) {
         pullRequest(number: $pr) {
           reviewThreads(first: 100) {
             nodes {
               id isResolved isOutdated path line originalLine
               comments(first: 50) {
                 nodes { id databaseId author { login } body url createdAt }
               }
             }
           }
         }
       }
     }' > "/tmp/pr-comments-$PR_NUMBER-threads.json"
   ```

4. Write the PR-level comments to `/tmp/pr-comments-<PR>-issues.json` with `gh api "/repos/$OWNER/$NAME/issues/$PR_NUMBER/comments" --paginate`.

Capture `PR_NUMBER`, `OWNER`, `NAME`, `HEAD_SHA`, `ME`, and the three flag values. You need them for every later step.

## Step 2: Fetch unresolved review threads and PR-level comments

Step 1 already fetched both sources: `/tmp/pr-comments-<PR>-threads.json` holds the inline review threads with a `path` and `line`, plus the GraphQL ids and the first comment's `databaseId` needed for REST replies, and `/tmp/pr-comments-<PR>-issues.json` holds the PR-level issue comments, with no path or line.

Now filter in Claude (not bash) so you can reason about each thread:

- Skip threads where `isResolved == true`.
- If `INCLUDE_BOTS=false`, skip comments whose author login is in the bot list above.
- Outdated threads (`isOutdated == true`, or `line == null`) MAY have valid feedback that no longer maps to a line. Surface them with an `(outdated)` tag and let the user decide.
- For each remaining thread, the **last** comment is usually the most recent ask. Show the whole thread but treat the latest comment as the prompt.

## Step 3: Display triage summary

Print one block summarising what was found, then list each thread/comment with an index. Keep it compact:

```
Found N unresolved review threads, M PR-level comments (K bot comments suppressed).

Threads:
  [1] src/foo.ts:42  alice: "this allocates on every call, can we cache?"
  [2] src/bar.ts:103 bob (outdated): "rename to fooBar?"
  [3] src/baz.ts:7   alice: "missing null check"

PR-level:
  [4] charlie: "tests please"
```

If `DRY_RUN=true`, stop here. Print "dry-run: no edits applied, no replies posted."

## Step 3b: Pick what to work on

With one item or none, skip this step. Otherwise ask **Which comments to work on?** Read `skills/grounding-review/references/pick-list.md` in this plugin and follow it with `AskUserQuestion` checkboxes (typed list if absent), using the Step 3 indexes, every item selected. An item left out is treated as **Skip**: Step 4 does not visit it and the final summary lists it, so nothing slips through silently. `--dry-run` never reaches this step.

## Reading and writing Conventional Comments

Read `playbook:grounding-review` `references/conventional-comments.md` once before Step 4. Reviewers may write comments as `<label> [decorations]: <subject>`. Read them like this:

- Must fix by default: `(blocking)`, `issue`, `chore`, `todo`, `typo`.
- Optional: `nitpick`, `thought`, `polish`, `(non-blocking)`.
- Needs an answer: `question`.
- Judge it and fix or decline with a reason: `suggestion`.
- A bare `blocking:` from an older comment reads as `issue (blocking)`. A comment with no label is judged on its text.

When a reply is a review comment, write it in the same format, for example `question: ...`. A plain reply such as "Fixed, pushed." stays plain.

## Step 4: Iterate one at a time

For each item picked in Step 3b (every item when that step was skipped), do this loop:

1. **Show context.** Print the file path, the comment author, the full body, and the URL. Then `Read` the file around the line (10 lines either side).
2. **Verify the claim.** Does the code actually do what the reviewer says? Read enough to be sure. If not, you're going to draft a reply rather than a fix.
3. **Choose an action and present it to the user:**

   - **Fix**: propose a concrete diff. Show the diff snippet before applying.
   - **Reply**: draft a one-or-two-sentence reply (no fix needed). Show the reply text.
   - **Both**: apply a fix AND queue a short reply ("Fixed, pushed.") that is posted once the commit lands.
   - **Skip**: neither fix nor reply. Use sparingly. Skipped items get listed in the final summary so nothing slips through silently.

4. **Get user approval.** Ask `[F]ix / [R]eply / [B]oth / [S]kip / [Q]uit / [E]dit-then-fix`. Wait for the answer.

   - `Edit-then-fix` means: user wants to write a different fix than what you proposed. Wait for them to describe it, then apply.
   - `Quit` means: stop iterating, jump straight to Step 5 with what you have so far.

5. **Apply.** The approved item is fully decided, so apply it yourself; this command spawns no agent.

   - **For Both, apply the fix half now and queue the reply text for Step 6.** The reply half of a **Both** action is NEVER posted here: only its content is decided now. Step 6 posts it after the commit, once the push has succeeded.
   - For **Fix**, or the fix half of **Both**: apply the approved diff with `Edit`. Read the changed lines back and print the applied hunk to the user immediately, before advancing to the next indexed item.
   - For **Reply only** (no fix): write the approved reply text to a file with `Write` (for example `/tmp/<repo>/address-pr-comments-<PR_NUMBER>-item-<N>.md`) and post it:
     Intent: pr-reply. If the session's tool map lists it, use that command with the PR number, the file, and either the thread's first-comment `databaseId` or the PR-level option. It posts the file verbatim, refuses an empty body, an em or en dash and AI attribution, and prints `Posted: <url>` and the body. Otherwise do this: check the file yourself for those three problems, then post it from the file, which needs no shell quoting:
     ```bash
     # Inline review-thread reply (the databaseId of the thread's first comment)
     gh api -X POST "/repos/$OWNER/$NAME/pulls/$PR_NUMBER/comments/$DATABASE_ID/replies" -F body=@<that file> -q .html_url
     # PR-level comment reply
     gh api -X POST "/repos/$OWNER/$NAME/issues/$PR_NUMBER/comments" -F body=@<that file> -q .html_url
     ```
     Print the posted body to the user before advancing to the next indexed item.
   - If an edit does not apply or the post fails, surface the error to the user plainly and mark the item `failed` in the tracked state below, never `fixed`/`replied`.

6. **Track state.** Keep a running markdown list, one row per indexed item, with status `fixed | replied | both-queued | failed | skipped`. This is the audit trail.

## Step 5: Pre-commit confirmation

Print a summary:

```
Triaged N items: A fixed, B replied, C both-queued, D skipped, E failed.
Staged changes:
  <git diff --stat output>
```

Run `git diff --stat` to show what changed locally. The commit follows `/playbook:commit-and-push`: when the branch has a ticket key, the scope carries it (`fix(PROJ-123): ...`), and on a stacked PR that is required. If `AUTO_COMMIT=false`, ask the user `Proceed with commit-and-push? [Y/n]`. If the user says no, exit with the changes left in the working tree (do not stage, do not commit). Print the list of queued replies they'll need to post manually.

## Step 6: Commit, push, post queued replies

If approved (or `AUTO_COMMIT=true`):

1. Invoke the `commit-and-push` skill with the `-A` flag and an extra hint that the commit message should reference the PR (e.g. "address review comments on #<PR_NUMBER>") and the ticket key read from the scope of the PR title, if it has one. Pass it with the worktree root and branch in its prompt ("Worktree root: <path>, branch: <name>") when you work in a git worktree. The skill handles staging, formatting, message generation, rebase, and push. Capture the resulting commit SHA from the skill's output.

2. For each `both-queued` reply, write the reply body to a file and post it as a thread reply, the same way Step 4 sub-step 5 posts an immediate reply. Print the posted body before moving to the next queued reply. On a failure, apply the SAME rule Step 4 sub-step 5 uses: surface it to the user plainly, mark the item `failed` (not `both-queued` anymore), and do not count it toward "posted P queued replies" below.

3. Print a final summary:
   ```
   Done. Committed <SHA>, posted P queued replies, skipped D items, F failed posts.
   Skipped items needing follow-up:
     - <thread URL> author: comment-summary
   Failed posts needing follow-up:
     - <thread URL> author: reason
   ```

## Notes

- **Threads with multiple comments.** A thread can have a back-and-forth. Display all comments in chronological order but treat the latest non-author comment as the prompt. If the latest comment is from `$ME` (you replied earlier), surface that and let the user decide if there's anything still pending.
- **No silent edits.** Every file change must be visible to the user before the next iteration. If a file is edited, print the resulting hunk.
- **Failure handling.** If a reply POST fails (404 on the thread, 403 if you're not a collaborator), don't retry blindly. Print the error, leave the reply in the queue, and continue with the rest. Surface the failures in the final summary so the user can post them manually.
- **Resuming after Quit.** If the user quits mid-iteration, print the remaining indexed items with their URLs so they can re-run `/playbook:address-pr-comments` later (the resolved/unresolved state on GitHub is still the source of truth).
