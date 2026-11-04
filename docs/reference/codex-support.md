# Codex support

What works in Codex today, what does not, and why. Read from the Codex docs (learn.chatgpt.com and developers.openai.com) on 2026-10-20 and from this repo's code. Nothing here was run in a live Codex, so `[unverified]` marks every claim that needs one. Codex moves fast: check a row against the current Codex docs before you rely on it.

Playbook runs best in Claude Code. Codex gets the skills and, if you register them, some of the guards.

## Matrix

| Piece | Status | Why |
|---|---|---|
| Skills and commands | Works | `playbook skills sync` copies skills, and commands rendered as skills, to `~/.agents/skills/playbook-*`. See [Codex skills](../internals/15-codex-skills.md). Whether Codex follows the one-line pointer in a split stub to `references/playbook-body.md` is `[unverified]`. |
| Bash guards as hooks | Registered by `setup --only codex-hooks`, except the sanitizer | `rm-workspace-guard`, `precommit-check` and `commit-message-sanitizer` read `tool_input.command` and print JSON shapes that Codex documents. Tests feed them Codex-shaped payloads (`crates/cli/tests/hooks_codex_payloads.rs`). `playbook setup --only codex-hooks` registers the first two. The sanitizer is not registered. |
| Edit and read hooks | Not supported | Codex has no `Read` tool, and `apply_patch` carries patch text in `tool_input.command` with no `file_path`. These hooks would need a patch parser. |
| `policy-guard` | Not supported | Its rules guard Claude paths such as `~/.claude/projects/*/memory` and `.claude/settings*.json`. Registering it would look like protection and do nothing. |
| `session-init`, `usage-warm` | Not supported | `session-init` reads `~/.claude`, runs the Claude plugin update and writes Claude restart hints. Not safe to register as is. |
| `memory-capture`, `session-clean-exit` | Not supported | `memory-capture` fires on a marker the Claude statusline writes. Codex has no statusline. |
| `auto-guard`, `auto-cost`, `auto-model-detect`, `bg-await-guard` | Not supported | They depend on `AskUserQuestion`, Claude model aliases and `run_in_background`, which Codex does not have. |
| `WorktreeCreate`, `WorktreeRemove` | Not supported | Codex has no worktree event. |
| Agents | Not rendered | Codex has subagents and custom agent TOML files, but no per-agent tool allowlist, and a subagent inherits the parent sandbox. Playbook agents such as the critic, fact-checker and reviewer rely on tool restriction to be read-only, which Codex cannot enforce. The command preamble tells the agent to do the step itself or ask in chat. |
| System prompt | Not written | Codex reads `~/.codex/AGENTS.md`. Playbook does not write to it. Skills carry the rules. |
| Doctor | Codex row | `playbook doctor` reports whether Codex is found and whether the guards are in `~/.codex/hooks.json`. It cannot see whether you trusted them in `/hooks`. |
| Usage | Works, unpriced | `CodexSource` reads `~/.codex/sessions/**/rollout-*.jsonl`. It has no OpenAI price table, so cost shows as unpriced. Compressed `.jsonl.zst` rollouts are skipped. Codex 0.163.0 still compresses older rollouts: on one machine, 17 of 21 rollouts were `.jsonl.zst` and only the recent ones were plain, so history Codex compressed before playbook read it is not counted. A rollout named `<parent>_<child>` carries the parent's session id in its `session_meta` line. Each event is keyed by session id, time and running total, so a line repeated in a child file is stored once. On one machine the child's totals continued from the parent's last total. Whether a rollout made by a user fork repeats the parent's events has not been recorded, so it is `[unverified]`. The `agent` split is in `playbook usage --json` (`groups.agent`) only, not in the text summary or the terminal view. |
| Plugin distribution | Skills sync only | Codex reads Claude-compatible plugin manifests. Whether it loads the playbook plugin folder directly is `[unverified]`. |
| MCP | Nothing to port | Playbook ships no MCP server. |

## Registering the guards

`playbook setup --only codex-hooks` adds two entries to `~/.codex/hooks.json`, after a backup, and keeps every other entry: `rm-workspace-guard` and `precommit-check`, both on `PreToolUse` for `Bash`. It runs only when Codex is found, and `--yes` leaves it off. Open `/hooks` in Codex afterwards and trust the entries. `playbook uninstall` removes them. The sanitizer is not registered until a live Codex confirms its rewrite is applied.

## How the guards behave in Codex

- A hook that errors, times out or returns malformed output is reported and the tool call continues. Only an explicit deny or exit code 2 blocks. The Codex docs call hooks "a useful guardrail, not a complete enforcement boundary". Treat the guards as best effort.
- The guards allow when stdin is empty or not JSON. ADR-0015 says the hooks adapter must fail closed on a payload it cannot parse, so this is a known gap, not the intended design. The guards also read only `tool_input.command` and ignore `tool_name`, so an `apply_patch` call is allowed unless its patch text holds a command the guard denies. Tests pin both.
- A hook is skipped until you review and trust its exact definition in `/hooks`.
- `commit-message-sanitizer` rewrites a command by printing `updatedInput` and context, with no `permissionDecision`. The Codex docs describe a rewrite as `permissionDecision: "allow"` with `updatedInput`. Whether Codex applies the form playbook prints is `[unverified]`, and so is whether an allow would skip Codex's own approval prompt for the rewritten command. Do not rely on the sanitizer in Codex until both are checked.
- `precommit-check` has no `if` filter in Codex, so it runs on every Bash call and exits quietly when the command is not a commit. Whether Codex rejects or ignores an `if` field is `[unverified]`.

## Test fixtures

The payloads in `crates/cli/tests/fixtures/codex/` are schema-derived from the Codex hooks docs, not recorded from a running Codex. Each file says so in its `_fixture` header. A recorded `PostToolUse` payload, with the Codex version, is still missing.

## Cursor

Cursor loads `~/.agents/skills/` and `.agents/skills/`, so `playbook skills sync` reaches it too, but this is not verified in a running Cursor. The rendered preamble names `$playbook-<command>`, which is Codex syntax. Cursor hooks use other event names and payloads (`beforeShellExecution` and others), so they would need their own adapter, which is not planned. No public Cursor transcript format is known, so Cursor usage is not supportable today.
