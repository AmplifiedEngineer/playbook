# Without the binary

Read this file only when the session's tool map lacks an intent. A recipe names the intent, and each section below is that intent's fallback. The heading is the intent id.

## run-mode

If `$ARGUMENTS` has both `--auto` and `--ask`, stop with one line: "--auto and --ask conflict; pass one." If it has `--auto`, the mode is `auto`. In every other case the mode is `ask`, the default.

## config-get

Use the defaults of the key the recipe names. For `fix.maxFiles` the default is 3, and for `fix.maxLines` it is 500.

## route-model

Spawn the agent the recipe names, with the model its agent file pins.

## pr-stack

Detect the stack by hand as `skills/grounding-review/references/stack-review.md` section 1 describes.

## review-suggest

Read `skills/grounding-review/references/review-routing.md` in this plugin and apply Part 1 by hand with plain `git` and `gh`. For the whole stack, Part 1 runs on the union of the open PRs' files. The same applies when the command fails, for example an older binary without `--scope` that exits 2.

## effort-resolve

Spawn the base agent with no `effort`, and say in your report that no ceiling was checked.

## effort-choose

Skip the automatic choice and use the effort table in the `playbook:delegating-subagents` skill.

## atlassian-check

The `playbook:atlassian` skill needs `acli` 1.3.39 or newer. Run `acli --version` and compare the `X.Y.Z` in `acli version X.Y.Z-stable` with 1.3.39. No output or no `X.Y.Z` means unreadable; a lower version means too old. Run `command -v twg` to see whether `twg` is present.

## pr-comments

Resolve the PR and fetch its comments by hand. The steps need `gh`.

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

## pr-reply

Check the file yourself: it must not be empty, hold an em or en dash, or carry AI attribution. Then post it from the file, which needs no shell quoting:

```bash
# Inline review-thread reply (the databaseId of the thread's first comment)
gh api -X POST "/repos/$OWNER/$NAME/pulls/$PR_NUMBER/comments/$DATABASE_ID/replies" -F body=@<that file> -q .html_url
# PR-level comment reply
gh api -X POST "/repos/$OWNER/$NAME/issues/$PR_NUMBER/comments" -F body=@<that file> -q .html_url
```

Shell-quoting gotcha for `gh api -f body=`: apostrophes in double quotes are literal (`"I don't"` is correct, NEVER escape to `"I don\'t"` which posts as `don'''t`). If the body contains complex quoting, write it to a temp file and use `-F body=@/tmp/reply.txt`.
