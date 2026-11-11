---
description: Use when starting on a repo the memory store knows nothing about, or to refresh it. Reads git history, PRs, JIRA and Confluence into memory.
allowed-tools: Bash, Read, Grep, Glob, Write, Agent, Skill, WebFetch
argument-hint: "[--refresh] [--graph-only] [--stage] [--from-staged] [--max-prs N] [--max-commits N] [--auto] [--ask]"
model: opus
effort: medium
---

# Learn Project

Build a durable mental model of the repo you're in and persist it as memory facts. Read broadly (code, git history, PRs, and JIRA/Confluence when reachable), distill into topics, classify each fact as repo-specific or cross-project, and write it in the memory format from the system prompt's **Memory** section. Read-only on the project: the only writes are under `~/.config/playbook/memory/` (fact files), plus the graph in `~/.config/playbook/data/memory.db` when it rebuilds.

## Argument parsing

Parse `$ARGUMENTS`:

- `--refresh` → re-derive and supersede existing learned facts instead of skipping them.
- `--graph-only` → skip Phases 1-3; rebuild the single graph in `~/.config/playbook/data/memory.db` from current memory (Phase 4.5), then report. Use after hand-editing facts.
- `--stage` → run collection and analysis (Phases 0-2) but don't write to the live store or ask for confirmation. Write candidate facts to `~/.config/playbook/memory/<owner>/<repo>/staging/` for later review, then stop. See **Staging mode**. Use for unattended or session-end runs.
- `--from-staged` → skip collection; load candidates from `~/.config/playbook/memory/<owner>/<repo>/staging/`, run the normal confirm-and-write flow (Phases 3-4.5), then clear the staging area.
- `--max-prs N` (default 200) and `--max-commits N` (default: all, summarized) → bound scope on large repos.
- `--auto` or `--ask` → set the run mode for this run. Step 0 reads it.
- Anything else → ignore with a one-line warning; don't abort.

## Execution rules

1. Run every bash block for real with the `Bash` tool. Don't simulate.
2. Read files before asserting facts about them (grounding).
3. Combine independent `Bash` calls into a single tool call.
4. Never edit project code or config. Writes are limited to `~/.config/playbook/memory/` files.
5. Dispatch subagents for collection and analysis with the Agent tool, `collector` for Phase 1 and `analyst` for Phase 2: issue the independent Agent calls in a single message so they run in parallel. Subagents produce distilled structured findings, never raw dumps.
6. **Delivery differs per agent tier, per `playbook:delegating-subagents` (invoke it before dispatching).** `collector` holds `Bash`, so it MUST write its findings to a named absolute path under `/tmp/learn-project/<owner>-<repo>/` (`mkdir -p` it first) and return only a one-line count; read those files after each collector finishes, goes idle, or is given up on, because an Agent-tool spawn often completes and returns nothing. `analyst` is structurally read-only and cannot write a file, so its candidate facts come back only by return value, which may not arrive. Either way, an agent that delivered nothing did NOT run: name it as missing rather than proceeding with a partial picture, since a fact written from a half-collected repo is worse than a missing one and much harder to notice later.
7. No silent truncation. If you cap commits/PRs or skip a source, the final report says so.
8. Never persist secrets. Tokens, keys, or credentials seen in configs/CI must never enter a memory fact.

## Step 0: Read the run mode

Do this first, before Phase 0.

**Memory tool (MUST).** This command builds the memory store and its graph, and it reads the store through the memory tool. Intent: memory-context. Intent: memory-rebuild. Intent: memory-stats. If the session's tool map (printed at session start) does not list memory-context and memory-rebuild, stop with one line and nothing else: "/playbook:learn needs a memory tool, and this session's tool map does not list one, so nothing was read or written." Otherwise carry on and use those commands where the phases below name them.

Resolve whether this run is `ask` or `auto`. Intent: run-mode. If the session's tool map lists it, use that command with this command's name (`learn`) and `$ARGUMENTS`, and follow `../skills/delegating-subagents/references/run-context.md` (relative to this file) for its output: the warning, the effort ceiling, a conflicting-flags exit, and any other failure. Otherwise do this: the mode is `auto` when the arguments carry `--auto` and `ask` when they carry `--ask`. When both are given, stop with one line saying they conflict. With neither, run `printenv PLAYBOOK_MODE` and use its value when it is `ask` or `auto`; in every other case the mode is `ask`. There is no effort ceiling on this path.

