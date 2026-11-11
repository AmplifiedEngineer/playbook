# How to fix the account label of past usage

<!-- verify: names -->

Move past usage to the account Claude billed, when the store labeled it with the signed-in email. It happens when `CLAUDE_CODE_OAUTH_TOKEN` holds a token for another account.

**You need:** the personal token in one shell.

1. Upgrade as in [upgrade-playbook](upgrade-playbook.md), then check `playbook --version`. Remove any 0.40.2 binary from your `PATH`, because it splits the newer log lines wrong.

2. Back up the store, then add `--db <copy>` to each `account` command to rehearse.

   ```bash
   cp ~/.config/playbook/data/usage.db ~/usage-2026-11-11.db
   ```

3. In the shell that holds the token, get its key and name it.

   ```bash
   playbook usage account fingerprint
   playbook usage account name token:ab12cd34 personal
   ```

4. Find the boundary. List the sessions with their keys. Use the day you set the token, or else the day totals from the Claude page to rank boundaries.

   ```bash
   playbook usage account sessions --from 2026-10-30 --to 2026-11-04
   playbook usage account reconcile --from 2026-10-30 --to 2026-11-04 --dashboard 2026-11-03=285.03
   ```

5. Preview the move. It prints counts and cost and changes nothing. Add `--confirm` to apply it, or `--session <ID>` for one session.

   ```bash
   playbook usage account assign --from 2026-11-01T00:00 --to 2026-11-04T00:00 --as personal --was you@work.example
   ```

6. Check with `playbook usage --summary --account personal` and again with the work email. Reverse a wrong move with `playbook usage account undo`.

7. Start one session with the token set. Confirm `playbook usage account sessions` shows its `token:` key.

## What you see

- Dollars are list-price estimates.
- A reset keeps applied moves.
- Only `CLAUDE_CODE_OAUTH_TOKEN` counts. An API key session shows the signed-in email.
- `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1` hides the token from the hook, which then records the email.

## Next

- [The usage dashboard](../internals/04-usage-dashboard.md): how each event gets its account.
