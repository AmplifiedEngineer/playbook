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

Take approach C: a storage crate first, then a `libsql` engine behind a feature, then sync code only after an evidence gate passes. The owner has decided the points in the Decisions section below.

1. **Storage crate first.** Every crate imports `playbook-store` in place of `rusqlite`. The crate keeps rusqlite's names. Call sites change their `use` line and little else. A CI guard fails any `rusqlite` import outside the crate. This part ships with no behavior change and helps even if sync never ships.
2. **Engine behind a feature.** `engine-rusqlite` is the default. `engine-libsql` is a second feature, and building with both fails. `playbook-store` is the one place that depends on the engine, and the target is that it switches from rusqlite to `libsql` for the whole `playbook` binary.
3. **Evidence gate before sync code.** A CI job builds the `playbook` binary with `engine-libsql` for the five release targets, including musl and Windows. It measures binary size, push time for 10,000 rows, and open overhead on the statusline path against a rusqlite baseline in the same job. If a build fails or the size jump is unacceptable, the work parks and reports back to this ADR. It does not force the port, and fallback B below applies. No sync code lands before the gate passes.
4. **Opt-in, off by default.** Nothing leaves your machine unless you turn on sync with `playbook sync enable`. A fresh install, an upgrade and a doctor run do not contact a remote.
5. **What leaves the machine.** Only usage rows (`usage_events`) and a snapshot of shareable config keys. The repo goes as a salted hash of the repo name (128 bits, a truncated SHA-256 written as hex) with a per-user salt kept in the remote `shared_meta`, like the account salt. The account goes as a salted hash. Sync sends no `branch`, `cwd`, file path or raw email, and it does not send `session_id` or `agent`.
6. **Allowlists.** A per-account list (`sync.accounts`, empty by default) decides which accounts sync. The work account syncs too, once the owner adds it to the list. An explicit column list picks the row columns, and an explicit key list (`SHARED_KEYS`) picks the config keys. A new column or key stays local until someone adds it to the list. Paths and machine-bound keys stay off the list.
7. **Transcripts stay machine-local.** Sync does not read or upload a transcript. Memory facts, the code index, the gate databases and `watch.db` are out of scope for the same reason.
8. **Remote keys.** The remote primary key is `(machine, local key)`. Rows from two machines cannot collide. A push repeated with the same rows writes nothing new.
9. **A pulled config is a proposal.** A pull writes a proposal and does not apply it. `playbook sync config apply` shows the diff first.
10. **Schema safety.** An older binary refuses a newer schema with a message that names both versions. This stops two machines on different versions from corrupting a shared store.

## Privacy and the opt-in carve-out

The promise to users becomes: nothing leaves your machine unless you turn on sync. Before this ADR the promise had no condition. After it, the condition is one explicit command, and the guides name it once sync exists. The user guides stay as they are until then, because they describe what the code does today.

ADR 0025 rejected "remote or shared learning across users". Its reason was that one user's habits are not another's and that learning must stay local. This ADR does not share anything across users. It moves one user's own usage between that user's own computers, to a store that user controls. The two decisions do not conflict. ADR 0025 carries a short pointer to this record.

### Threat model of the salted hashes

The account hash and the repo-name hash hide the email and the repo name from someone who opens the remote and reads it casually. They do not hide either from a determined guess.

- The salts sit in the same remote as the data (`shared_meta`), because every new machine must fetch them to compute the same hashes. Anyone who can read the rows can read the salts.
- With a salt in hand, a person can hash a list of known emails or known repo names and compare. Both are short and guessable. This works against a known candidate, and the repo-name hash has the same limit as the account hash.
- Each hash is therefore a pseudonym. Treat the remote as data that identifies the account to anyone who can read it, and protect it with the provider's access controls, not with the hash.

### Revoking access

1. Rotate or delete the access token at the provider. This is what cuts off a machine, because the provider decides what a token can reach.
2. Run `playbook sync disable` on each machine. It stops all remote traffic from that machine.

Data already in the remote stays there until the owner deletes it at the provider. A `playbook sync purge` command removes this machine's rows.

## Considered alternatives

**A. Port every call site to `libsql` directly.** Rejected. Every call site changes once, then again if the engine changes. The storage crate gives the same port in one place.

**B. A separate `pbsync` helper that shares no code with the shipped crates.** The fallback if the evidence gate fails. It duplicates SQL and the table code, and it adds a second build.

**D. Files only, with JSONL export and import.** Rejected because the owner chose libSQL. The row format keeps this option open.

**Send the raw email, or the repo and branch, for a richer view.** Rejected. Those fields identify people and work, and the account view does not need them.

**Sync by default with an opt-out.** Rejected. It breaks the local-only promise for every existing user on upgrade.

## Consequences

- The storage crate and the schema check help even if the owner cancels sync, and so does the machine scope in pbtop.
- A failed evidence bar stops the plan at the gate. That is the intended cost control.
- The remote holds pseudonymous account data, with the limits described above.
- If the gate passes, one build and one engine ship. If it fails, the fallback adds a second build, `pbsync`, compiled in a separate cargo run so both engines do not link into one binary, and the release carries one more file.
- Sync runs off the hook and statusline paths. Local reads and hooks do not wait on the network.
- Doctor and the guides need a sync section once sync exists.

## Decisions

The owner answered the earlier open questions.

1. **Hosting: Turso Cloud.** The URL stays a setting. The token comes from an environment variable whose name sits in config. Playbook stores no token.
2. **Link: port everything to `libsql`.** There is no separate `pbsync` helper unless the gate fails. `playbook-store` switches from rusqlite to `libsql`. The evidence gate measures binary size and the musl and Windows builds first. If a build fails or the size jump is unacceptable, the work parks and reports instead of forcing it. Local reads and hooks do not wait on the network, because sync runs off the hook and statusline paths.
3. **Privacy: sync a salted repo-name hash.** The hash is 128 bits of hex from a truncated SHA-256, with a per-user salt in the remote `shared_meta`. Sync still sends no branch or working directory, no raw email, and no `session_id` or `agent`.
4. **Work account: it syncs too.** The explicit per-account allowlist `sync.accounts` stays in config and starts empty. The owner adds accounts. Enabling sync stays a visible opt-in with a consent prompt, because company policy still applies.

The Status stays Proposed until the owner accepts this record.
