# How to upgrade Playbook

<!-- verify: names -->

Check for a newer release, then upgrade the binary and the plugin together.

**You need:** the `playbook` binary and a network connection. On Homebrew, the last step says to run `brew upgrade playbook` instead.

1. Look for a newer release. This changes nothing.

   ```bash
   playbook upgrade --check
   ```

2. Upgrade. In a terminal it shows both versions and asks you to confirm.

   ```bash
   playbook upgrade
   ```

3. Start Claude Code again, so the session loads the new plugin.

## What you see

The command checks the release checksum and, when `gh` is installed, its build attestation. It keeps the old binary as a backup, then updates the plugin to match. On Homebrew the command leaves the binary alone, says Homebrew owns it and exits 0.

## Next

- [Upgrade](../guides/00-install.md#upgrade): every route and rollback.
- [Sync the plugin](sync-the-plugin.md): update only the plugin.
