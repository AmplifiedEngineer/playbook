# ADR-0059: Tombstone Format for On Disk State

- **Status:** Accepted
- **Date created:** 2026-11-05
- **Date modified:** 2026-11-05

## Context

Playbook keeps state on disk. It holds session folders, gate databases, logs, loaded handoffs, and worktrees with the build output inside them. Nothing removed most of it, and the pieces that did remove something deleted it for good. A session folder that looked idle could belong to a session that was only paused, and a deleted handoff could not be read again.

`playbook gc` and a daily sweep now keep this state from growing without bound. The design in `plans/tombstone-process-for-on-disk-state.md` needs one stored format for what gc moves aside, and a clear rule for what it deletes outright. Other code reads and writes the format, and a later change to it would strand tombstones that users already hold, so it is a record worth keeping.

## Decision

1. **A tombstone is a move, not a delete.** An item that a store rule marks as due moves to `gc/tombstones/<date>/<id>/`. A line in `gc/index.jsonl` records where it came from. After the item's grace period a later run deletes the copy. Until then `playbook gc --restore <id>` puts it back.
2. **The index is append only.** One line per state change, and the last line for an id wins. A record holds the id, the original path or paths, the destination, the size, the reason, `tombstoned_at`, `retain_days` (the grace for this item, taken from its store when the item moved) and the `mode`. The mode is `rename` when source and destination share a filesystem and `copy` otherwise. One id can cover a group of paths, such as a gate database with its `-wal` and `-shm` files.
3. **The states are fixed.** A record is `pending`, `copied`, `done`, `expiring`, `expired`, `restoring`, `restored`, `lost` or `dropped`. Compaction drops `expired`, `restored`, `lost` and `dropped` records once they are older than the grace period, and it keeps the others.
4. **The move is crash safe.** The `pending` record is written and flushed before anything moves. Then the data moves, then a `done` record follows. A run that finds a `pending` record older than the run lock's stale limit finishes the move or drops the record, so the index and the disk agree again. A record younger than that limit is left alone, so a stolen lock cannot make a second run drop a live holder's move. When a move fails or cannot start, the source stays where it was.
5. **Ids do not clash.** An id is the epoch milliseconds followed by 8 hex characters of the path hash. The destination is created without overwrite, and a clash adds a counter.
6. **Expiry is bounded.** Expiry reads `tombstoned_at` and the record's own `retain_days`. It skips a record dated in the future and deletes at most 500 tombstones in one run. A migrated handoff gets `tombstoned_at` equal to the migration time, so it receives a full grace period.
7. **One guard removes data.** Every removal goes through the gc guard, which re-checks the run lock's owner token before it acts and refuses a path that is outside a registered store root, is a link that leaves the root, or is the root itself. The sweep's removal of a worktree and of build output also goes through it, and a test scans the crate for any other call to a removal function.
8. **Some state is deleted, not tombstoned.** Build output is regenerable, so it is deleted directly. Worktrees keep their existing remove rules (landed, clean, stale, dead pid) and their own time limits, and get no marker record. A worktree's gate databases still go with the worktree when it is removed.
9. **Most numbers are configuration.** The grace periods, idle limits, log limits and free space limits are `gc.*` keys with typed defaults. A value outside its range falls back to the default.

## Known limits

These are accepted for the first version.

- **Log rotation.** Rotating a log copies it over `<log>.1` and then truncates the original, but only when its length is unchanged since the copy began. Lines written between the length check and the truncate are lost. A rotation also replaces an earlier `<log>.1`, so one rotated copy is kept on purpose and the older one is lost.
- **Dormant clones.** A second clone of a repo that stays idle for `gc.gateDbIdleDays` can lose its gate databases to a tombstone. The tombstone stays restorable for the grace period.
- **Lock touch during long calls.** The run lock goes stale when it is not touched. The sweep, the copy of a large item and the build output removal touch it after each item, and a copy touches it every 1000 entries. A single removal of one very large folder is one call, so the lock is not touched inside it.
- **Session start budget.** The first session start of a day runs a slice of the cleanup. It checks its 20 second budget between stores, not inside one. A store that waits on `gh` or removes a large folder can run past the budget, and the next start continues from the next store.
- **Virtual environments.** `.venv`, `venv` and any folder with a `pyvenv.cfg` are outside the build output table in the first version. A plain build does not recreate them, so gc leaves them alone.
- **Windows junctions.** A junction that points outside a root is not tested, because no runner runs the suite on Windows.

## Alternatives considered

- **Delete after the idle period.** Rejected. A wrong guess about idleness would lose data with no way back.
- **Move to the system trash.** Rejected. It is not available on every platform playbook supports, it has no index that playbook can read, and it gives no per item grace period.
- **A marker record for worktrees.** Rejected. The existing remove rules already hold the grace period, so a second record would repeat them.
- **Tombstone build output.** Rejected. A rebuild restores it, and moving a large folder aside would cost the disk space gc is meant to free.

## Consequences

- A removal that turns out to be wrong can be undone for the length of the grace period.
- Disk space comes back only after the grace period for tombstoned items, and at once for build output.
- Code that moves or removes state under a store root goes through the guard, so a new store registers a root and an action before it can act.
- The index and the date folders are the record. A date folder entry with no index record is reported as needing attention and is not deleted.
