# How to sync the Playbook plugin

<!-- verify: names -->

Install the plugin for a binary that did not come from Homebrew, or bring the plugin up to date without touching the binary.

**You need:** the `playbook` binary, `claude` on your `PATH` and a network connection.

1. Install the plugin and its marketplace. A second run changes nothing.

   ```bash
   playbook plugin install
   ```

2. Later, update only the plugin.

   ```bash
   playbook plugin upgrade
   ```

3. Restart Claude Code, or run `/reload-plugins` in a session.

## What you see

Each command prints one line for each step or plugin. `install` turns on `autoUpdate` for the marketplace in `~/.claude/settings.json` and keeps an explicit `false`. Either command exits 1 when `claude` is missing or a step fails.

## Next

- [Upgrade Playbook](upgrade-playbook.md): the binary and the plugin together.
- [Install guide](../guides/00-install.md): the plugin on its own.
