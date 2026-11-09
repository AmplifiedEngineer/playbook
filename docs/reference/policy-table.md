# Policy table

This table lists every agent, command and skill with the model and effort in its
file, and why. The test `crates/plugin-checks/tests/policy_table.rs` reads the frontmatter of
`plugins/playbook/agents`, `commands` and `skills` and fails when a row and its
file disagree, or when a component has no row. The reasons come from the bench
rounds recorded in the playbook repository (`docs/internals/02-model-routing-and-memory.md`
and ADR 0021, 0022 and 0023).

<!-- policy-table:start -->
| Kind | Name | Model | Effort | Reason |
| :- | :- | :- | :- | :- |
| agent | `analyst` | `haiku` | `medium` | Distills collector findings into memory facts. Round 2 bench: Haiku 45/45 at medium against Sonnet 15/15, at about a fourteenth of the cost; the user confirms facts in learn-project Phase 3. |
| agent | `auditor` | `opus` | `high` | A read-only repo audit where a missed finding is the cost. |
| agent | `cheap-checker` | `haiku` | `low` | One narrow concern from a named reference file. A full lens covers the rest. |
| agent | `collector` | `haiku` | `low` | Gathers and compacts raw history. The analyst does the thinking. |
| agent | `critic` | `sonnet` | `medium` | Adversarial review of plans and ADRs. Round 2 bench: medium 89/90, high 71/90 (false blocks on clean plans), so medium finds the seeded flaws at 39% lower cost. |
| agent | `fact-checker` | `haiku` | `medium` | Three bench rounds: Haiku medium 134/138, high 137/138, at about a tenth of Sonnet's cost. The Haiku effort cap already held it at medium. |
| agent | `git` | `haiku` | `medium` | Drafts commit and PR text. The binary now refuses a commit type that contradicts the files, so a weaker draft cannot land a wrong type. `playbook commit all` drafts the common commit with no model. The role may still go up to xhigh (OPT_INS). |
| agent | `implementer` | `sonnet` | `medium` | Writes and commits production logic. Round 2 bench on 20 repo tasks: Sonnet medium 200/200, high 198/200; Haiku stays under 92%. |
| agent | `review-triage` | `haiku` | `low` | A three-way classifier. A bad or missing answer falls back to a full lens. |
| agent | `reviewer` | `opus` | `medium` | Opus medium 199/200 against high 200/200 on the reviewer bench, 19% cheaper. The orchestrator passes a higher or lower `effort` on the spawn for a risky or small diff (ADR-0021). |
| agent | `test-reviewer` | `haiku` | `low` | Pilot (18 calls per cell): Haiku low, medium and high 18/18, Sonnet high 18/18, Sonnet low 16/18. |
| agent | `writer` | `haiku` | `medium` | Writes the final text of one kind from facts the orchestrator decided. Pilot on 18 cases: no invented fact and no hard prose finding. It decides nothing. |
| command | `address-pr-comments` | `opus` | `medium` | Judgment on review threads and replies that go public. Maintainer choice on 2026-10-11, no bench (ADR-0023). |
| command | `adr` | `opus` | `medium` | A hard-to-reverse decision record. Maintainer choice on 2026-10-11, no bench (ADR-0023). |
| command | `commit-and-push` | - | - | Forks into the git agent, which sets the model and effort. |
| command | `create-pull-request` | - | - | Forks into the git agent, which sets the model and effort. |
| command | `deep-review` | `opus` | `medium` | Orchestrates the reviewer swarm. A missed finding is the cost. Maintainer choice on 2026-10-11, no bench (ADR-0023). |
| command | `doctor` | `haiku` | `low` | Runs compiled helpers and prints the result. Mechanical. |
| command | `fix` | `sonnet` | `medium` | Small, well understood bugs, with code changes. Maintainer choice on 2026-10-11, no bench (ADR-0023). |
| command | `implement` | `sonnet` | `medium` | Executes an approved plan and delegates edits. Maintainer choice on 2026-10-11, no bench (ADR-0023). |
| command | `learn-project` | `opus` | `medium` | Builds durable memory from a whole repo. Mistakes persist. Maintainer choice on 2026-10-11, no bench (ADR-0023). |
| command | `plan` | `opus` | `medium` | Design work. A weak plan is expensive. Maintainer choice on 2026-10-11, no bench (ADR-0023). |
| command | `quick-review` | `sonnet` | `medium` | One pass over a diff. Its reviewer runs on Opus at medium, the reviewer's own effort. |
| command | `repo-audit` | - | - | Forks into the auditor agent, which sets the model and effort. |
| command | `review` | `sonnet` | `low` | Recommends a quick or a deep review from the diff, asks, then hands the run to that review. It reads and routes, and the review it runs does the work. |
| command | `session-handoff` | `haiku` | `low` | Writes and saves a handoff using binary-supplied rules and facts. |
| command | `session-start` | `haiku` | `low` | Loads a saved handoff. Mechanical. |
| command | `setup` | `haiku` | `low` | Runs compiled helpers and asks questions. Mechanical. |
| skill | `atlassian` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `delegating-subagents` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `engineering-standards-javascript` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `engineering-standards-python` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `engineering-standards-rust` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `engineering-standards` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `finish-pull-request` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `grounding-research` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `grounding-review` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `playbook-usage` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `systematic-debugging` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
| skill | `writing-style` | - | - | Knowledge loaded into the caller. A skill never sets a model or effort, so the caller decides. |
<!-- policy-table:end -->
