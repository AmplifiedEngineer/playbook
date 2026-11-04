---
description: Use when recording a significant, hard-to-reverse architectural decision, or choosing between named options that are expensive to undo. Writes a fact-checked ADR to docs/adr/.
allowed-tools: Bash, Read, Grep, Glob, Write, Edit, Agent, SendMessage, TaskStop, Skill
argument-hint: "<topic> [--record-only] [--list] [--auto] [--ask] [--help]"
model: opus
effort: medium
---

# ADR: Decision Records with Execution Blueprints

Create a formal Architecture Decision Record (ADR) with an optional execution blueprint: an archivable decision record paired with an actionable implementation plan. The blueprint is its implementation plan.

Invoked as `/playbook:adr`. The remaining arguments are the topic and flags.

## Defaults

No external config; these are fixed:

- **Document:** Architecture Decision Record (ADR).
- **Directory:** `docs/adr/`, tracked in git, created on first use. A decision record is shared history, so it belongs in the repo, not in a git-ignored scratch dir.
- **Filename:** `NNNN-{kebab-title}.md` (zero-padded sequence + kebab title). The date lives in the record's own front matter, not the filename.
- **Companion files:** blueprint at `{base}-blueprint.md`, quality report at `{base}-quality.md`, same directory.

## Help

If the arguments contain `--help`, print this and stop:

```
/playbook:adr - Architecture Decision Records with execution blueprints

USAGE:
  /playbook:adr <topic> [options]

OPTIONS:
  --help         Show this help
  --record-only  Write the decision record without an execution blueprint
  --list         List existing ADRs in docs/adr/
  --auto         Run unattended (this command stops, it needs a person)
  --ask          Force the interactive mode

NOTE: every ADR should have a companion execution blueprint. Use --record-only
only when the blueprint comes in a follow-up session.

EXAMPLES:
  /playbook:adr "replace polling with WebSocket push"
  /playbook:adr "split monolith order service"
  /playbook:adr --record-only "deprecate v1 API"
  /playbook:adr --list

Records save to docs/adr/ (tracked) as NNNN-{kebab}.md.
```

## Step 0: Read the run mode

Do this first, after the `--help` check above. Resolve whether this run is `ask` or `auto`. Intent: run-mode. If the session's tool map (printed at session start) lists it, use that command with this command's name (`adr`) and `$ARGUMENTS`, and follow `../skills/delegating-subagents/references/run-context.md` (relative to this file) for its output: the warning, the effort ceiling, a conflicting-flags exit, and any other failure. Otherwise do this: the mode is `auto` when the arguments carry `--auto` and `ask` when they carry `--ask`. When both are given, stop with one line saying they conflict. With neither, run `printenv PLAYBOOK_MODE` and use its value when it is `ask` or `auto`; in every other case the mode is `ask`. There is no effort ceiling on this path.

If the mode is `auto`, stop here. Print one line and nothing else: "/playbook:adr needs a person to run it, because an ADR records a decision that a person has to make and own." Do not run any later step.

In `ask` mode, behave exactly as this file describes.

**Routing.** Where you choose what to spawn, pick the model for a `design` task. Intent: route-model. If the session's tool map lists it, use that command for the `design` kind and follow its `action` under the `routing.escalate` gate: on `ask` stop and ask the user, on `downgraded` (`deny`) use the lower tier it names, and in auto mode (`auto`) proceed and log the assumption. If the command fails, carry on. Otherwise do this: spawn each agent on the model its own agent file sets, and carry on.

## Execution Rules (MUST)

1. **Execute every bash block for real with the `Bash` tool.** Don't simulate, summarise, or predict output; use the actual output to drive the next step.
2. **No caching.** Every invocation is a fresh run. Don't reuse results from prior conversations or training data.
3. **No skipping.** Execute steps in order. The only exception: steps guarded by a flag the user didn't set.
4. **No assumptions.** Don't guess file contents, command output, or environment state. Run the command and read the result.
5. **Follow the command's gates, not your own.** If a step says "ask the user", ask. If it doesn't, don't add a confirmation gate.
6. **Show real data.** Tables and reports are populated from actual command output, never placeholders.

## Flag Handling

- `--list`: list existing records in `docs/adr/` (filenames + titles). If none exist, say so. Then stop.
- `--record-only`: run Stages 1-2 but skip the execution blueprint.

If no flags match, run the full workflow with the remaining text as the topic.

## Load Writing Discipline (MUST, before any drafting)

