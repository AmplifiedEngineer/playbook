# How to start a Claude Code session with Playbook

<!-- verify: names -->

Start Claude Code with the playbook prompt, the model chain and your effort ceiling, then find the session again later.

**You need:** the `playbook` binary and the `claude` command on your `PATH`.

1. Set up once. Setup prints its plan and asks before it changes anything.

   ```bash
   playbook setup
   ```

2. Start a session in your repo. `pb` is the short name for `playbook`, which setup links for you.

   ```bash
   pb cc
   ```

3. Later, list the sessions of this directory.

   ```bash
   pb cc list
   ```

## What you see

`pb cc` opens Claude Code with the playbook system prompt. `pb cc list` prints `no sessions for repo` until you have run one.

## Next

- [The launcher](../guides/09-launcher.md): fresh sessions and worktrees.
- [What setup installs](../guides/05-what-setup-installs.md): every change setup makes.
