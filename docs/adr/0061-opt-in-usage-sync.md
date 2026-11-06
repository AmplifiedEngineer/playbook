# ADR-0061: Sync Usage Between One User's Computers Only When the User Turns It On

- **Status:** Proposed
- **Date created:** 2026-11-07
- **Date modified:** 2026-11-07
- **Relates to:** [ADR 0025](0025-self-improving-playbook.md) (it rejected remote learning across users; this ADR covers one user's own machines and does not change that), [ADR 0020](0020-usage-terminal-ui.md) (the usage UI that reads these rows)

## Context

One person often uses playbook on more than one computer, for example a home machine and a work machine. The machines run different projects and can use different Claude accounts. Limits belong to the account and usage belongs to the machine. Today every number stays on the machine that made it. No view of one account across both machines exists.

The owner chose libSQL as the shared store. Two facts make that choice costly.

- Five crates call `rusqlite` directly, in 49 files, and nothing sits between the code and the engine.
- `libsql` bundles its own SQLite. External reports say two SQLite libraries do not link into one binary, and the `libsql` API is async while the workspace has no async runtime. The evidence gate below rechecks both points.

The user guides say nothing leaves the machine (`docs/guides/05-what-setup-installs.md`, `docs/guides/10-telemetry.md`). Sync changes that claim. The claim needs a precise new wording and a record of the rules behind it.

## Decision

Take approach C: a storage crate first, then a `libsql` engine behind a feature, then sync code only after an evidence gate passes.

1. **Storage crate first.** Every crate imports `playbook-store` in place of `rusqlite`. The crate keeps rusqlite's names. Call sites change their `use` line and little else. A CI guard fails any `rusqlite` import outside the crate. This part ships with no behavior change and helps even if sync never ships.
2. **Engine behind a feature.** `engine-rusqlite` is the default. `engine-libsql` is a second feature, and building with both fails. The expected result is that only a separate `pbsync` helper uses `libsql`. The `playbook` binary adopts it only if the gate below passes.
3. **Evidence gate before sync code.** A CI job builds `pbsync` for the five release targets and measures binary size, push time for 10,000 rows, and open overhead on the statusline path against a rusqlite baseline in the same job. A failed bar stops the work and returns to this ADR. No sync code lands before the gate passes.
4. **Opt-in, off by default.** Nothing leaves your machine unless you turn on sync with `playbook sync enable`. A fresh install, an upgrade and a doctor run do not contact a remote.
5. **What leaves the machine.** Only usage rows (`usage_events`) and a snapshot of shareable config keys. Sync sends no `repo`, `branch`, `cwd`, file path or raw email. The account goes as a salted hash.
6. **Allowlists.** A per-account list (`sync.accounts`, empty by default) decides which accounts sync. A work account stays local until the owner adds it. An explicit column list picks the row columns, and an explicit key list (`SHARED_KEYS`) picks the config keys. A new column or key stays local until someone adds it to the list. Paths and machine-bound keys stay off the list.
7. **Transcripts stay machine-local.** Sync does not read or upload a transcript. Memory facts, the code index, the gate databases and `watch.db` are out of scope for the same reason.
8. **Remote keys.** The remote primary key is `(machine, local key)`. Rows from two machines cannot collide. A push repeated with the same rows writes nothing new.
9. **A pulled config is a proposal.** A pull writes a proposal and does not apply it. `playbook sync config apply` shows the diff first.
10. **Schema safety.** An older binary refuses a newer schema with a message that names both versions. This stops two machines on different versions from corrupting a shared store.

## Privacy and the opt-in carve-out

The promise to users becomes: nothing leaves your machine unless you turn on sync. Before this ADR the promise had no condition. After it, the condition is one explicit command, and the guides name it once sync exists. The user guides stay as they are until then, because they describe what the code does today.

ADR 0025 rejected "remote or shared learning across users". Its reason was that one user's habits are not another's and that learning must stay local. This ADR does not share anything across users. It moves one user's own usage between that user's own computers, to a store that user controls. The two decisions do not conflict. ADR 0025 carries a short pointer to this record.

### Threat model of the salted account hash

The hash hides the email from someone who opens the remote and reads it casually. It does not hide the email from a determined guess.

- The salt sits in the same remote as the data (`shared_meta`), because every new machine must fetch it to compute the same hash. Anyone who can read the rows can read the salt.
- With the salt in hand, a person can hash a list of known emails and compare. Emails are short and guessable. This works against a known candidate.
- The hash is therefore a pseudonym, and treat the remote as data that identifies the account to anyone who can read it. Protect it with the provider's access controls, not with the hash.

### Revoking access

1. Rotate or delete the access token at the provider. This is what cuts off a machine, because the provider decides what a token can reach.
2. Run `playbook sync disable` on each machine. It stops all remote traffic from that machine.

Data already in the remote stays there until the owner deletes it at the provider. A `playbook sync purge` command removes this machine's rows.

## Considered alternatives

**A. Port every call site to `libsql` directly.** Rejected. Every call site changes once, then again if the engine changes. The async API and the missing runtime hit hooks and the statusline, which must stay local and synchronous.

**B. A separate `pbsync` helper that shares no code with the shipped crates.** Kept as the fallback. It duplicates SQL and the table code.

**D. Files only, with JSONL export and import.** Rejected because the owner chose libSQL. The row format keeps this option open.

**Send the raw email, or the repo and branch, for a richer view.** Rejected. Those fields identify people and work, and the account view does not need them.

**Sync by default with an opt-out.** Rejected. It breaks the local-only promise for every existing user on upgrade.

## Consequences

- The storage crate and the schema check help even if sync is cancelled, and so does the machine scope in pbtop.
- A failed evidence bar stops the plan at the gate. That is the intended cost control.
- The remote holds pseudonymous account data, with the limits described above.
- Two builds exist, `playbook` and `pbsync`, compiled in separate cargo runs so both engines do not link into one binary. The release carries one more file.
- Doctor and the guides need a sync section once sync exists.

## Open questions for the owner

This ADR leaves both open.

1. **Hosting.** Turso Cloud or a self-hosted `sqld`. This sets who can read the remote, what it costs, and how you issue and rotate a token.
2. **The libsql and rusqlite link outcome.** Whether the `playbook` binary can adopt the `libsql` engine, or only `pbsync` does. The gate measures it and records the answer.

The Status stays Proposed until the owner accepts this record.
