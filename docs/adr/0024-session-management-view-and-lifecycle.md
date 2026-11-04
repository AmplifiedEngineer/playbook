# ADR-0024: A Session Management View With a Derived Record and a Suggest-First Lifecycle

- **Status:** Accepted
- **Date created:** 2026-10-11
- **Date modified:** 2026-11-04

## Context

Issue #536 asks for a view of every running and recent Claude session, a link from each session to the work it was started for, and a lifecycle that wraps a session up when that work is done. Today, sessions pile up as open tabs, and nothing knows that a session, its branch and its PR belong together.

### What exists (checked against `origin/main` at `ac4cb166` and Claude Code 2.1.293)

| Piece | Where | What it knows |
| --- | --- | --- |
| Usage store | `src/usage/db.rs`, `src/usage/entry.rs` (`UsageEvent`) | Per message: session id, agent, model, repo name, git branch, the four token counts, cost. No working directory, no PR, no pane. |
| Live and Sessions panels | `src/usage/tui/panels.rs`, `src/usage/query.rs` | Live lists sessions with a message in the last 15 minutes (`ACTIVE_SECONDS`). Its `state` column is only `live` or `idle`, from the age of the last message (`session_state`). Sessions lists sessions per UTC day. Both are read only. |
| Session listing | `src/cc/sessions.rs` | Per directory: UUID named transcripts, newest first, resumed by `customTitle`. No state, no PR. |
| Hooks | `src/hooks/session_init.rs`, `session_clean_exit.rs` | `SessionStart` resets per session state and loads handoffs. `Stop` refreshes `last-clean-ts` on every turn. `SessionEnd` with a reason other than `other` writes the `clean-exit` marker (and `mark_cleared` for `clear`). State lives in the per session directory from `session_dir`. |
| Hook wiring | `hooks/hooks.json`, `src/init/wire.rs` | The plugin file registers `SessionStart`, `WorktreeCreate` and `WorktreeRemove`. `wire.rs` writes `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `PreCompact`, `Stop` and `SessionEnd` into `settings.json`. Nothing registers `Notification`. |
| Worktree cleanup | `src/worktree/mod.rs` (`classify`, `named_branch_landed`, `sweep`, `remove`), `docs/internals/03-worktree.md` | Removes worktrees whose branch landed (merged PR, merged into the default branch, or stale past `worktreeCleanup.staleAfterDays`) and are not locked or in use. Never removes a dirty tree. Runs from `cc worktree` housekeeping and once a day from `SessionStart`. Logs removals to `worktree-sweep.log`. |
| Handoff | `src/handoff/mod.rs`, command `/playbook:session-handoff` | `playbook handoff save` writes markdown keyed by repo slug, loaded by the next `SessionStart`. |
| Telemetry | `src/usage/otlp.rs`, `telemetry_db.rs`, `src/cc/telemetry.rs` | Optional per request rows (session id, request id, model, tokens, cost) and a `session_sources` table. |
| Codex | `src/usage/codex.rs` | Codex rollouts ingest into the same store, with `session_meta` giving id, cwd and branch. |
| tmux | nothing | `grep -ri tmux` over `src`, `docs`, `hooks` finds no code. |

[ADR-0020](0020-usage-terminal-ui.md) already fixed that the TUI and the JSON output share one query layer (`src/usage/query.rs`) over the store. [ADR-0015](0015-multi-agent-support.md) fixed that the core stays agent neutral, with an adapter per agent.

### What Claude Code gives us (official hooks reference, version 2.1.293 installed)

- Every hook input carries `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `permission_mode`.
- `SessionStart` has `source` (`startup`, `resume`, `clear`, `compact`, `fork`), an optional `model` and an optional `session_title` (only when a custom title is set). On `resume` and `fork` it adds `context_tokens` and `seconds_since_last_response` (v2.1.251 and later).
- `SessionEnd` matcher values are `clear`, `resume`, `logout`, `prompt_input_exit`, `other`. Its hooks share a 1.5 second budget unless a longer `timeout` is set.
- `Notification` has a `notification_type` matcher with `permission_prompt`, `idle_prompt`, `agent_needs_input`, `agent_completed` and others. This is the only hook signal for "waiting on you".
- `Stop` carries `last_assistant_message`. The transcript file is written asynchronously and can lag the hook.
- `CLAUDE_PROJECT_DIR` is set for hooks and stays fixed after the session enters a worktree. Hooks inherit the parent environment, so `TMUX` and `TMUX_PANE` should pass through. The docs do not promise it, so the design treats the pane as optional and verifies it with a logged `env` before the tmux stage.

