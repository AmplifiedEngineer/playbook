# How to watch your Claude Code costs

<!-- verify: run -->

See tokens and cost for your sessions, in a terminal dashboard or as one text summary.

**You need:** the `playbook` binary. `pbtop` is the same dashboard under its own name.

1. Open the dashboard.

   <!-- verify: names -->
   ```bash
   pbtop
   ```

2. Print a summary for the month and exit. It works in scripts too.

   ```bash
   pbtop --summary --range month
   # expect: No usage in this month (*).
   ```

3. Check that the collector and the telemetry setting are healthy.

   <!-- verify: names -->
   ```bash
   playbook usage status
   ```

## What you see

The page runs step 2 on a fresh machine, so it prints `No usage in this month`. With sessions recorded, it prints the message count and the estimated cost, then one table per grouping, such as by day or by model. The dashboard has a Charts, a Numbers and a Sessions tab. Press `?` for its keys.

## Next

- [pbtop](../guides/11-pbtop.md): the views and the keys.
- [Telemetry](../guides/10-telemetry.md): what it records and how to turn it off.