**Routing.** Where you choose what to spawn, pick the model for a `mechanical` task. Intent: route-model. If the session's tool map lists it, use that command for the `mechanical` kind and follow its `action` under the `routing.escalate` gate: on `ask` stop and ask the user, on `downgraded` (`deny`) use the lower tier it names, and in auto mode (`auto`) proceed and log the assumption. If the command fails, carry on. Otherwise do this: spawn each agent on the model its own agent file sets, and carry on.

- **`ask` mode:** behave exactly as this file describes.
- **`auto` mode:** follow the Auto path below.

### Auto path

In auto mode behave as `--stage`: collect and analyse, then write the candidates to the staging area and stop. The Phase 3 write question ("Write these to memory?") is skipped in that path, and nothing reaches the live store. Where Phase 0 would ask which JIRA key or Confluence space to use, take the most likely one and name it in the report. If `--from-staged` was also passed, stop with one line: "Auto mode does not promote staged facts; run /playbook:learn --from-staged --ask."

## Phase 0: Preflight and scope

Check the prerequisites. Intent: learn-preflight. If the session's tool map lists it, use that command. It prints `Repo:`, `Root:`, `Store:` and `Commits:` lines, then `gh:` and `acli:` probe lines (with `acli jira:` and `acli confluence:` auth lines when `acli` is present), then the JIRA project keys found in the last 500 commits as a count histogram. It exits 1 with `error: not in a git repo` outside a repository. Otherwise do this, and stop with that same error when the first command fails:

```bash
git rev-parse --show-toplevel
gh repo view --json nameWithOwner -q .nameWithOwner || git remote get-url origin
git rev-list --count HEAD
gh auth status >/dev/null 2>&1 && echo "gh: ok" || echo "gh: UNAVAILABLE (PRs skipped)"
if acli --version; then acli jira auth status; acli confluence auth status; else echo "acli: absent"; fi
git log --oneline -500 | grep -oE '[A-Z][A-Z0-9]+-[0-9]+' | sed 's/-[0-9]*$//' | sort | uniq -c | sort -rn | head -10
```

`REPO` is `<owner>/<repo>` (from the remote URL, drop the `.git` suffix, the scheme and the host) and the store is `~/.config/playbook/memory/<owner>/<repo>`.

Then, before collecting:

- **Atlassian access:** the `jira` and `confluence` collectors have no MCP tools, so brief them to use `acli` (they load `playbook:atlassian`) whenever `acli` is present and authenticated, even if `mcp__atlassian__*` tools exist here. When only MCP is available, run the Atlassian reads yourself and pass the results in the collectors' briefs. The `playbook:atlassian` skill carries the real command surface and the Confluence page-discovery workaround; load it before any Atlassian call, and load `playbook:writing-style` before any write. Jira and Confluence authenticate separately, so treat the two probes above independently: Jira reachable and Confluence not is a normal state, not an error. With neither, mark JIRA/Confluence **unavailable** and record it for the report.
- **Targets:** resolve the JIRA project key(s) from the histogram and the Confluence space from README/links. If ambiguous, ask the user once.
- **Capture:** `REPO`, `ROOT`, scope caps, and which sources are reachable. You need these in every later phase.

## Phase 1: Collect (parallel subagents)

On `--refresh` only, before dispatching, run the memory-context command for `$REPO`: its facts block (`name: description` lines), not the full fact bodies. Include the result in each collector's prompt so it can flag what's already documented instead of silently re-discovering it, and note anything that looks stale against what it finds. Skip this on a fresh (non-`--refresh`) run: there's rarely anything in the store yet, and this command's job is building it, not consuming it.

Collect the git history and the layout first. Intent: learn-collect. If the session's tool map lists it, use that command for `git` and `structure`. It counts the git history and reads the layout with no model, and prints one JSON object with a `git` key (contributors by name, churn hotspots, commit types and conventional-commit share, remote branch prefixes, recent tags, commits per month) and a `structure` key (top level, languages, entry points, build manifests with their scripts and targets, test and lint config, Docker, CI, IaC, migrations, model files, `scripts/`). Write the `git` value to `/tmp/learn-project/<owner>-<repo>/git-history.json` and the `structure` value to `code-structure.json`, so the analysts read the same files the collectors would have written. No collector is spawned for these two roles. Otherwise do this: add a `git-history` and a `code-structure` collector to the dispatch below, briefed to cover the same fields and to write those two files.