Invoke the `playbook:writing-style` skill (voice, banned words, prose rules) and the `playbook:grounding-research` skill (evidence and citations). Every ADR title, context line, alternative, and rejection note MUST follow them. ADR-specific rules on top:

- **Data over opinion.** Support claims with file paths, metrics, query counts, or concrete scenarios.
- **Spartan and informative.** Every sentence adds information. Cut sentences that only add emphasis.

## Stage 1: Investigate (mandatory)

Build understanding before writing. Skipping this produces records that don't survive contact with the codebase.

1. **Read existing records** in `docs/adr/` (if it exists) for precedent and numbering.
2. **Explore the codebase** with Read/Glob/Grep: modules and services affected by the topic, database schemas and migration history (if relevant), test patterns, configuration and deployment.
3. **Read memory stores if present** (optional enhancement, not required). `<owner>/<repo>` comes from `git remote get-url origin` with the scheme, host and `.git` suffix stripped. Intent: memory-context. If the session's tool map lists it, use that command for `<owner>/<repo>`; it reads the memory graph itself and prints a `Memory context: none` line saying why when it has nothing. Otherwise do this: read the `name:` and `description:` frontmatter lines of the fact files in `~/.config/playbook/memory/<owner>/<repo>/` and of the `*.md` files directly in `~/.config/playbook/memory/` and in `~/.config/playbook/memory/<owner>/`; any of these folders may be missing, and no facts in either counts as none. Then load the relevant fact files on demand. Use what you find to inform Considered Alternatives (reference a named pattern where one applies) and to avoid re-proposing something already rejected. Surface any `contradicts` edge among the returned facts that bears on the decision, rather than silently choosing one side. Note in the digest whether facts were found. If there are none, skip this step silently and proceed on the codebase alone.
4. **Summarise findings to the user:** what's relevant to the topic, which areas are affected, existing patterns/constraints, and applicable patterns from memory if a memory store was present (with brief rationale).

**Knowledge capture:** if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, write any durable convention or gotcha revealed by exploration as a project memory fact now. If no project store is present, skip this step silently.

Wait for the user to acknowledge before Stage 2.

## Stage 2: Draft

### Determine the filename

Find the next record number and today's date. Intent: next-adr-number. If the session's tool map lists it, use that command; it creates `docs/adr` if needed and prints `Next number: NNNN   Date: YYYY-MM-DD`. Otherwise do this:

```bash
d="$(git rev-parse --show-toplevel)/docs/adr"
mkdir -p "$d"
n=$(ls "$d" | grep -E '^[0-9]{4}' | cut -c1-4 | sort -n | tail -1)
printf 'Next number: %04d   Date: %s\n' "$((10#${n:-0} + 1))" "$(date +%F)"
```

The number is one above the highest four-digit prefix in `docs/adr`, or `0001` when there is none. Gaps are not filled.

Filename: `{NEXT}-{kebab-title}.md` in `docs/adr` at the repo root. Write the record directly there (never to a temp/local scratch path).

### Decision Record

Write `{DIR}/{filename}` with Status **Proposed**, using this structure:

```markdown
# ADR-{NNNN}: {Title}

- **Status:** Proposed
- **Date created:** {YYYY-MM-DD}
- **Date modified:** {YYYY-MM-DD}

## Context
{Evidence-grounded background: file paths, metrics, concrete observations from Stage 1.}

## Decision Drivers
- {driver referencing concrete evidence}

## Considered Alternatives
### {Alternative} (effort: S | M | L | XL)
- {how it works}
- Trade-offs: {pros and cons}

## Decision
{The chosen alternative and why. State explicitly why each other alternative was rejected.}

## Consequences
- {positive, negative, and follow-up consequences}

## Architecture Diagrams
{Mermaid: a current-state diagram and a proposed-state diagram, plus sequence/state/ER diagrams as warranted.}
```

Requirements:

- At least 2 alternatives beyond the status quo (3 total minimum), each with a genuine effort estimate (S/M/L/XL) and real trade-offs.
- Decision drivers reference concrete evidence from Stage 1.
- The Decision section gives the reasoning for rejecting each alternative.
- **Diagrams:** keep them readable, label nodes meaningfully, pick the right type (flowchart for components, sequence for interactions, state for lifecycles, ER for schemas). Always include current-state and proposed-state.

