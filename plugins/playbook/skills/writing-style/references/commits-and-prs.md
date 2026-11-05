# Commits and Pull Requests

Read before writing a commit message, a PR title or a PR body. The dash, plain English, voice and banned-word rules in SKILL.md still apply, and so do `phrases` and `structures`. Intent: prose-check. If the session's tool map (printed at session start) lists it, run it on the message or body before you post.

## Commit messages

- Commit messages MUST use a conventional imperative subject under 73 chars (`type(scope): what`), one change per commit. Add a body only when the why isn't obvious; a sentence or two, or tight bullets.

## Pull requests

- PR descriptions MUST lead with one line of what and why; treat that as the target, not the floor. Add a short bulleted "what changed" only when the diff needs it to be reviewable; link to detail instead of inlining it. Aim for the gist in under 30 seconds, most PRs need fewer words than feel natural to write.

## When creating PRs

- Be concise: one sentence per reply, two at most (`LEN-REPLY`). The other limits are in SKILL.md under `Length limits`.
- For code changes done in response to feedback: "Done, <what was changed>."
- For questions: answer directly and briefly.
- For ticket creation: "Tracked in <TICKET-ID>." A ticket description follows `LEN-TICKET`.
- **Test Plan sections:** only include manual verification steps a reviewer cannot get from CI. Do NOT list CI results, test counts, or generic statements. If there are no manual steps, omit the Test Plan section entirely or write "N/A". Never write filler like "Covered by unit tests".
