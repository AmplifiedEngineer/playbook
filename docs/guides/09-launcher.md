# The launcher

`playbook cc` starts Claude Code with the playbook system prompt (it replaces Claude Code's built-in prompt and never appends to it), the right model chain and your effort ceiling. `playbook ccd` is the same with permission prompts skipped.

`pb` is a short alias for `playbook` (`pb cc`, `pb ccd`). install.sh, Homebrew and `playbook setup` link it next to the binary when the name is free and the binary sits in the install folder. No shell line is needed.

Turn the prompt off with `playbook setup --no-system-prompt`. `pb cc` then starts with Claude Code's own prompt. A plain `claude` never loads the playbook prompt. The old `ccc` and `ccd` shell functions are gone. `playbook doctor check` finds leftovers and names the replacement.

```bash
playbook cc                     # resume this directory's last session, or start fresh
playbook cc fresh               # new session, no history
playbook cc list                # recent sessions for this directory
playbook cc launch worktree <branch>   # create or enter a git worktree, then start a session there
playbook cc clean               # resume with /model, /effort and similar settings stripped
playbook cc raw [id]            # resume verbatim, no fork or cleanup
playbook cc prune               # prune old transcripts now
```

The work happens in the binary, so bash and zsh behave the same. A program cannot change its parent shell's directory, so after a session in a new worktree `playbook cc` prints the `cd` command to run. Worktrees land in `<repo-parent>/.worktrees/<repo>/<folder>`.

See [worktrees](../internals/03-worktree.md) and [launcher and hooks](../internals/01-launcher-and-hooks.md).
