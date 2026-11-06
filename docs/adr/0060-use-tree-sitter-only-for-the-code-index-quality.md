# ADR-0060 Quality Report

- **Record:** `docs/adr/0060-use-tree-sitter-only-for-the-code-index.md`
- **Date:** 2026-11-06
- **Result:** The record is Accepted. The owner approved the plan direction (no language servers, no automatic downloads, no sidecar server). The gate ran 2 rounds.

## Gate checklist

| Phase | Latest verdict | Round | Note |
|---|---|---|---|
| Fact-check | PASS | 2 | Run inline: paths, line numbers and numbers checked against the repo, `gh` and the benchmark reports. |
| Adversarial review | PASS | 2 | Round 1 found ADR 0026 left unmarked; the edit was added. |
| Test review | WARN | 2 | Round 1 FAIL on gate rules missing from decision 4; folded in. Two warnings remain. |

## What the rounds changed

- Four line citations were corrected (ADR 0058 lines 23, 49 and 62, and the `flate2` claim, which is present in `Cargo.lock` through other crates).
- Benchmark reports and the plan are cited as not committed.
- ADR 0026 gets a withdrawn marker on decision 3.
- Decision 4 now states override binding, the unmeasured-cell rule, the floor check, the merge-base rule and the per-PR versus nightly split.

## Known gaps

- Overrides can lower the baseline in the same pull request. Owner review of the data files is the control, and the ruleset does not require code owner review yet.
- The benchmark reports are not committed, so the figures cannot be re-checked from the repo.