**Knowledge capture:** if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, record the decision and each rejected alternative as memory facts (so future planning, including `/playbook:plan`, doesn't re-propose them). If no project store is present, skip this step silently.

Present the draft. Revise in place on feedback. Repeat until the user explicitly approves.

### Execution Blueprint (skip if `--record-only`)

After the record is approved, write `{DIR}/{base}-blueprint.md` (`{base}` = filename without extension):

```markdown
# {ADR-NNNN} Execution Blueprint

- **Parent ADR:** {path to the decision record}

## System Snapshot
{Real paths discovered in Stage 1: modules, entry points, schemas, tests.}

## Work Units
### WU-0: {name}
- Requires: {WU-x, WU-y | nothing}
- Goal: {independently verifiable outcome}
- Files: {real path | action | purpose} (production + test)
- Verification: {literal, runnable command(s)}
- Tests: {Gherkin scenarios or TDD cycles, per engineering-standards}
- Done When:
  - [ ] {observable acceptance criterion}

## Ordering
| WU | Requires | Parallel group |
|---|---|---|
| WU-0 | none | none |
| WU-1 | WU-0 | P1 |
| WU-2 | WU-0 | P1 |

## Parallel Groups
- P1 (after WU-0): WU-1 and WU-2. Disjoint files, no shared state, safe to run concurrently by separate agents.
- Sequential: WU-0 first.

## Dependency Graph
{Mermaid graph generated from the Ordering table.}

## Confidence + open items

- Confidence: HIGH | MEDIUM | LOW, <one line on what makes it that>
- Open items (verify downstream): each one MUST be stated precisely enough that whoever picks it up next knows exactly what to check or decide. If it can't be phrased that precisely yet, say so plainly instead of listing a vague placeholder that only looks actionable.
  - <blind spot or LOW-confidence premise>, <who verifies: /playbook:plan interview, /playbook:implement watch>
```

Requirements: each work unit independently verifiable and committable as one small unit; file plans reference real existing paths; verification commands are literal (no placeholders); the Ordering table shows each WU's `Requires` and `Parallel group`; the test plan follows `playbook:engineering-standards`. Mark a shared `Parallel group` only when its members have no dependency on each other, touch disjoint files, and share no mutable state or ordering-sensitive step; otherwise leave them sequential. `/playbook:implement` consumes this blueprint exactly like a `/playbook:plan` plan: one small commit per WU, parallel-safe WUs dispatched to concurrent agents.

Present the blueprint. Revise in place until the user explicitly approves.

## Stage 3: Quality Gate (MUST)

After the user approves all drafts, run the three-phase gate before finalising. Don't skip it; don't finalise until it passes or the user explicitly overrides. Criteria are inline. Run Phase 1 first (Phases 2 and 3 read its report), then dispatch Phase 2 and Phase 3 in parallel: issue both Agent calls in a single message so they run at once. Phase 2 and Phase 3 are independent, so they never run one at a time; the 1-before-(2,3) order is the only real dependency.

**All three phase agents are structurally read-only, so their only channel is the return value, and it is unreliable** (`playbook:delegating-subagents`; invoke it before dispatching). `fact-checker`, `critic` and `test-reviewer` hold Read, Grep, Glob and Skill only. They cannot write a report file: the repo's agent checks forbid `Write` and `Bash` for that tier by design, and granting them would fail CI.

**Run Phase 1 inline. It is faster and it actually finishes.** Its checks are mechanical (do these paths exist, do the claimed line numbers match, is the dependency graph acyclic, do the Ordering table and the mermaid graph agree, are the parallel groups file-disjoint). That is a handful of shell and python commands, about two minutes, with re-runnable output. Delegating it has produced nothing across repeated attempts on this repo, while the inline version caught three wrong line citations and two unhonoured memory facts.

**A phase that returned nothing is INCONCLUSIVE, never PASS.** On two separate gates for ADR 0007, every dispatched phase agent went idle without returning; recording those as passes would have published a gate that checked nothing. INCONCLUSIVE blocks finalisation exactly like FAIL. Either redo that phase inline or record the gap explicitly in the quality report, naming which phase did not run.

Spawn each delegated agent with a stable `name`. Stop it only once you have its verdict or have made one post-idle `SendMessage` attempt; stopping is unrecoverable for a read-only agent.

Throughout this stage, `<record-slug>` is `adr-{base}`, where `{base}` is the record's filename without extension (from Stage 2's Determine the filename step, e.g. `docs/adr/0016-foo-bar.md` gives `<record-slug>` of `adr-0016-foo-bar`). Every gate record and gate check below uses `<record-slug>` directly, already carrying its `adr-` prefix; do not prefix it again. That prefix keeps this record's gate rows distinct from `/playbook:plan` and `/playbook:implement` runs sharing the same per-worktree gate ledger.

