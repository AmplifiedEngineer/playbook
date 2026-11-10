---
name: grounding-review
description: Evidence and labeling rules for reviewing a PR or diff. Use for any quick or deep code review. Not for investigation.
user-invocable: false
---

# Review Grounding

Discipline for reviewing pull requests: how to ground, label, and report findings.

`/playbook:review` picks the depth through `pr review triage` and reads `references/quick-pass.md` or `references/deep-pass.md` in this skill's folder.

The session that runs the review also reads `references/orchestrator.md` in this skill's folder for the report skeleton, the verification sweep and the subject line examples. A lens reviewer or `cheap-checker` skips it: the orchestrator sweeps your findings, you do not.

Findings stay plain: label first, one or two sentences, no hedging, no em or en dashes. The posting step of each review pass loads the writing style skill to draft the comment bodies from the swept findings, not to produce or check findings.

**One register.** A posted review comment uses the same simple, plain, direct English as chat. The rules come from `playbook:writing-style`, applied at the posting step. There is no warmer register for posted comments.

## Voice for Reviews

You're a senior engineer leaving a review for a teammate. Simple, direct sentences. Short words over long words. No idioms, no fancy vocabulary.

- **Always explain why,** but in a clause, not a paragraph. Every finding answers "why does this matter?" by naming the real-world consequence (what breaks, who's affected, what happens in production). The why is *part of the one-sentence finding*, not a separate paragraph after it.
- Use "we" and "this" instead of "you" and "your code". "this could lead to..." not "you should...".
- **Skip the praise.** Don't open with compliments. Jump straight to what you found.
- For `(blocking)` comments, be clear and direct. For suggestions, name the change: "consider changing X to Y".

Full voice rules for posted comments are in the `playbook:writing-style` skill.

## Evidence Rules

1. Open the file with Read before making claims. The diff alone isn't enough.
2. Copy-paste exact code into the `Evidence` field. Don't paraphrase or reconstruct.
3. Confirm every name you reference (functions, tables, variables, types) by reading the source.
4. Only reference file paths that exist. Use Glob if unsure.
5. Confirm line numbers by reading the file. Leave them out if you can't confirm.
6. If you can't verify, write `[unverified]`.

## Proof Ladder

Different claims require different levels of proof:

| Claim Type | Required Proof |
|---|---|
| Code defect | File path, line number, exact code snippet exhibiting the defect. |
| Performance concern | Concrete scenario: loop iteration count, query count, data volume, measured timing. |
| Failure scenario | Step-by-step sequence: trigger, mechanism, observable failure. |
| Comparative claim ("X is better than Y") | At least one measurable dimension with data (call count, coupling surface, lines affected). |
| TypeScript compile error | Exact TS mechanism (excess property check, missing property, narrowing failure) AND whether it flows through a generic (`.map()`, `right()`, `Promise.resolve()`) that bypasses it. If unsure, tag `[unverified]`. |

Claims without supporting proof MUST be tagged `[unverified]`.

## Finding Block

The orchestrator's prompt names the output shape. A finding in the review report looks like this:

```
N. <label> [decorations]: <one-line subject naming the consequence>
   `<file>:<line>` · <category> · <HIGH|MEDIUM|LOW>
   Evidence: `<exact code at that line>`
   <1 sentence body when possible, 2 at most: the problem and the failure it causes>
   Fix: <one fix, in plain words>
```

- Labels: the nine Conventional Comments labels, with decorations, as defined in `references/conventional-comments.md`. Write `<label> (<decorations>): <subject>`, for example `issue (blocking): ...`. The label and decoration set the finding order (see Findings in the skeleton) and the report subject; the posted comment uses the exact same text.
- Subject names the consequence, not a rule: "user input runs as SQL", not "SQL injection".
- Location line: `` `file:line` `` then category (security, logic, perf, tests, types, data, maintainability, and so on) then confidence, `·`-separated.
- Body: 1 sentence when possible, 2 at most. State the problem plainly, skip restating what the code already shows (the reader can see the diff). The why is part of the sentence, not a separate paragraph. No bullet lists inside a finding.
- `Evidence:` the exact code, copied, so the sweep can re-check the finding from its cited lines alone.
- `Fix:` one fix, not a menu, the smallest change that works.
- No comment body here. A finding is plain; the posting step of the review command drafts each GitHub comment from the swept finding and adds it as a `Draft:` block under the finding. Reviewers never write that block.
- Report-only finding (evidence not on a changed diff line, so no inline anchor): end with `Report-only: not on a changed line, no inline comment.` The posting step skips it.

## Conventional Comments

Review comments follow [Conventional Comments](https://conventionalcomments.org/): `<label> [decorations]: <subject>`, then an optional blank line and discussion. Read `references/conventional-comments.md` only when you write or read code review comments, and skip it otherwise. It holds the canonical definition, the label table, the decoration table, the scope rule and two full examples.

## Evaluation Categories

Not every category applies to every diff. Focus on what's relevant; skip categories where the change has no meaningful impact.

- **Security:** references/security.md
- **Performance:** references/performance.md
- **Reliability:** references/reliability.md
- **Maintainability:** references/maintainability.md
- **Functionality / Correctness:** references/correctness.md
- **Architecture:** references/architecture.md
- **Scope Control:** references/scope-control.md

Each path above is relative to THIS skill's own directory, the path the Skill tool reports as "Base directory for this skill" when `playbook:grounding-review` is invoked, not relative to the caller's working directory or any target repo being reviewed.

A lens-scoped reviewer dispatched by `/playbook:review --deep` or `/playbook:implement` does not locate its reference file itself. A `cheap-checker` never invokes this skill and reads only the resolved path it is handed. The orchestrator resolves the matching file to an absolute path instead, from the base directory the Skill tool reported when it invoked this skill (the lens table is in `references/triage-tiers.md`), and hands that agent the resolved path. This is necessary because a reviewer cannot look up that base directory itself and `Read` needs an absolute path, and `Read` requires an absolute path either way.

An unscoped reviewer (the single pass of the quick review) invokes this skill directly, reads the base directory the Skill tool reports, and combines it with each relative path above to read all 7.

## Known Rationalizations (Review)

| Rationalization | Reality |
|---|---|
| "The file probably exists at..." | Verify with Glob or Read. Probably is not evidence. |
| "Based on the pattern, it should be..." | Pattern matching is not proof. Check the actual file. |
| "This is a common anti-pattern" | Common to whom? Show the specific code that exhibits it. |
| "The function likely does X" | Read the function. Likely is not verified. |
| "I can see from the diff that..." | The diff shows changes, not the full file. Read the source. |
| "This is obviously a bug" | Show the failure scenario: trigger, mechanism, observable failure. |
| "Based on my experience..." | Your experience is not evidence in this codebase. Cite the file. |

## Verification Summary

Every review MUST end with a Verification Summary table.

```
## Verification Summary

| File | Read | Lines | Findings |
|---|---|---|---|
| src/dao/UserDao.ts | Yes | 12, 15, 42 | #1, #3 |
| src/auth/login.ts | Yes | 4, 8 | #2 |
| src/utils/hash.ts | No | - | - |

Confidence: HIGH | All findings verified against source files.
```

**Confidence levels:**

- **HIGH**: every finding has tool-verified evidence. All file paths confirmed via Read/Glob.
- **MEDIUM**: most findings verified, but 1-2 rely on diff-only evidence (tagged `[unverified]`).
- **LOW**: multiple findings lack verification. MUST tag each as `[unverified]`.

A LOW confidence review is better than a fabricated HIGH confidence review.

## Boundaries

- Stay in your lane: only report findings within your area of expertise.
- Don't duplicate findings other specialist reviewers would cover.
- Don't invent file paths, function names, table names, or variable names.
- Don't cite abstract rules without explaining the real-world consequence.
- When reviewing dependency injection: verify constructor parameters use the service interface, not extracted raw functions. Flag raw function injection where an existing service or interface provides the method.
- Cite a commit by SHA only when the finding is about that commit. Text posted to GitHub never names one.
