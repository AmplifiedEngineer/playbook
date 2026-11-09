# How to check your Playbook setup

<!-- verify: names -->

See which parts of Playbook work on this machine and what to run to fix the rest.

**You need:** the `playbook` binary.

1. Print the checks from a shell.

   ```bash
   playbook doctor check
   ```

2. Inside a Claude Code session, run `/playbook:doctor` for the seven layers in one table.

## What you see

Each line starts with `PASS`, `INFO`, `WARN` or `FAIL`. A `WARN` or `FAIL` line usually names the command that fixes it, such as `playbook setup --yes`. Run that command, then check again.

## Next

- [Layers](../reference/layers.md): the seven layers the doctor reports.
- [What setup installs](../guides/05-what-setup-installs.md): what each fix changes.
