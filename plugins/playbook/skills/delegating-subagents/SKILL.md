---
name: delegating-subagents
description: Handoff, roster, routing, and recovery rules for subagents. Use before dispatching any subagent, and when one finishes or goes quiet.
user-invocable: false
---

# Delegating to Subagents

A return value can be lost. When the agent can write a file, the file is the delivery channel and the return value is a courtesy. When it cannot, the return value is the only channel, and silence means the agent did not run. Measurements behind this are in [references/evidence.md](references/evidence.md); read it before loosening any rule here.

## The rule

1. **Every brief for an agent that can write names an output file**, as an absolute path in the prompt.
2. **The orchestrator reads that file** the moment the agent finishes, goes idle, or is given up on, before drawing any conclusion.
3. **A missing file is a distinct outcome** from an agent that found nothing. Say which happened, and never substitute a guess about what the agent would have said.

Brief template for agents that can write:

```
Write your full report to <absolute path>.
Return only: status, plus any commit SHAs, plus one line of results (at most about 300 words in all).
The report file is the deliverable. The return value is not.
```

Read the report even when the work looks fine. The reports that mattered most came from agents with green commits, and recorded what they chose NOT to do and why.

## Roster

File delivery needs `Write` or `Bash`. The read-only agents have neither on purpose (the repo's agent checks enforce it, ADR 0003): a reviewer must not be able to modify the code it reviews. Do not grant them `Write` to fix a delivery problem; change the plan, not the guard. The read-only rows cannot run `playbook code`: the orchestrator runs it and pastes the output into their brief.

| Agent | `subagent_type` to pass the `Agent` tool | Delivery |
|---|---|---|
| `implementer` | `playbook:implementer` | File (has `Write` and `Bash`) |
| `collector` | `playbook:collector` | File (has `Bash`) |
| `auditor` | `playbook:auditor` | File (has `Bash`) |
| `reviewer` | `playbook:reviewer` | **Return value only** (Read, Grep, Glob, Skill) |
| `critic` | `playbook:critic` | **Return value only** (Read, Grep, Glob, Skill) |
| `fact-checker` | `playbook:fact-checker` | **Return value only** (Read, Grep, Glob, Skill) |
| `test-reviewer` | `playbook:test-reviewer` | **Return value only** (Read, Grep, Glob, Skill) |
| `analyst` | `playbook:analyst` | **Return value only** (Read, Grep, Glob, Skill) |
| `cheap-checker` | `playbook:cheap-checker` | **Return value only** (Read, Grep, Glob, Skill) |
| `review-triage` | `playbook:review-triage` | **Return value only** (Read, Grep, Glob, Skill) |
| `writer` | `playbook:writer` | **Return value only** (Read, Skill) |

**Always pass the `playbook:` prefix** from the second column. These are plugin agents, and a bare `subagent_type: critic` resolves to the wrong agent or none (fixed repo-wide in #284). Elsewhere, the bare name is only a label for talking about the agent. `playbook agents check` (run in CI) keeps the read-only rows free of `Edit`, `Write`, `NotebookEdit` and `Bash`. No test compares this table with `agents/*.md`: change both together.

Every agent spawns by the `subagent_type` in the table. Its effort is set per spawn with the `effort` parameter of the `Agent` call, see "Pick the effort" below. There are no per-effort agent names.

**For the read-only agents:**

- **Delegate them.** Reviews, critiques and fact-checks run in their own context. For purely mechanical checks (path existence, line counts, graph acyclicity) a few shell commands you run yourself are cheaper and re-runnable.
- **Treat silence as NOT RUN.** Never as "reviewed clean" or PASS. Name the missing lens or phase in the report and to the user.

When you need a result inline and cannot wait for an output file, prefer a forked skill (`context: fork` with a prefixed `agent: playbook:<name>` in the command's frontmatter, since a bare name falls back to a general purpose agent on the session model) over an Agent-tool spawn. It has been reliable in every measurement.

## Fresh, not forked

An `Agent` call with no `subagent_type` forks the whole parent conversation: the child inherits the parent's history (measured: about 372K tokens on the first turn, $2.90 per fork, 117 of them in 30 days). **Always pass a `subagent_type`.** Use a `playbook:` agent from the roster, or the built-in `Explore` for read-only search and `general-purpose` for anything else. A fresh agent starts with only its brief, so write the brief to stand alone:

```
Goal: <one sentence, the question or the change>
Inputs: <absolute paths with line ranges; pasted text only when short>
Output: write your full report to <absolute path>
Constraints: <scope, files not to touch, read-only or not>
Return: status, findings as file:line, output path; at most about 300 words
```

The `Return` line caps the message, not the work: the full report goes in the output file, uncapped. Every agent's prompt carries the same cap. To keep the parent context small, rely on the capped return. Read a finished agent's output file only when the return is not enough, and only the parts you need. Do not poll it while the agent runs.

Built-in `Explore` agents read no CLAUDE.md and none of playbook's prompt, but they have Bash: every `Explore` or `general-purpose` brief carries the code search rule (run `playbook code status`, then `playbook code index` when it says stale or missing, then `playbook code context` before Grep or Glob), or you run the search yourself and paste the result.

Fork only when the child needs the decisions and corrections made earlier in this session and writing them down would cost more than the inheritance. That is rare: the measured forks were research, review, edits and commit text, all of which a brief covers. Classification of every playbook spawn site is in [references/briefs.md](references/briefs.md).

## Route the task first

Route the task before picking an agent. Kinds: `mechanical`, `classify`, `check`, `review`, `write`, `implement`, `design`. Intent: route-model. If the session's tool map (printed at session start) lists it, use that command with the kind and `--json`. For `implement` add `--tier low|medium|high` (low for any well specified unit, medium for a cross-module change or ambiguous spec, high for workflow determinism, concurrency or subtle architecture). Add `--failures <n>` when the task already failed. Read `model`, `effort`, `agent` and `action`:

- `action: proceed`: dispatch the named agent.
- `action: ask`: stop and ask the user first. The task runs on the top model, uses the high tier, or has failed twice. Do not dispatch until the user says yes.
- `action: downgraded`: the user set `routing.escalate` to `deny`. Dispatch the cheaper route given.

The `routing.escalate` key is the gate: `ask` asks before escalating, `auto` (default) proceeds and logs the assumption in the run, `deny` never escalates. Say the tier and a one line reason in every dispatch report. A failure does not raise the tier by itself. The user's effort ceiling always wins (`cappedFrom` says when it applied).

Use `write` for text, see "Writing text" below. Without the tool, do this: pick the agent from the roster above, which pins its own model, and treat the gate as `ask`, the default. Before you send a task to the top model or to the high tier, or after it has failed twice, stop and ask the user first.

## Writing text

The strong model decides and the cheap model writes. For a review comment on a diff, a ticket description or other text a person reads, finish the analysis first. Then dispatch `playbook:writer` (route kind `write`, Haiku) with a brief from "Writer brief builder" in [references/briefs.md](references/briefs.md). Do not draft the text yourself first: you would pay for it twice.

1. Send the facts, not a draft. The writer adds no fact.
2. Write the text to a file and the facts to `<file>.facts` with a first line `kind: <kind>`. Run `playbook prose check <file>`. If it prints a hard finding, a passive voice hit or an unknown token, send those lines back to the writer once.
3. If it fails again, or the writer returns a `NEED:` line you cannot fill, write the text yourself from the template in the brief builder. Do not ask the user and do not pass `--failures` to the route: a text never needs approval.

Kinds that stay with the orchestrator: a reply to a PR comment. The pilot showed that a decision record (action, reference, reason) costs more output tokens than the one or two sentences it replaces. The binary drafts the commit message and the PR title, so they need no writer.

## Pick the effort

A spawn sets its effort with the `effort` parameter of the `Agent` call, on the base agent. The tool only takes the parameter when a skill asks for it, and this skill asks for it on every spawn of a playbook agent. Claude Code 2.1.296 or later applies it to plugin agents; an older version ignores it and the agent runs at the effort its file ships. Spawn the base `subagent_type` from the roster, never a `-low` or `-xhigh` name. Two rules, in order.

**1. The ceiling.** Before a spawn, resolve the agent under the user's effort ceilings. Intent: effort-resolve. If the session's tool map (printed at session start) lists it, use that command for `agents <name>` with `--json` and read `effort`, the shipped effort lowered by every ceiling (Claude Code's `maxEffortLevel`, playbook's, `effort.agents.<name>` and the model cap), and `ceiling`, the lowest of those limits (null when nothing limits). Pass `effort` as the `effort` parameter. Never pass a level above `ceiling`, and when `effort` is null omit the parameter. If the tool map lacks it, follow `references/without-binary.md#effort-resolve`.

**2. The diff.** Within the ceiling, use the limits the repo already has. The level replaces the one from step 1 only when it is not above `ceiling`:

| Diff | `effort` | Why |
|---|---|---|
| At most 300 changed lines (the `dedup` trigger in `/playbook:review --deep`), and no security-sensitive path | `low`, except for the `reviewer` and `auditor`, which stay at the level from step 1 | Little to reason about, so extra effort is spend with no extra findings. |
| Anything between, or when unsure | the level from step 1 | The default, tuned for ordinary diffs. |
| Over 60 KB (`MAX_DIFF_BYTES` in `crates/cli/src/pr/triage.rs`, the size past which `review-triage` never calls a PR quick), or the lens is `security`, or the diff touches auth, secrets, crypto, or untrusted input | `high`, and never `xhigh`: `high` is the top for the reviewer and the gate reviewers (`critic`, `fact-checker`, `test-reviewer`) | A missed finding here is expensive and the diff is big enough to hide one. `xhigh` finds no more here and costs more. |

**3. Automatic choice (only when the user turned it on).** When the key `effort.auto` is `suggest` or `on`, let the tool choose. Intent: effort-choose. If the session's tool map (printed at session start) lists it, use that command with the agent name, `--base <ref>` (the diff's base, or `--lines`, `--files` and `--path` when there is no git diff) and `--json`. Do nothing extra while `effort.auto` is `off`, the default. Pass the `effort` it returns as the `effort` parameter and spawn the `dispatch` it returns, which is always the base agent: with `on` that is the chosen level, inside the agent's quality floor and every ceiling, and with `suggest` it is still the default. When `note` is set, print it as one line to the user, since the choice deviates from the default. An error or an unexpected answer means the default: use what step 1 gave. The agent marked costly to miss (the reviewer, the auditor) is only raised, never lowered. The tool logs every choice. This replaces the table above for that spawn, and never overrides step 1. If the tool map lacks it, follow `references/without-binary.md#effort-choose`.

Narrow `cheap-check` lenses go to `cheap-checker`, already pinned low. A verifier that misses a wrong claim defeats its purpose, so lower a `fact-checker` only when the ceiling requires it. Every level keeps the base agent's `tools`, so the read-only and file-delivery limits above apply unchanged.

## Worktree guard

A commit or PR command (`/playbook:commit`, `/playbook:pr-create`) run by a subagent can start in the wrong checkout. Its first step is the guard in [references/worktree-guard.md](references/worktree-guard.md); the caller passes "Worktree root: <path>, branch: <name>" in the prompt.

**Shell agents after `EnterWorktree`.** Claude Code 2.1.318 refuses every shell command whose directory is the shared checkout once a session has entered a worktree. An agent spawned without `isolation: worktree` inherits that guard and cannot run shell commands there. When the session entered a worktree, spawn a shell agent (`collector`, `general-purpose`) with `isolation: worktree`, or call `ExitWorktree` with action `keep` first. An isolated agent works in its own checkout, so its brief names the branch to base on and where to deliver. For the `implementer`, and for forked skills (`git`, `auditor`), call `ExitWorktree` with `keep` first: they work in the session's checkout. Read-only agents have no Bash and need nothing. If a call is refused with the worktree isolation message, do not retry it: call `ExitWorktree` with `keep` and continue from the original directory.

## Re-dispatching

Every `Agent` call is a fresh spawn with no memory of any prior round, even for the same `subagent_type`. When a quality-gate phase (`critic`, `test-reviewer`, `fact-checker`) FAILs, is revised, and needs a second pass, send the COMPLETE current artifact, not a changelist. A partial re-prompt makes the agent flag things as unaddressed that the artifact already covers; treat a "still failing" or "new finding" from one with suspicion. Why: [references/evidence.md](references/evidence.md). How many rounds a gate phase may take, and when it stops as STUCK, is in [references/gate-loop.md](references/gate-loop.md).

## Verifying delegated work

Reading the report does not replace these; it tells you where to aim them.

- Confirm the work from git, not the report's claims: `git show --stat`, then read the diff against the brief.
- Re-run the scoped verification yourself. A DONE the diff does not support is a failure.
- When the report names a deliberate divergence or untested edge, decide explicitly whether to accept it and record the decision.
