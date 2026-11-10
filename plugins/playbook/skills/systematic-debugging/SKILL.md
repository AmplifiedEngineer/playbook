---
name: systematic-debugging
description: Root-cause-first debugging. Use on any bug, failing test, or unexpected behavior, before proposing a fix.
user-invocable: false
---

# Systematic Debugging

**Iron law: no fix without a root-cause investigation first.** A change that makes the symptom go away without explaining why it happened is not a fix. This holds for trivial-looking bugs, under time pressure, and when someone wants it fixed now. It applies to test failures, production bugs, performance problems, build failures, flaky tests and integration issues.

Finish each phase before the next.

## Phase 1: Find the root cause

1. **Read the error carefully.** The whole stack trace, line numbers, paths, error codes. Warnings often hold the answer.
2. **Reproduce it.** Exact steps, every time or intermittent. If you cannot reproduce it, gather more data instead of guessing.
3. **Check recent changes.** `git diff`, recent commits, new dependencies, config and environment differences.
4. **Gather evidence at component boundaries.** In a multi-component system (CI to build to signing, API to service to database), add temporary logging at each boundary: what enters, what exits, whether config propagates. Run once to see WHERE it breaks, then investigate that component.
5. **Trace the data flow backward** to where the bad value started and fix at the source. See `root-cause-tracing.md`.

## Phase 2: Analyse the pattern

1. Find similar working code in the same codebase and list every difference from the broken path, however small.
2. If following a pattern or library, read the reference implementation completely, not the gist.
3. Note what the code depends on: components, settings, config, environment.

## Phase 3: Hypothesis and test

1. State one hypothesis in writing: "I think X is the root cause because Y."
2. Test it with the smallest change, one variable at a time.
3. Worked: go to Phase 4. Did not work: form a NEW hypothesis, never stack another fix on top.
4. When you do not know, say so. Ask the user or research more.

## Phase 4: Fix the root cause

1. **Write a failing test first**, the simplest reproduction (a one-off script if no framework exists). Follow the project's engineering standards, including the language rules for the code under test.
2. **Make one fix.** No "while I am here" changes, no bundled refactor.
3. **Verify:** the new test passes, nothing else broke, the issue is gone.
4. **Count failed fixes.** Under 3: return to Phase 1 with the new information. **3 or more: stop and question the architecture.** If each fix reveals new shared state or coupling elsewhere, needs massive refactoring, or creates a new symptom, the pattern may be wrong. Ask the user before attempting fix 4.

Then consider validation at several layers so the bug becomes impossible. See `defense-in-depth.md`.

## Stop and return to Phase 1 when

- You think "quick fix now, investigate later", "try X and see", "one more attempt" after two failures, or "it is probably X".
- You list fixes before tracing the data flow, or bundle several changes before running tests.
- The user says "stop guessing", "is that not happening?", "will it show us...?" or "are we stuck?".

Read `rationalizations.md` when tempted to skip a step.

## When no root cause exists

If the cause is genuinely environmental, timing-dependent or external: document what you investigated, add handling (retry, timeout, clear error), and add logging for next time. Most "no root cause" cases are incomplete investigation, so be honest about which this is.

## Supporting files

- `root-cause-tracing.md`: trace a bug backward through the call chain to the original trigger.
- `defense-in-depth.md`: add validation at each layer after finding the cause.
- `condition-based-waiting.md`: replace arbitrary timeouts in tests with polling on the real condition.
- `rationalizations.md`: excuses for skipping the process and the reality behind each.

Verify claims against real code and cite `file:line`.

## Memory

When the investigation surfaces a durable root cause or gotcha, and a project memory store exists at `~/.config/playbook/memory/<owner>/<repo>/` (from `git remote get-url origin`), save it as a project memory fact with `anchors:` to the files involved. If there is no project store, skip it.
