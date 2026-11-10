# Triage tiers

How `/playbook:review --deep` and `/playbook:implement` (Step 9) act on the tier map that `review-triage` returns, once it is completed. The two commands point here for the rules below and keep their own lens lists, prompts and commands.

## Completing the map

A dispatch that failed or returned nothing, a lens the map left out, and a `tier` other than `skip`, `cheap-check` or `full-lens` all become `full-lens`; lenses triage did classify keep their tier. A silent or malformed triage never narrows coverage (per `playbook:delegating-subagents`, `review-triage` is structurally read-only so its return value can fail silently).

When no tool completes the map, do it by hand. Take the JSON object from the return (the whole text, or what sits between the first `{` and the last `}` when it is wrapped in prose or a fence). Then write one entry per selected lens, in the caller's order, as `{"tier", "reason"}`:

- No usable JSON object: every lens is `full-lens`, reason `triage returned nothing usable: ran in full`.
- The lens is missing: `full-lens`, reason `triage did not classify this lens: ran in full`.
- The tier is not one of the three: `full-lens`, reason `triage gave an unrecognised tier: ran in full`.
- A tier the caller forces (such as `tests=skip` under `--no-tests`) wins for that lens only.
- Otherwise keep the tier and reason triage gave.

## cheap-check

Dispatch a `cheap-checker` agent (`subagent_type: playbook:cheap-checker`, already pinned to low effort, so no tier variant applies) instead of `reviewer`. Its prompt names the lens's narrow concern, taken from the tier map's `reason` field for that lens (already a short, grounded justification from `review-triage`, so it doubles as the concern statement), the diff the command reviews and its conventions for where the tree lives, and ONE reference file path to read for criteria.

Pick that file from this table. The folder is the base directory the Skill tool reported when the orchestrator invoked `playbook:grounding-review`; a lens with no row, or whose file is missing, reads that folder's `SKILL.md`.

| Lens | File |
|---|---|
| `security` | `references/security.md` |
| `perf`, `data`, `big-o` | `references/performance.md` |
| `logic`, `correctness` | `references/correctness.md` |
| `types`, `integration` | `references/reliability.md` |
| `architecture`, `migration`, `adr` | `references/architecture.md` |
| `complexity`, `dedup` | `references/maintainability.md` |
| `scope` | `references/scope-control.md` |

Hand `cheap-checker` that ABSOLUTE path, never a bare repo-relative string: it has no `Bash`, and a repo-relative path never resolves against the diff's own target repo. The narrow concern text, not the reference file, is what scopes the check, so a `SKILL.md` fallback still returns a finding scoped to just that lens's concern.

## skip

Dispatch nothing for that lens. Track it explicitly as skipped in the one-line `Triage:` summary (for example "Triage: security=full-lens, docs=cheap-check, perf=skip"). A skipped lens is never conflated with a dispatched lens that returned nothing: it was never dispatched, so it has no return value to lose.

## Trust gate

This tiered dispatch ships and functions as soon as it lands: a `full-lens` tier still gets the exact reviewer it always did, a `cheap-check` tier gets a real narrow-scope pass from `cheap-checker`, and a `skip` tier is a real, tracked decision to run nothing. But a `skip` or `cheap-check` decision should not be treated as validated judgment yet: the review-triage eval has not yet recorded a pass verdict against a real fixture set. Until it has, treat triage's tier choices as best-effort, not proven: a `skip` verdict is not yet evidence a lens truly had nothing to find, and a `cheap-check` narrow pass is not yet guaranteed to have caught everything the full lens would have.