**Gate source has two tiers.** Before each phase's own dispatch (its initial dispatch and any retry of that specific phase), snapshot the record file's content followed by the blueprint file's content (record content alone for a `--record-only` ADR) to a phase-specific scratch file, e.g. `/tmp/<repo>/adr-<record-slug>-source.<phase-name>.md` (`<phase-name>` is `fact-check`, `adversarial`, or `test-review`). That phase's gate record always points at its own snapshot, frozen at the moment that phase was dispatched, never at a sibling phase's file: Phase 2 and Phase 3 dispatch in parallel, so if Phase 2 FAILs and gets revised before Phase 3 returns, Phase 3 must still record its verdict against what it actually reviewed, not the just-revised content. A separate shared scratch file, e.g. `/tmp/<repo>/adr-<record-slug>-source.md`, stays refreshed on every revision exactly as before; this is `<gate-source-path>`, used only by this stage's final Gate Check, which needs the current, fully up-to-date record and blueprint rather than any one phase's frozen snapshot.

**Gate ledger.** Every phase return is recorded, and one final check decides whether Stage 3 may finalise. Intent: gate-record. Intent: gate-check. If the session's tool map lists them, use those commands. Record with `<record-slug> adr <phase> - --source <snapshot>`: the agent's full raw return text goes on stdin (a quoted heredoc, no scratch file) and the phase's own snapshot is the source file. Check with `<record-slug> adr fact-check adversarial test-review` and `<gate-source-path>` as the source file; its exit code is the verdict. Otherwise do this: keep a `## Gate checklist` section in the draft quality report `{DIR}/{base}-quality.md`, one line per phase with its latest verdict (PASS, WARN, FAIL or INCONCLUSIVE), the round, and the snapshot it reviewed. A later verdict for a phase replaces its earlier line. The check passes only when all three lines read PASS or WARN and neither the record nor the blueprint is newer than any phase's snapshot (`[ <file> -nt <snapshot> ]`); a missing line is MISSING and a phase whose snapshot predates a revision is STALE. Treat anything else as a non-zero exit. Without the tool this enforcement is advisory, since nothing outside this session holds you to it, so apply it strictly.

Before running the gate, read `../skills/delegating-subagents/references/quality-gate.md` (relative to this file); it holds the recording, Verification Summary, Test Review criteria, INCONCLUSIVE and gate check rules used below.

### Phase 1: Fact-Check

**Before Phase 1's first dispatch, and before each retry of Phase 1**, snapshot the record (and blueprint, if any) to `/tmp/<repo>/adr-<record-slug>-source.fact-check.md`, Phase 1's own snapshot, and refresh the shared `<gate-source-path>` alongside it.

Spawn a `fact-checker` agent with the record (and blueprint, if any). It verifies: file paths in the system snapshot and file plans exist; function/type signatures referenced are accurate; the plan is consistent with existing patterns; the work unit dependency graph is acyclic and each Parallel group's WUs have disjoint files with no dependency on each other; if a memory store was loaded in Stage 1, known gotchas related to the topic are accounted for. Returns a PASS/FAIL/WARN report. Phase 1 folds a Verification Summary into it (table shape in the quality-gate reference).

**Record the gate** for `fact-check` against `/tmp/<repo>/adr-<record-slug>-source.fact-check.md` (Gate ledger above). Do this every time Phase 1 returns, including every retry (the recording rule is in the quality-gate reference).

After it returns, if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, persist any durable gotcha as a memory fact, locked append as in Stage 1; otherwise skip that step silently. **FAIL → revise and re-run under the gate loop (`playbook:delegating-subagents`, `references/gate-loop.md`: hard cap `gate.maxRounds` rounds, stop at PASS, STUCK on no progress).** A revised record or blueprint means re-writing this phase's own snapshot, and the shared `<gate-source-path>`, before this phase re-runs.

### Phase 2: Adversarial Review

**Before Phase 2's own dispatch, and before each retry of Phase 2**, snapshot the record (and blueprint, if any) to `/tmp/<repo>/adr-<record-slug>-source.adversarial.md`, Phase 2's own snapshot, and refresh the shared `<gate-source-path>` alongside it.

Spawn a `critic` agent with focus `decision`, given the record, blueprint, and the Phase 1 report. It challenges the decision: simpler alternatives, scope creep, over-engineering, missing error paths, blast radius, contradictions with the fact-check.

