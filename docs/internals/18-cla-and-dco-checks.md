# CLA and DCO checks

A contributor signs the CLA by adding a row to `.github/CONTRIBUTORS.md` in a commit they author. The commit stays in the repository history, so the record moves with the repository to any Git host. No bot writes it, and no GitHub label or setting holds it.

## Where things live

- `.github/CONTRIBUTORS.md` is the register. The table sits between the `CLA-BOT:START` and `CLA-BOT:END` markers. A row is `| @login | YYYY-MM-DD | email |`. The email cell is optional. The marker names are kept so older tools can still read the table.
- `.github/workflows/cla-dco.yml` is the caller. It runs on `pull_request` and `merge_group`, with read-only permissions and no secret. It has no `issue_comment` trigger, so zizmor's `dangerous-triggers` audit has nothing to report and no ignore is needed.
- `.github/workflows/cla-dco-checks.yml` holds the two jobs, `dco` and `cla-check`. The caller job is named `cla-dco`, so the required checks keep their names: `cla-dco / dco` and `cla-dco / cla-check`.
- `scripts/cla-check.sh` makes both decisions from git history. `scripts/cla-check.test.sh` tests it against small repositories built on the fly.

## What `cla-check` verifies

The job checks out the pull request with `persist-credentials: false` and the full history. It asks the REST API for one thing: which account owns each commit. The script then reads only git objects.

1. The register on the default branch is the list of people who already signed.
2. Every non-bot commit author in `base..head` and the pull request author must be on that list, or sign in this pull request.
3. A pull request signs for someone only when a row it adds is introduced by a commit authored by that same person, and that commit message holds the sentence `I have read the CLA Document and I hereby sign the CLA.` Case and line wraps do not matter.
4. A pull request may not edit or remove an existing row. The register is append-only.

A person matches a row by login, by email, or by the numeric account id in a GitHub noreply address (`12345+login@users.noreply.github.com`). The id survives a login rename, and an email on the row survives a move to another host.

## Cases

- Bots: a commit or pull request from an account of type Bot, a name or login ending in `[bot]`, or an address or login in the caller's `extra-bots` input is skipped.
- Maintainers: no exemption. A maintainer is on the list like anyone else.
- Agents: an agent commits as its owner, so the owner's row covers it. AI co-authors are ignored (see Co-authors).
- Co-authors: see the next section.
- Squash merges: the merged commit carries the same row, and later pull requests read it from the default branch.
- Merge commits are skipped in both checks. They only combine commits that were checked on their own pull requests.
- Fork pull requests: the same, since the job needs only a read-only token.
- Renamed login: the account id and the optional email on the row still match. If neither is there, add a new row under the new login. Two rows for one person are allowed.
- No API data (another Git host): the script falls back to the commit email and noreply addresses. The row must then carry that email.

## Co-authors

Every line that starts with `Co-authored-by:` in any pull request commit (key in any case, many per commit, bot-authored commits included) names a person who must be signed, matched by email or by the account id or login in a noreply address. The same person as a commit author is checked once, as an author. A co-author who is not on the default branch list, and does not add their own row in a commit they author in this pull request, fails the check. The message names the person and says that they add a row in a commit they author. Their own pull request is enough.

A trailer that is not `Name <email>` fails closed with a clear message, unless it matches the ignore list.

AI tools and bots are ignored. The built-in list is `[bot]`, `noreply@anthropic.com`, `noreply@github.com`, and the words `github-actions`, `dependabot`, `renovate`, `claude`, `anthropic`, `copilot`, `chatgpt`, `openai`, `codex`, `gemini`, `cursor`, `devin`, `aider`, `windsurf`, `codeium`, `tabnine` and `sweep`. A pattern with `@` or `[` is a substring of the email or name. Any other pattern is a whole word in the name or email, so a person named Claude is also skipped. Treat the trailer as a declaration, not proof: anyone can write any name in it.

To add tools, set the `extra-ignored-coauthors` input (comma separated). To replace the whole list, set `ignored-coauthors`.

## Rollout

The register keeps every existing row, and the change adds an empty email cell to the table header. Anyone already listed passes with no action, so open pull requests do not lock out. A pull request opened before this change keeps its old check run until its next push. The first push after the merge runs the new check, and it passes for any listed author.

## Limits

A pull request that edits the workflow or the script runs its own copy. The `.github/` path and the repository default require maintainer review in `CODEOWNERS`, and that review is the control. The shared workflow used to be pinned to a commit in another repository, which a pull request could not change.

## Reuse in another repository

Copy `cla-dco-checks.yml`, `cla-dco.yml` and `scripts/cla-check.sh`, or call the workflow by commit from the organization `.github` repository. Inputs: `signatures-file`, `cla-path`, `statement`, `extra-bots`, `ignored-coauthors`, `extra-ignored-coauthors`, `script`, `tooling-repository` and `tooling-ref`. When a caller sets `tooling-ref` to a full commit SHA, the script is fetched from the tooling repository at that commit, so a pull request cannot change the checker it runs under. Without it the script is read from the caller repository, as here.