Dispatch the remaining collectors in parallel with `subagent_type: playbook:collector`. `collector` pins Haiku, the cost win this phase is built for. Each returns a compact structured summary (tight JSON or markdown) that cites paths/refs, NOT raw command output. Spawn each collector with a stable `name`. If the session entered a worktree, spawn each with `isolation: worktree` (collectors run `git` and `gh`), or call `ExitWorktree` with `keep` first.

- **pull-requests** (if `gh` ok): `gh pr list --state all --limit <MAX_PRS> --json number,title,labels,body,author`: recurring themes, review norms, linked JIRA keys, notable decisions.
- **jira** (if reachable): epics, active sprints/boards, components, common labels for the project key(s).
- **confluence** (if reachable): pages on setup/onboarding, architecture, runbooks, and decisions in the project space; capture titles, URLs, and key points.

## Phase 2: Analyze into topics (parallel subagents)

Feed the Phase 1 findings to three analysts, spawned with `subagent_type: playbook:analyst`, each owning a group of clusters. Spawn each analyst with a stable `name`. The findings are read once per spawn instead of nine times. Each analyst returns one section per cluster it owns, headed by the cluster name, so Phase 3 still reads facts per cluster. Each emits **candidate facts**, where each fact has: `title`, `body` (the fact, then Why, then How to apply), proposed `type` (`project` for repo knowledge, `reference` for external pointers), `scope` (`repo` | `global`), proposed `links` edges, and `anchors` (repo-relative code locations the fact describes: dirs, files, or `file#symbol`).

Analyst groups and their clusters:

- **Structure and language:** architecture & module map; conventions & patterns (design patterns adopted, build/test/lint, branching, commit/PR); domain glossary
- **History and operations:** decisions & active work (ADRs from PRs/commits + JIRA epics); infrastructure (CI/CD, deploy, cloud, IaC); setup (local dev and onboarding)
- **Tooling and data:** scripts & tooling; database schemas & models; data access patterns

Name the clusters of the group in the analyst's prompt, in this order. A repo with no database code or no JIRA still gets the group; its empty clusters come back as empty lists.

Keep facts atomic: one concept per fact. Drop low-signal or self-evident facts.

## Phase 3: Classify, dedupe, plan

- **Scope routing:** default `repo`. Mark `global` only when the fact is org/account-wide and not tied to this repo (company tooling, the Atlassian instance, standards seen across repos). A repo fact that contradicts a global one wins for this repo; note it with a `contradicts` edge.
- **Dedupe:** run the memory-context command for `$REPO` once: its facts block already covers both project-scoped and global-scoped facts for this repo (global-scoped facts are always included), so one call replaces both index reads. Load the relevant fact files it names. If it prints `Memory context: none` on a repo that already has facts, stop and report the broken read rather than writing possible duplicates. If a fact already exists: skip it, unless `--refresh`, in which case update the file or write a successor carrying a `supersedes` edge. Never blind-duplicate.
- **Plan:** show the user a concise table of candidate facts (title · scope · type · new/update/supersede). Ask once: "Write these to memory?" Proceed only on yes; honor a subset selection.

## Phase 4: Write memory

Project store, first time in this repo only:

```bash
mkdir -p "$STORE"
```

Then write each approved fact:

- One fact per file, kebab-case name, in the chosen store (`$STORE/` or `~/.config/playbook/memory/`).
- Frontmatter: `name`, `description` (one-line when-to-use), `type`, `links:` with bare-basename edges (`supersedes`, `depends_on`, `relates_to`, `contradicts`), and `anchors:` listing the repo-relative code locations the fact describes (`src/auth/`, `src/auth/login.py`, or `src/auth/login.py#authenticate`).
- Body: the fact, then **Why:** and **How to apply:**. Use absolute dates for anything time-bound (`date +%F`).
- In the project store, do NOT name the repo in the fact text; it's implicit.
- Write a `project-overview` fact as the entry point, linked via `relates_to` to the main topic facts.

## Phase 4.5: Rebuild the navigation graph

`~/.config/playbook/data/memory.db` holds a single graph covering every fact, global, org, and project. It rebuilds automatically: the `rebuild-memory-graph` PostToolUse hook fires whenever a fact file under `~/.config/playbook/memory/` is saved, so once Phase 4 has written the facts the graph is already current. Normally you skip this phase.

