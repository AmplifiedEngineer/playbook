# Gate loop

How `/playbook:plan`, `/playbook:adr` and `/playbook:implement` (Step 4) repeat a quality-gate phase (`fact-checker`, `critic`, `test-reviewer`) after a FAIL. The three commands point here and do not restate it.

## Rules

1. **Round n** is the nth dispatch of one phase. The **hard cap is `gate.maxRounds` rounds** per phase (default 5, range 1 to 20). Read it once at the start of the gate. Intent: config-get. If the session's tool map lists it, use that command for `gate.maxRounds`. Otherwise do this: use the default, 5. It is a ceiling: a phase never runs to the cap because it can.
2. **PASS or WARN stops the phase at once.** No extra round to be safe.
3. **INCONCLUSIVE** (a silent or empty return) is not a PASS and blocks like FAIL. Redo the phase inline, as before. It uses a round of the cap but stays out of the progress comparison below: the next verdict is compared with the last round that returned one.
4. **FAIL**: revise the artifact, refresh the snapshots, and re-dispatch with the COMPLETE current artifact (see Re-dispatching in `SKILL.md`). Record after every return.

## Progress

A **blocking finding** is a finding marked `(blocking)`, or for `fact-checker` a check marked FAIL. Compare rounds by a stable key per blocking finding: its title lowercased with punctuation removed, plus its anchor (`file:line`, or the WU or step id when there is no file). If a revision only moved the line, compare file and title. The **top finding** is the first blocking one in the report, which lists the highest severity first.

Let B(n) be the blocking set of round n. From round 2, round n made **no progress** when either holds:

- B(n) has as many findings as B(n-1) or more.
- B(n) is a subset of B(n-1) and still contains the top finding of B(n-1).

## STUCK

Two consecutive rounds with no progress (so round 3 at the earliest) end the phase as **STUCK**, which is not FAIL and not INCONCLUSIVE. Record it as STUCK. Intent: gate-record. If the session's tool map (printed at session start) lists it, use that command with `--stuck`, which stores STUCK in place of the report's FAIL. Otherwise do this: write STUCK on the phase's checklist line (see `quality-gate.md`). The gate check blocks on STUCK exactly as on FAIL.

Reaching the last round (`gate.maxRounds`) with a FAIL stops the phase as FAIL, because progress never stalled.

## Stop report

On STUCK or a FAIL at the cap, stop and give: the remaining blocking findings (title and anchor), the rounds used, and one line per revision on what was tried. The next manual pass starts from that list. In `--auto`, do not save; report it. Interactively, hand the report to the user. `--force` is unchanged.

The gate recording tool prints the round number it just stored as `round N of M`, where M is the cap, and keeps the history in its `gate_rounds` table, so a cap that binds can be counted later. Without it, the checklist's round numbers are the only count.