### What it cannot do

- No hook carries a pid, a tty or a terminal or tmux identity.
- A killed terminal or a crash fires no `SessionEnd`. The existing `clean-exit` marker already exists because of this.
- There is no event for "PR merged". That signal comes from `gh` or git, not from the agent.
- No hook can close a Claude Code tab, and a hook cannot drive another session. Closing a session means stopping a process or a pane from outside.
- A session thinking without writing a message yields no transcript line yet (known limit in `docs/internals/04-usage-dashboard.md`).

## Decision

### 1. One derived record, no registry in the first stages

A **session record** is computed on read. It is not a new store.

| Field | Source |
| --- | --- |
| `session_id`, `agent`, `last_activity`, `cost`, `model` | Usage store (Claude Code and Codex share it). |
| `repo`, `branch` | Usage store (latest event of the session). |
| `cwd`, `worktree` | The first transcript line that carries `cwd` (found by session id under the agent's session directory), then `git worktree list --porcelain` from that directory. The `classify` convention is reported (launcher, agent, review, other). |
| `context_size` | Input plus cache read plus cache write tokens of the newest event of the session, which is the prompt size of the latest request. |
| `linked_issue` | A number in the branch name (`536-foo`, `feat/536-foo`, `issue-536`), shown as inferred. |
| `linked_pr`, `pr_state` | `gh pr list --head <branch> --state all` with the existing timeout helpers, cached for the life of one refresh. An open PR wins over any other. A merged PR counts only when it targeted the default branch and its head is the local branch tip. `None` when `gh` is missing or offline. |
| `task` | A short hash of the first user prompt, never the text. It tells two sessions on one branch apart and detects a resume. |
| `pane` (optional) | `TMUX_PANE` recorded by a hook (stage 3), matched to `tmux list-panes -a`. |

Why derived: the store already has the session id, repo, branch, cost and tokens. A second store would need hooks to keep it fresh and would drift from the transcripts. The cost is that a field a hook alone can see (pane, waiting) needs a small hook written file. That file is stage 3 and is the only write path, keyed by session id under the existing session directory (`session_dir`), mode 0700 like today.

### 2. States

| State | Rule |
| --- | --- |
| `working` | Last activity under 2 minutes ago, or (stage 3) a `Stop` has not followed the last `UserPromptSubmit`. |
| `waiting` | (Stage 3 only) a `Notification` of type `permission_prompt`, `idle_prompt` or `agent_needs_input` is newer than the last activity. Before stage 3 the state is not produced, and the view says `idle`. |
| `idle` | Last activity under `sessions.staleAfterDays` and not done. |
| `stale` | No activity for `sessions.staleAfterDays` (default 3) and not done. |
| `done` | Any done signal below. |

Waiting and working are timing guesses in stage 1 (a message in the last 2 minutes reads `working`). The view labels them as such in the docs. A closed state exists only once a lifecycle action ran (stage 4) and is a record in the same hook file.

### 3. Done signals and the caution each needs

| Signal | Meaning | Confidence | What it may trigger |
| --- | --- | --- | --- |
| PR for the branch is merged into the default branch, at the branch tip | The work landed | High | Suggest close. Safe cleanup via the existing sweep. |
| PR closed unmerged | The work was dropped | Medium (may be re opened) | Suggest close and handoff. No cleanup. |
| Branch merged into the default branch locally (`merged_branches`) | The work landed without a PR | Medium | Suggest close. |
| Linked issue closed | The ask is done | Medium (can be a different PR) | Suggest only. |
| Branch or worktree gone | The work place is gone | Medium | Suggest close. |
| No activity for N days | Abandoned maybe | Low | Mark `stale`, list as needing attention. Never an action on its own. |

Linking uses the branch first. A session whose branch is the default branch (`main`) has no PR link and is never `done` by a PR signal, because the default branch always has merged history.

### 4. What the lifecycle may do

`sessions.autoClose` has three levels, off by default.

- `off`: the view and `sessions list` only. Nothing is suggested or run.
- `propose`: the view and a one line hint at `SessionStart` show what could be closed and why. Nothing runs. This is the default when the feature ships.
- `auto`: only the actions in the "automatic" column, only when every guard holds.

| Action | `propose` | `auto` |
| --- | --- | --- |
| Show "close this tab" or "archive" suggestion | yes | yes |
| Save a handoff for the session | suggest | no. The agent writes a handoff, a hook cannot. It stays a suggestion. |
| Remove the worktree | suggest | yes, only through the existing `worktree::remove` and `sweep` rules: landed, clean, not locked by a live process, not in use. Deleting the branch is a later stage. |
| Mark the record closed | suggest | yes |
| Stop the process or close a tmux pane | never | never without a confirmation in the view |
| Anything on a tree with uncommitted or untracked changes, or unpushed commits | never | never. The session goes to a needs attention list. |
| Anything on a session with activity in the last 15 minutes | never | never |

No lifecycle code path may call `git reset`, `git checkout --`, `git clean` or `git branch -D`. Cleanup is the existing `worktree::remove` and `sweep`, and the existing `worktree-sweep.log` records what it took. Those do call `git worktree remove --force`, so the safety rests on the checks that run first: landed, not in use, clean (untracked files count) and not locked by a live process. The `--force` only lets git drop a tree that has passed those checks, ignored files included. A new lifecycle path must go through the same checks and never add its own forced removal.

Where it runs: a read in the view and in `sessions list` (always), and the existing daily `SessionStart` sweep (`maybe_sweep_worktrees`) for the `auto` cleanup of worktrees. No new daemon and no OS scheduler. A repo nobody opens is not swept, the same known limit as today. Watch mode (#534) can call the same function later.

### 5. Surface: a command and a TUI view over one core

One module (`src/sessions/`) builds the records and has no terminal code. Two thin surfaces use it.

1. **`playbook sessions`** (alias `sessions list`), text by default and `--json` for scripts and for a session with no TTY. First, because it is scriptable, fits the ADR-0020 rule that non-interactive output stays, and has no merge conflicts with the TUI work in flight.
2. **A Sessions management view in the usage TUI**, as a separate module under `src/usage/tui/`, toggled with one key, grouped by repo or state, sorted by last activity. It reads the same core, so the two never disagree.

Why not only the TUI: the usage TUI is about spend over a range, with a store query. Session management is about now and about git and `gh` state, which are slow and can fail. A command can show a partial row (`pr: unknown`) and exit, and a script can read it. Why not only a command: the issue wants one key to open or close, and a live view is the place for that. The first needs no new dependency, the second reuses ratatui.

The view columns: repo, branch, worktree (short path), issue or PR (`#536`, `PR 812 merged`), state, last activity (relative, UTC in the footer like the rest of the TUI), context size, cost. Footer keys follow the usage TUI. All times are UTC, the theme is the existing `usage.theme`.

### 6. tmux (stage 3, optional)

- Read only: `tmux list-panes -a -F '#{pane_id} #{session_name}:#{window_index} #{pane_current_path} #{pane_pid}'`, matched to a session by the `TMUX_PANE` a hook recorded, falling back to `pane_current_path` equal to the worktree. A fallback match is labeled inferred.
- Without tmux (plain tabs, IDE terminals, `TMUX` unset) every field degrades to empty and the view works the same. Nothing requires tmux.
- Never `select-pane`, `select-window` or `switch-client`, so the view never steals focus. Labeling a pane (`select-pane -T`) is the only write and is off by default.
- `kill-pane` and `kill-window` only after a confirmation in the view naming the pane, and never for a pane whose process tree holds a running agent with activity in the last 15 minutes, or whose tree is dirty.

### 7. Privacy

- No prompt, response or tool content is stored or printed. The task link is a hash of the first prompt, computed in memory while the transcript head is read.
- Branch names, repo names and paths are shown on the local terminal and in `--json` on stdout. Nothing is sent anywhere. The hook file in stage 3 holds the session id, pane id, timestamps and notification type, never the message text.
- `gh` is called only for the branches in the view and only reads PR number and state.

### 8. Config keys (new, documented in `docs/guides/04-config-keys.md` when each lands)

| Key | Default | Values | Stage |
| --- | --- | --- | --- |
| `sessions.autoClose` | `propose` | `off`, `propose`, `auto` | 4 (hint), 5 (`auto`) |
| `sessions.staleAfterDays` | `3` | whole number, 1 or more | 1 |
| `sessions.recentDays` | `7` | whole number, 1 or more | 1 |
| `sessions.closeRemoves` | `worktree` | `none`, `worktree`, `worktree,branch` | 5 |
| `sessions.tmux` | `auto` | `off`, `auto` | 3 |

### 9. Rollout

| Stage | Adds | Behavior change |
| --- | --- | --- |
| 1 | `src/sessions/` core and read only `playbook sessions` with text and `--json`. States `working`, `idle`, `stale`, `done` from activity and PR state. | None. Nothing is written. |
| 2 | Sessions management view in the usage TUI (separate module). Needs attention group. Suggestions shown, never run. | None. |
| 3 | A `Notification` and `UserPromptSubmit` hook writing a small per session file (pane id, last notification type). States `waiting` and exact `working`. tmux read mapping. | New hook entry, new small file. |
| 4 | `sessions sweep --dry-run` and the `propose` hint at `SessionStart`. | A one line message. |
| 5 | `auto` for the safest action first: closed or merged PR plus clean worktree, through the existing sweep. Then branch delete, then record close. | Removes worktrees, as the sweep already does. |

**The first slice is small and safe:** stage 1 only. It reads the usage store, a transcript head, `git worktree list` and `gh pr list`. It writes nothing, adds no hook and no dependency, and every part it uses is tested code. Stage 2 follows as its own change.

### 10. Codex

The record is agent neutral. `agent` comes from the store and Codex rows already carry id, repo and branch. Codex has hooks (`SessionStart`, `Stop`, per ADR-0015, unverified for this use), so stage 3 for Codex is an adapter question and a follow up. Cost for Codex shows as unpriced, as today.

## Alternatives considered

- **A persisted session registry written by hooks, `cc` and `pr create`.** Gives pane and waiting from day one. Rejected for the first stages: it adds three writers that can disagree with the transcripts, and a crash leaves it stale. The derived record covers most fields, and stage 3 adds only the two fields a hook alone can see.
- **A tab in the usage TUI only.** Rejected as the first step. The TUI is built around a date range over the store. Session state needs slow `git` and `gh` calls that the TUI worker would have to isolate, and the TUI work in flight (#769) makes a large change there costly to merge. It comes second, over the same core.
- **A command only.** Rejected: the issue wants a live, one key view.
- **Closing sessions by killing the process.** Rejected. There is no pid in any hook payload, and a kill can lose uncommitted work.
- **A background daemon that watches PRs.** Rejected for now: no scheduler exists, and a daemon is a new thing to install and trust. The daily `SessionStart` sweep and watch mode (#534) cover it.
- **Task text from the first prompt.** Rejected for privacy. A hash plus the branch and issue number identifies the work without storing content.

## Consequences

- Stage 1 and 2 are read only and reversible by deleting the module.
- Linked issue is a heuristic from the branch name. It can be wrong, so it is labeled inferred, and a recorded link (`playbook sessions link <issue>`) is a follow up.
- `gh` calls add latency to a read. They run with a short timeout, in parallel, once per branch, and a failure shows `unknown`, never blocks.
- `waiting` is not available before stage 3. The view says so rather than guessing.
- The `done` signal for a PR merged in the last minutes depends on `gh` and GitHub propagation.
- A resumed or forked session keeps its id or gets a new one (`source` is `resume` or `fork`). The record follows the branch, so two ids on one branch show as two rows with the same link.

## Risks

| Risk | Mitigation |
| --- | --- |
| A session wrongly called done and its worktree removed | Cleanup is only the existing `worktree::remove` guards (landed, clean, unlocked), only at `auto`, off by default. A merged PR plus clean tree is the first and only automatic case. |
| Uncommitted work lost | No new forced git command. The existing removal runs only after the clean, landed, not in use and unlocked checks. A dirty tree moves the session to needs attention. |
| `gh` missing, offline or rate limited | `unknown` link, no action. |
| Hook payload changes between Claude Code releases | Fields are read as optional and a missing field degrades the row. Stage 3 is verified against the installed version in its PR. |
| Transcript layout changes | The cwd lookup falls back to the repo name from the store. |
| Slow read with many sessions | Bounded by `sessions.recentDays` and a per refresh cache. |

## Follow up issues (not created)

1. Stage 1: `playbook sessions` core and read only list (the first slice).
2. Stage 2: Sessions management view in the usage TUI, with the needs attention group.
3. Stage 3a: `Notification` and `UserPromptSubmit` hook files for `waiting` and exact `working`.
4. Stage 3b: tmux pane mapping and optional pane label.
5. Stage 4: `playbook sessions sweep --dry-run` and the `propose` hint at `SessionStart`.
6. Stage 5: `sessions.autoClose = auto` for merged PR plus clean worktree, then branch delete.
7. `playbook sessions link <issue>` for a recorded link, and `cc worktree` and `pr create` recording it.
8. Codex adapter for the pane and waiting fields (#533).
9. Log `env` from a test hook to confirm `TMUX_PANE` reaches hooks.
10. Wire `Notification` in `wire.rs` and add it to the migration framework (ADR-0016).
