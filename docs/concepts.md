# Concepts

The few ideas you need to read the rest of the Playbook docs: what the plugin and the binary each do, the four kinds of building block, the seven layers, memory and the code index. It is for someone who has finished [Get started](get-started.md) and wants the picture behind it.

## When to use it

- You wonder why Playbook has both a plugin and a binary.
- You want to know where a new rule or procedure belongs.
- You see a layer or a hook in a report and want to know what it is.

## The plugin and the binary

Playbook ships in two parts that work together.

- The **plugin** is for Claude Code. It holds the slash commands, the skills and the subagents. It works on its own, and it writes nothing to `~/.claude`.
- The **binary**, `playbook`, runs on your machine. It runs the hooks and the safety guards, the launcher (`playbook cc`), the status line, the usage dashboard, the memory store and the code index.

The plugin without the binary gives you the skill text only. Hooks are bare `playbook hook <name>` commands, so without the binary they do nothing. The [install guide](guides/00-install.md#pick-an-install-mode) compares the three modes. `playbook setup` wires the binary into Claude Code and shows each change before it acts.

## Commands, skills, agents and hooks

| Block | What it is | Example |
|---|---|---|
| Command | A multi-step procedure you start | `/playbook:plan` |
| Skill | Shared knowledge that many procedures load by name | the writing style rules |
| Agent | A role with its own model, effort and tool limits | the read-only `reviewer` |
| Hook | A rule that runs on a tool call or a session event, with no one asking | the rm guard |

The split follows one question: who has to remember to do it? A hook runs without anyone remembering, so safety rules live there. A command holds the order of steps. A skill holds knowledge once, so copies do not drift. An agent exists when a plain prompt cannot give a restricted tool set or a pinned model. [The reasoning behind the pieces](internals/14-why-the-pieces-are-shaped-this-way.md) gives the reasons, and [Authoring](authoring/01-commands-skills-hooks.md) shows how to add one.

## Layers

`/playbook:doctor`, or `playbook doctor check` in a terminal, reports seven layers: the plugin, the guards and settings, the launcher, the system prompt, the status line, the binary, and a check for hooks that point at missing files. A working install needs the plugin, the guards and the binary. The [layers page](reference/layers.md) lists all seven.

## Plan to PR

Feature work runs in two steps. `/playbook:plan` turns an idea into a checked plan made of Work Units grouped into Segments. `/playbook:implement` builds it. Each Segment becomes one small pull request, and each Work Unit becomes one commit. [Plan and implement](guides/01-plan-and-implement.md) shows the full flow.

## Memory

Playbook keeps a local store of facts in `~/.config/playbook/memory/`, in three scopes: global, per organization and per repo. Sessions recall the facts that match the work. The store is separate from Claude Code's own memory. Playbook does not write Claude Code's `CLAUDE.md` files or auto memory, and it does not commit the store. See the [memory guide](guides/15-memory.md).

## The code index

`playbook code` builds a tree-sitter index of a repo's source and answers questions from it: search, symbol, outline, read, refs and context. One index serves every worktree of a repo, so a new worktree does not start from nothing. Sessions run `playbook code status` before a code question, and `playbook code index` when the index is missing or stale. [ADR 0056](adr/0056-share-one-code-index-across-worktrees.md) records the choice to share one index, and the [command reference](reference/cli-and-config.md#playbook-code) lists every subcommand.

## Next

- [Get started](get-started.md) if you have not installed Playbook yet.
- [Docs map](README.md) for every guide, reference page and internals page.