`--graph-only` forces a rebuild without re-collecting, for use after hand-editing fact files: run the memory-rebuild command. When the tool map lists memory-stats, the rebuild also prints its summary.

**Why a dedicated command rather than invoking the hook.** The hook skips unless the write it was told about is under `~/.config/playbook/memory/`, which is right for a PostToolUse hook and leaves no way to force a full rebuild. This used to run `python3 hooks/rebuild-memory-graph.py < /dev/null`, because that script treated empty stdin as "rebuild everything". The Rust port dropped that branch deliberately (see `should_skip` in `crates/cli/src/hooks/rebuild_memory_graph.rs`), judging it unexercised by the hook's test suite. It was exercised, by this command. Faking a `tool_input` payload that names a path inside the memory dir would also work and is what the port's own doc calls the more fragile option, since it breaks silently the next time the skip logic changes.

It walks every fact under `~/.config/playbook/memory/`, derives each fact's scope (`global`, `org` with its `owner`, or `project` with its `owner/repo`), and writes `~/.config/playbook/data/memory.db` in one transaction. The fact files stay the source of truth, so the database is safe to delete: the next rebuild makes it again. Nodes are facts plus their `anchors:` code locations; edges are the `links:` between facts and the fact→code anchors.

To read the counts, run the memory-stats command (`playbook memory stats`) when the tool map lists it. If it does not, say in the report that this playbook version has no stats command. The command is read only and prints the database path, size and last rebuild time, facts per scope, nodes, edges per relation, anchors, and the dangling edges and anchors. Do not open `memory.db` with `sqlite3`: the stats command already has the answer.

## Staging mode (`--stage` and `--from-staged`)

These split collection from the write decision, so a run can happen unattended (for example, nudged at session end) and the human approves later. The staging area is `$STORE/staging/`, inside the central memory store.

**`--stage`** (collect now, decide later):

1. Run Phases 0-2 as normal to produce candidate facts.
2. Skip Phase 3's confirmation and Phase 4's live writes. Create `$STORE/staging/`, then write each candidate to `$STORE/staging/<kebab>.md` in the normal fact format, plus two extra frontmatter fields: `status: pending` and `staged: <date +%F>`, a `scope:` (`repo` | `global`), and, when it would update an existing fact, a `supersedes:` note.
3. Write or refresh `$STORE/staging/STAGED.md` with one `- [Title](file.md): one-line hook` line per candidate.
4. Do NOT touch the live `memory.db`.
5. Report the count staged, the staging path, and: "Review with `/playbook:learn --from-staged`."

**`--from-staged`** (review and promote):

1. Skip Phases 0-2. Read every candidate in `$STORE/staging/`.
2. Run Phase 3 against them: show the candidate table, dedupe against the live stores, and ask once "Write these to memory?" (honor a subset).
3. For approved candidates, run Phase 4 (write to the live store, dropping the `status`/`staged` staging fields; apply `supersedes`/updates) and Phase 4.5 (rebuild `memory.db`).
4. Remove promoted candidates from staging. Leave any the user skipped; delete any the user rejects.
5. Report as in Phase 5.

## Phase 5: Report

One tight summary:

- Facts written / updated / superseded, per cluster and per store.
- Sources used, and **sources skipped with the reason** (e.g. "Confluence: no MCP, acli or twg").
- The output of the memory-stats command (`playbook memory stats`), as printed, or a note that this version has none: the `~/.config/playbook/data/memory.db` path, counts, and any dangling anchors or edges. Do not count with `sqlite3`.

## Teardown (MUST run, even on failure or abort)

Close any subagent from this run that is still alive before finishing.

## Anti-patterns to refuse

1. Dumping raw `git log` / PR / JIRA output into memory. Facts are distilled, atomic, and actionable.
2. Silent skips. An unreachable source or applied cap must appear in the report.
3. Duplicating an existing fact instead of superseding or updating it.
4. Writing repo-specific detail into the global store, or cross-project facts into the project store.
5. Editing project code or config. Memory files under `~/.config/playbook/memory/` are the only writes.
6. Persisting secrets or tokens pulled from configs or CI.
7. Leaving `memory.db` stale. Rebuild it whenever facts change.
