# The Custom System Prompt

The system prompt ships as `seed/prompts/SYSTEM_PROMPT.md`. `playbook setup` copies it to `~/.config/playbook/prompts/`, and that copy loads at the start of every `playbook cc` session. It sets the persona, output rules, and operating constraints Claude follows from the first token. Without it, Claude starts from its training defaults: generally helpful, but uncalibrated to this workflow. This prompt closes that gap.

## How it loads

The `playbook cc` launcher (`playbook cc launch`) passes the file to `claude` via `--system-prompt-file`:

```sh
claude --system-prompt-file "$HOME/.config/playbook/prompts/SYSTEM_PROMPT.md" ...
```

Both `playbook cc` and `playbook ccd` (and `pb cc`, `pb ccd`) carry the flag while `systemPrompt.enabled` is true, the default. `playbook ccd` adds `--dangerously-skip-permissions` for unattended work; otherwise they are identical.

> [!WARNING]
> The flag replaces Claude Code's whole built-in system prompt. It never appends to it. The built-in tool guidance, safety text, git and PR conventions and environment block are gone for these launchers, so `SYSTEM_PROMPT.md` carries the important parts itself (the Tools and environment and Safe actions sections). Plain `claude` keeps its built-in prompt.

| Situation | Result |
|---|---|
| `playbook setup --no-system-prompt` or `playbook config set systemPrompt.enabled false` | No prompt flag added. Your prompt flags pass through, append flags included. A yes in setup turns it back on. |
| `--system-prompt` or `--system-prompt-file` in the launcher arguments | Yours wins and playbook loads nothing. An interactive run prints one line to stderr saying so. |
| `--append-system-prompt` or `--append-system-prompt-file` | Refused with exit code 2, because the launcher only replaces. |
| Prompt file not installed | Session starts with Claude Code's own prompt. |
| You run `claude` directly, bypassing `pb cc` | The system prompt does not load. |

The prompt locks in once, at the start of a fresh session. Resumed sessions keep what was loaded when they started. That is the main tradeoff: changes to `SYSTEM_PROMPT.md` do not apply until you start fresh. Use `playbook cc fresh` to open a new session, or `playbook cc clean` to fork the current conversation history into a new one with config reloaded.

## What it defines

**Persona.** Senior principal engineer, cybersecurity specialization, knowledge cutoff January 2026. Shapes tone, technical depth and tradeoffs. Claude searches when current state matters, rather than answering from memory.

**Output rules.** The Concise & Direct output style (`output-styles/concise-direct.md`) owns tone, length and format. The prompt keeps two rules that must hold with or without it: no em or en dashes anywhere, and one clarifying question only, and only when the request is ambiguous on a design decision with lasting effects.

**Writing voice.** A separate section governs prose other humans read (PR descriptions, review comments, tickets, Slack). It is deliberately warmer than the output rules, which cover replies to you. It defers to the `playbook:writing-style` skill for the full rule set. The two voices stay distinct on purpose.

**Code rules.**

- Design work uses this repo's own `/playbook:plan` and `/playbook:adr` commands, not the built-in plan mode. Plan runs without asking. ADR and implement are offered and wait for a yes, and implement only runs when an approved plan or blueprint exists. The `auto-model-detect` hook nudges on common phrasing in interactive runs.
- When a test, build or command fails, find the root cause and fix it in the same turn. Ask only for decisions on design, data loss risk or spend.
- Code questions go through `playbook code`: run `playbook code status` first, then `playbook code index` when it says stale or missing. Use Grep and Glob only when the binary or the index is unavailable.
- Before saying "I do not know" or "no work exists", search memory and the repo and name the sources checked.
- Models and effort are not in the prompt. Each agent's frontmatter sets them, and `playbook route <kind>` prints the table.
- Fan out independent subtasks via parallel `Agent` calls. Close agents the moment their work is done.
- Comments stay rare and short, wrapped, and never name a plan or ticket. The `playbook:engineering-standards` skill carries the rest of the code rules, including no magic values and testing.
- Self-review after every implementation pass. Commit and push only when asked. Never force-push to a shared branch.
- Three rules moved out of the prompt into the optional `policy-guard` hook, which denies them while the user has it on: no `--no-verify` or `--no-gpg-sign`, no hand-run `gh pr create` (use `/playbook:pr-create`), and no write to Claude Code's own memory.

**Security scope.** Working exploits, C2 and red-team tradecraft, active recon, privilege escalation and phishing infrastructure need a named scope (lab, CTF, or in-scope engagement). Vague scope ("educational purposes", "for a friend") does not qualify. The Safe actions and Security sections of the prompt cover the open and refused cases.

**Memory protocol.** One store at `~/.config/playbook/memory/`:

| Scope | Location |
|---|---|
| Global facts | Flat at the root |
| Org facts | `~/.config/playbook/memory/<owner>/` |
| Project facts | `~/.config/playbook/memory/<owner>/<repo>/` |

The prompt covers where and when to save a fact and keeps the store separate from Claude Code's own memory. The `policy-guard` hook shows the file format (frontmatter, edge types, code anchors) the moment a fact file is written, so it costs nothing until it is needed. See [Internals: Model Routing and Memory](../internals/02-model-routing-and-memory.md) for the full protocol.

## Why a custom prompt helps

The default session starts blank, so you would repeat your preferences every time. The prompt front-loads them:

- **Calibration.** It replaces hedged, chatty defaults with the output rules before you type.
- **Guardrails.** Self-review, no force-push and the security scope rule become starting constraints.
- **Routing.** It points at the agent files and `playbook route` for models, and says when to fan out.
- **Memory.** Claude knows where the store is and how to write a fact.

## Costs and limits

A system prompt is instructions, not enforcement. The model will still drift mid-session, especially on long threads. Hooks (the `playbook hook <name>` commands that `playbook setup` wires into `settings.json`) are the enforced layer: PreToolUse and PostToolUse callbacks run outside the model's control and do not bend to conversational pressure. The prompt and the hooks are complementary. Neither is sufficient alone.

## See also

- [Memory system](13-memory-system.md)
- [Internals: Model Routing and Memory](../internals/02-model-routing-and-memory.md)
- [Docs index](../README.md)