**Record the gate** for `adversarial` against `/tmp/<repo>/adr-<record-slug>-source.adversarial.md` (Gate ledger above). Do this every time Phase 2 returns, including every retry.

After it returns, if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/`, record any rejected simpler alternative (with reasoning) as a memory fact, locked append as in Stage 1; otherwise skip that step silently. **FAIL → revise and re-run under the same gate loop.** A revised record or blueprint means re-writing this phase's own snapshot, and the shared `<gate-source-path>`, before this phase re-runs.

### Phase 3: Test Review

**Before Phase 3's own dispatch, and before each retry of Phase 3**, snapshot the blueprint's test plan (and record, if any) to `/tmp/<repo>/adr-<record-slug>-source.test-review.md`, Phase 3's own snapshot, and refresh the shared `<gate-source-path>` alongside it.

Spawn a `test-reviewer` agent with the blueprint's test plan and the Phase 1 report (it runs in parallel with Phase 2). For a `--record-only` ADR with no blueprint tests, this is typically `PASS: N/A (no test plan)`. For a blueprint with Gherkin scenarios or TDD cycles, it evaluates them against the Test Review criteria in the quality-gate reference.

**Record the gate** for `test-review` against `/tmp/<repo>/adr-<record-slug>-source.test-review.md` (Gate ledger above). Do this every time Phase 3 returns, including every retry. **FAIL → revise the test plan and re-run under the same gate loop.** A revised record or blueprint means re-writing this phase's own snapshot, and the shared `<gate-source-path>`, before this phase re-runs.

### Structural Checks

- [ ] Every Considered Alternatives entry has effort and trade-off detail.
- [ ] The Decision section explains why each rejected alternative was rejected.
- [ ] All work units (if a blueprint exists) have file plans with real paths.
- [ ] All verification commands are literal (no `<placeholders>`).
- [ ] No unresolved questions remain (or each is explicitly deferred to a named work unit).

### Gate Result

**Gate check (MUST, before presenting the result):** run the gate check from the Gate ledger above for `fact-check`, `adversarial` and `test-review`, using the shared gate source file in its current, fully refreshed state (never a phase's per-phase snapshot). Everything above, the phase reports and the Structural Checks, is for the human reading it; the check's result is what actually decides whether Stage 3 may finalise. Exit 0: proceed to present the result below and continue to Stage 4. Non-zero exit: do NOT present a "gate passed" result and do NOT proceed to Stage 4; handle it as the quality-gate reference says (verbatim output, FAIL and INCONCLUSIVE retry, STALE re-run).

A non-zero exit blocks finalisation unless the user explicitly overrides: the override never changes or fakes the gate check's result, it is an explicit, recorded decision to proceed despite a real, honestly reported non-zero exit, not a claim that the gate actually passed. Present the result, now backed by the recorded verdicts above rather than only the in-session agent responses. FAILs block finalisation; WARNs are informational. If the user explicitly overrides a FAIL, record the override in the quality report file: `Quality gate override: proceeding despite FAIL on <check> because <reason>`.

```
## Quality Gate Result

**Fact-Check:**        PASS (N/N)
**Adversarial Review:** PASS (N/N)
**Test Review:**       PASS (N/N)  [or N/A]
[or: BLOCKED: N FAILs, M WARNs]
```

- **INCONCLUSIVE** blocks finalise exactly like FAIL (definition in the quality-gate reference).

## Stage 4: Finalise

After the gate passes (or is overridden):

1. **Update the record:** Status Proposed → Accepted, and bump Date modified to today.
2. **Verify the blueprint** (if any): its Parent ADR reference points to the correct record path.
3. **Save the quality report** to `{DIR}/{base}-quality.md`.
4. **Memory graph:** if a project store is present at `~/.config/playbook/memory/<owner>/<repo>/` and you wrote any memory facts for this decision, the graph rebuilds automatically on fact save via the PostToolUse hook. Otherwise skip silently.
5. **Report** the final file paths to the user.

### Kebab Title Convention

Lowercase all words, replace spaces and special characters with hyphens, collapse consecutive hyphens. "Replace Polling with WebSocket Push" → `replace-polling-with-websocket-push`.

## Implementing the Blueprint

The blueprint is a self-contained implementation plan. Run `/clear` first, then implement it with a clean context: `/playbook:implement {DIR}/{base}-blueprint.md`. The record-drafting and quality-gate back-and-forth in this session is not something the execution phase needs to inherit.

## Teardown (MUST run, even on failure or abort)

Close any subagent from this run that is still alive before finishing.
