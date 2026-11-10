---
name: grounding-research
description: Evidence rules for investigating code (cite file:line). Use when exploring a codebase, tracing execution, or mapping dependencies. Not for PR review.
user-invocable: false
---

# Research Grounding

Discipline for code exploration, investigation, and producing findings reports. This skill adds the research-specific layer on top of the universal writing rules and the system prompt's `## Code` discipline (verify, don't guess; self-review), which stay in scope.

Every finding and digest this skill produces is prose a person reads, so the caller also applies the writing style rules for voice, banned words and prose.

## Citation Rules

Every claim about code MUST be grounded in a verifiable source:

1. Cite every claim with a specific file path and line numbers, URL, or documentation section. No unsourced assertions.
2. Open referenced files with the Read tool to confirm citations are accurate. Don't cite from memory.
3. Only reference files that exist. Use Glob to confirm paths when unsure.
4. Copy-paste exact code when citing. Don't paraphrase or reconstruct.
5. If you can't verify a claim, write `[unverified]` next to it.
6. If you can't find evidence, say "I could not verify this."

## Code search first

For any code question, search the index before Grep or Glob:

1. Run `playbook code status`. When it reports `stale` or `missing`, run `playbook code index` first (a full index of about 1,000 files takes about 1 s).
2. Run `playbook code context "<question>"` for an open question, `playbook code symbol <name>` or `playbook code refs <name>` for one name, `playbook code search <words>` for text, and `playbook code outline <path>` or `playbook code read <path> <start>:<end>` for one file. Open the file at the line it gives before you cite it (rule 2 above).
3. Use Grep and Glob only when the binary or the index is unavailable: `playbook` is not installed, or `code status` or `code index` fails (no `origin` remote, or `another index run is in progress` in a sibling worktree).

Agents without Bash get the `playbook code context` output in their brief.

## Findings Format

Each individual finding should use this shape:

```
### <category>: <subject>
**File:** <verified file path>:<verified line number>
**Evidence:** <exact code quote or documentation excerpt>

<analysis and discussion>
```

Valid categories: `finding`, `pattern`, `risk`, `recommendation`, `observation`

Keep findings focused. One finding per logical claim. If you find five issues in one file, that's five findings, not one finding listing five things.

## Findings Digest

After reporting individual findings, produce a summary digest:

1. **Hypotheses evaluated**: which hypotheses were confirmed, ruled out, or remain inconclusive, and why.
2. **Confidence levels**: rate each conclusion as high, medium, or low. High = verified in multiple files. Medium = verified in one file. Low = inferred but not directly verified.
3. **Blind spots**: what you could NOT verify, and what would be needed (e.g., "could not verify runtime behaviour without executing the test suite").
4. **Scope boundaries**: what was NOT investigated and why (out of scope, insufficient context, or time constraints).

## Known Rationalizations (Research)

| Rationalization | Reality |
|---|---|
| "The code suggests that..." | Read the code. Suggestions are not findings. |
| "It's reasonable to assume..." | Assumptions are not evidence. Verify or tag `[unverified]`. |
| "Based on the naming convention..." | Names lie. Read the implementation. |
| "This is a well-known pattern" | Show it in THIS codebase with file:line citations. |
| "The documentation states..." | Read the documentation file. Memory of docs is unreliable. |

## Boundaries

- Stay within the assigned research scope.
- Don't make claims about code without reading the actual files.
- Don't invent file paths, function names, or API shapes.
- Don't cite documentation from memory without verifying it still exists.
- Don't speculate about behaviour without evidence from the source code.

## Decision Gates

**Stop gates** (report what you know and what would unblock you):

- Scope ambiguity: the research question could reasonably mean more than one investigation direction.
- Conflicting evidence that materially changes the conclusion or requires a judgment call.

**Green lights** (proceed independently):

- Expanding search radius within the defined scope (checking additional related files).
- Following tangential leads that are clearly relevant to the research question.

## Voice

Write like a senior engineer sharing findings with their team: clear, well-sourced, to the point. Frame risks in terms of real-world impact, not abstract principles. Full voice rules in the `playbook:writing-style` skill.
