# GitHub Review Comments and Replies

Read before posting a review comment, a reply to review feedback, or an issue or PR comment. These rules extend SKILL.md. Also read `review-examples` first; read `shell-quoting` before a `gh api -f body=` call.

## Review findings

- Review findings MUST follow `LEN-REVIEW` in SKILL.md: one sentence, two at most (the problem, then what breaks). A paragraph is the rare exception for a genuinely non-obvious mechanism, not the normal case.
- Use "we" and "this" in code review comments instead of "you", to stay non-confrontational.

## When reviewing (posting as a reviewer)

- **Trust the author.** Assume they know the language and the codebase. Don't teach concepts they use every day, don't rebuild context they already have, don't recap what the diff shows. Say the problem, not the background to it.
- **One sentence when possible, two at most: the problem, then why.** Use a direct causal template: "this won't work because X, do Y instead" or "this will cause X because Y." State the defect and its cause, then stop. A second sentence only when the mechanism is genuinely non-obvious (a subtle race, a data-loss path you can't see from the diff). A dev skims a review comment for the gist, not an essay: plainest words available, short sentences, active voice, no filler.
- **The `(blocking)` decoration already argues priority. Don't argue it again.** State the defect and stop; never add a clause about why it is worth fixing, what it would cost to ignore, or why now is the right time. "issue (blocking): this leaks the session token to the client" is enough. Not "issue (blocking): this leaks the session token, which matters because an attacker could hijack the session, and it costs nothing to fix now." The label carries the urgency; the sentence only needs the defect.
- **Avoid jargon.** Say things in plain words a teammate would use out loud. If a technical term is unavoidable, add what it means in a few words. Prefer "anyone can read another user's record" over "IDOR".
- **No word that could mean more than one thing.** If a term is ambiguous in context, use the plainer, unambiguous one instead.
- Assume the reader is a senior dev.
- Do NOT pad with blank lines or formatting. The label + the comment is enough.
- Lead with what you found, not with compliments. Skip "looks good" and "solid approach" openers.
- Don't write comparison reports. "X uses A while Y uses B. Works because C. Worth aligning to D." is a static-analyser pattern. State the risk ("this breaks if Z changes"), reference the other implementation as context, not as the other half of a symmetrical comparison.
- When suggesting a fix, use plain language ("consider changing X to Y", "change X to Y").
- **No fix in mind, say so.** State the problem and stop. A guessed-at fix you don't actually believe in is worse than naming a real gap and leaving the fix to the author.
- **A code snippet is a last resort**, only when the problem or the fix is too complex to explain in a sentence or two. Prefer pseudocode over real, runnable code: sketch the idea in a few lines, not a full implementation. A developer is attached to their own code, and a ready-made solution reads as presumptuous; pseudocode hands over the idea and leaves the implementation to them. Reserve real code for GitHub's own ```suggestion``` block, a literal, applicable diff for a mechanical fix, never for illustrating an approach.
- For nitpicks and minor suggestions, keep it short and plain. The label already says it is optional.
- Start each review comment with a Conventional Comments label in plain text, no bold. The format is defined in `playbook:grounding-review`, `references/conventional-comments.md`.
- **Nitpicks must be lightweight.** If the reason is obvious from the code, state the finding and stop. Don't trace the history of why the code exists.
- **Don't state implications the reader can draw themselves.** "It stands out now that every sibling has coverage" is obvious if you've already said "this is the only one without". Stop after the fact.
- **Don't explain what code does when the reader can see it.** State what's wrong, suggest the action (update or remove, pick one), stop.

## When replying to feedback (posting as PR author)

Reply drafts must read like a human typed them quickly in a code review thread.

- **One sentence is ideal. Two max (`LEN-REPLY`).** Three is too many. A paragraph is never acceptable in a code review reply. If you need two sentences, the second adds a fact, not a justification.
- **Skip the opener.** Go straight to substance. "Behind a feature flag for now." not "Good point. I'll put it behind a feature flag for now." Most replies need no acknowledgement at all. Praise-style openers in particular are immediate AI tells: NEVER lead with "Good catch.", "Nice catch.", "Great catch.", "You're right.", "Great point.", "Good point.", "Valid point."
- **Never paraphrase the reviewer's point back.** They just said it. Repeating it wastes space and sounds like a chatbot confirming receipt.
- **Code-change replies are especially terse.** When the reply is paired with a fix in the same push, the diff is the explanation. Default to one short sentence: "Fixed.", "Done.", "Ok, done." A brief locator tag is fine when it adds value ("Done, added (v || null) normalisation for notes."). NEVER rationalise the fix. NEVER restate the reviewer's finding before saying "fixed".
- **Only explain when you deviated.** If you applied the reviewer's suggestion verbatim, no rationale is needed. Spell out reasoning ONLY when (a) you took a different approach, (b) you applied part of a multi-option suggestion and deferred the rest, or (c) you intentionally disagreed. Even then, one clause, never a paragraph.
- **Acknowledgements must be terse when used.** One or two neutral words: "Ok.", "Fair point.", "My mistake.", "Will do.", "Noted." Vary across a batch.
- **No trailing hedges.** Don't end with "but good to be aware of", "worth keeping in mind", "something to watch for", "we can revisit if needed." Say the thing and stop.
- **Keep one register.** Reply in the same plain, direct English whatever the reviewer's tone.
- When explaining a design decision, state the reason in one clause. "They're distinct in Postgres. This is intentional." not a paragraph about trade-offs.
- When disagreeing, explain the reasoning respectfully. Still one sentence.
- **No slang.** Plain words only.

**Quote standalone comments:** When replying to non-threaded comments (review body comments, issue-level comments), prefix the reply with a blockquote of the original comment for context. Inline review thread replies don't need this since the platform threads them automatically.
