# Orchestrator Reference

For the session that runs a review (`/playbook:review`, `/playbook:implement` Step 9). Lens reviewers and `cheap-checker` do not read this file. The finding block, labels, evidence and proof rules are in the skill itself.

## Review Report Format

Both `/playbook:review --quick` and `/playbook:review --deep` render this exact structure. The only difference: `/playbook:review --deep` includes the `### Reviewers` line; `/playbook:review --quick` omits it. The `·` separators are the middle dot U+00B7, not a dash.

### Report skeleton

```
## PR #<number>: <title>
<N> files · +<additions> -<deletions> · <VERDICT> · confidence <HIGH|MEDIUM|LOW>

### Overview
<1 to 3 sentences, human voice, why the verdict>
Sweep: <N> kept, <N> dropped, <N> relabelled, <N> moved

### Reviewers
<deep review and implement Step 9 only: lens roll-up with tier, e.g. "security: full (2) · docs: cheap-check (0) · perf: skip">

### Findings
<numbered finding blocks, ordered by resolved action: must-fix first (anything marked blocking, then issue, chore, todo, typo unless marked non-blocking), then question, then suggestion, then the optional ones (polish, thought, nitpick, anything marked non-blocking)>

### Verification Summary
<the table and confidence line from the skill's Verification Summary section>

Verdict: <APPROVE | REQUEST_CHANGES | COMMENT | INCONCLUSIVE> · confidence <HIGH|MEDIUM|LOW>
```

## Verification Sweep (MUST)

After the first review pass produces its findings, and before the list is shown, posted or acted on, the orchestrator runs one more sweep over every finding, against the code at the reviewed head. The orchestrator does this itself: reviewer subagents are read-only and can be wrong, so the reviewer that wrote a finding never checks it.

Check three things for each finding:

1. **True.** Re-read the cited lines. Trace the failure scenario where that is cheap; run it only in a flow that already runs the PR's code behind its own warning (the deep pass). A finding tagged `[unverified]` is either confirmed, dropped, or kept with the `[unverified]` tag stated plainly.
2. **Label.** The label and decorations (see `conventional-comments.md`) match the real impact.
3. **Anchor.** The file and line are right. In a stacked or multi-PR review, the finding sits on the PR or branch that owns the code.

Drop a finding that does not hold, relabel one with the wrong label, and move a misplaced one. The posting step drafts from the swept list, so what it posts carries the label and anchor the sweep settled. Read the cited lines plus what the trace needs, never whole files, to keep the main context small.

Then report what the sweep changed in the `Sweep:` line under the Overview: how many findings were kept, dropped, relabelled, and moved. A finding that survives counts as kept, even when it was also relabelled or moved. Then recompute the verdict, confidence and finding order from the swept list, since drops, relabels and moves can change all three. Nothing is shown, posted or acted on until the sweep has run.

## Subject Lines

The subject is the first thing the author reads. Describe the consequence or situation, not a rule or label. Write it the way you'd summarise the issue to a colleague in one line.

| Bad (scanner output) | Good (human summary) |
|---|---|
| SQL injection via string interpolation | User input gets executed as SQL |
| N+1 query pattern detected | Each order fires a separate query |
| Missing error case tests | Error paths aren't covered yet |
| PII logged in plain text | User email ends up in log aggregator |
| Service imports Express type | Service is coupled to Express |
