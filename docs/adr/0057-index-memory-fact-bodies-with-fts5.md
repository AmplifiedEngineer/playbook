# ADR-0057: Index Memory Fact Bodies with SQLite FTS5

- **Status:** Proposed
- **Date created:** 2026-11-04
- **Date modified:** 2026-11-04
- **Amends:** [ADR 0004](0004-graph-first-memory.md) alternative D (a searchable index over fact bodies). It replaces the reason 0004 gave for rejecting that option. The rest of 0004 stands.
- **Relates to:** [ADR 0018](0018-tls-platform-verifier-not-adopted.md) (binary size is a cost worth refusing), [ADR 0026](0026-local-code-index-in-the-binary.md) (the FTS5 pattern `playbook code` already ships)

## Context

ADR 0004 considered "a searchable index over fact bodies" as option D, sized it XL, and rejected it because the corpus was 80 files under 200 KB: "infrastructure for a scale that does not exist". That reasoning was sound for 80 files. Three things have changed since.

- **The recall hook reads names and descriptions only.** `UserPromptSubmit` recall (`crates/cli/src/hooks/memory_anchors.rs`, `prompt_token_matches`) scans a per-session TSV of fact names, descriptions and aliases. It reads a fact body only after a match, to inject it. A fact whose wording sits in its body is not found.
- **The store is larger and expected to keep growing.** The in-scope store for this repo is 286 facts today (65 global, 221 project), and 698 facts across all scopes. The maintainer expects tens of thousands.
- **The scan is linear.** The plan `memory-search-rework` measured the current matcher at 2.4 ms p50 for 286 facts, 27.9 ms at 10k and 132 ms at 50k. Adding bodies to the scan costs 25.7 ms p50 at 286 facts, ten times today's cost, and 188 ms at 10k.

The premise that kept this option out of reach is also gone. FTS5 is compiled into the bundled SQLite: `.cargo/config.toml` no longer passes `-USQLITE_ENABLE_FTS5`, and `playbook code` already keeps a contentless FTS5 index and ranks with `bm25()` (`crates/code/src/db.rs`, `crates/code/src/search.rs`). The memory fact that said FTS5 was compiled out is stale.

Recent hook changes keep their meaning under this decision: the zero-match and weak-match hint (#1194) and the alias and crate-folder signals (#1198) stay in the scorer and in the TSV, so they do not move to the index.

## Decision

Add an FTS5 index over fact name, description and body to `memory.db`, and let the recall hook take its candidates from it.

1. **Engine.** A contentless FTS5 virtual table, `facts_fts`, with the plain `unicode61` tokenizer, in the existing `memory.db`. No new dependency and no added binary size. The memory matcher splits words on every non-alphanumeric character, so `unicode61` without `tokenchars` is the tokenizer that agrees with it.
2. **Ranking.** `bm25()` with name over description over body, ties broken by fact id so order is the same on every filesystem.
3. **The scorer stays.** The existing scoring, strength rule, alias bonus, crate-folder bonus, session dedup and caps keep running over the candidates the index returns. Aliases and crate-folder hits stay TSV lookups unioned into the candidate set, because alias matching runs on the whole prompt phrase and an index of filtered tokens cannot express it.
4. **Derived data, with a fallback.** `memory.db` stays fully derived from the Markdown files. The table is created in a guarded step, never in the shared schema batch, so a build without FTS5 degrades instead of failing every rebuild. The hook falls back to the TSV scan when FTS5 is unavailable, the table is missing, the index is stale, or the database is busy. An empty result from a fresh index is final and does not trigger the scan.
5. **Scoped writes.** Index rows are written inside the rebuild transaction and deleted by `(scope, project)`, because `memory.db` is one file shared by every repository.
6. **Mid-session freshness.** Reading the index live means a fact saved in this session becomes recallable at once. ADR 0004's session cache did not do that. This ADR adopts the new behaviour deliberately. A prompt that takes the fallback still sees the session snapshot.
7. **Evidence first.** A synthetic fixture and a regression harness land before any reader, so each later change shows what it did to recall.

The schema version stays at `"1"`. A bump would make an older binary refuse the newer file.

## Considered alternatives

**Keep the scan and add bodies.** Rejected on measurement. It has the best recall of the lexical options (20 of 25 at 286 facts) at 25.7 ms p50, about 230 times the latency of the index for the same recall.

**Tantivy.** Faster still, but it adds 4,142,400 bytes (+3.95 MiB) and 150 locked packages. Its recall@3 was lower at today's size (17 of 25 against 20 of 25). ADR 0018 refused a 2.2 MB increase on size grounds.

**Embedded vectors (`sqlite-vec` with a local model) and hybrid ranking.** The index is cheap, but ONNX session creation costs 0.50 to 0.75 s in every process, and recall runs in a fresh process per prompt. The model adds 23 MB. Recall would also stop being deterministic. Hybrid ranking inherits the same fixed cost.

**Meilisearch.** Licensed MIT and BUSL-1.1, with no embedded mode. It needs a daemon on a TCP port, and the hook must stay silent and work offline.

## Consequences

- Recall can find a fact by its body, and it will surface facts that never surfaced before. That is the goal, and it changes what users see.
- The index adds disk: about 40 MB at 50k facts in the contentless form, from the plan's measurements.
- The rebuild gains an index write. The session-start cost of the TSV and graph at 50k facts is unmeasured, and the first Work Unit measures it. If it exceeds the query saving, the later work does not start.
- Two retrieval paths exist, so a differential test runs both over a static store until bodies join the index. After that a weaker containment test replaces it.
- The similarity pass that builds `possible_relates_to` edges is quadratic and is the largest cost at scale. It is out of scope here and needs its own plan, and probably its own ADR against ADR 0011.

## Decisions this ADR needs from a maintainer

- Accept ADR 0004 alternative D as revisited.
- Accept mid-session freshness for the recall hook.
- Accept leaving `SCHEMA_VERSION` unchanged.
- Accept that indexing bodies changes which facts are recalled.
