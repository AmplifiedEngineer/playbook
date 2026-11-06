# The SQLite store crate

`crates/store` (package `playbook-store`) is the one place playbook will reach SQLite. It keeps the names of the engine behind it, so a crate moves over by changing its `use` line. Today the engine is `rusqlite =0.40.2` (bundled), behind the default cargo feature `engine-rusqlite`.

The crate holds the two bounded busy-retry helpers, kept as they were in their crates until a later change merges them:

| Helper | Retries | Copied from |
| --- | --- | --- |
| `retry_busy_or_locked` | `DatabaseBusy` and `DatabaseLocked` | `crates/common/src/sqlite.rs` |
| `retry_busy_only` | `DatabaseBusy` | `crates/telemetry/src/db.rs` |

`scripts/check-port-diff.sh` checks a porting change: only imports and type paths may differ, and a test file may change `use` lines only. `scripts/check-port-diff.test.sh` runs it on temp repositories.

`scripts/check-no-rusqlite.sh` fails CI when `rusqlite` appears in a Rust file or `Cargo.toml` outside `crates/store`. `scripts/check-no-rusqlite.test.sh` runs it on temp trees. Clippy's `disallowed_types` cannot do this job: it resolves a re-export to the engine path, so it flags `playbook_store::Connection` too.

## Call sites of rusqlite

Counted with `grep -r rusqlite` over each crate on the commit that added the store. A line counts once, however many times the word appears on it.

| Crate | Files in `src/` | Files in `tests/` | Lines |
| --- | --- | --- | --- |
| `cli` | 7 | 7 | 65 |
| `code` | 10 | 0 | 66 |
| `common` | 1 | 0 | 9 |
| `data` | 4 | 0 | 36 |
| `telemetry` | 26 | 1 | 66 |
| Total | 48 | 8 | 242 |

## rusqlite items in use and their libsql equivalents

libsql is async and has no direct counterpart for several items. "Unverified" marks an entry taken from memory of the libsql crate, not checked against its docs or a build.

| Item (uses) | libsql equivalent | Status |
| --- | --- | --- |
| `Connection::open`, `open_with_flags`, `OpenFlags` (29) | `Builder::new_local(path).build().await` then `connect()`; flags only partly | unverified |
| `Connection::execute` (162), `execute_batch` (111) | `Connection::execute`, `execute_batch` (async) | unverified |
| `Connection::prepare` (83), `query_map` (73), `query` (27) | `prepare` then `query`, rows read with `Rows::next().await`; no `query_map` | unverified |
| `query_row` (164) | `query` plus `Rows::next`; no direct helper | unverified |
| `OptionalExtension::optional` (28) | none | unverified |
| `params!` (25), `params_from_iter` (3), `named_params` (1) | `params!`, `params_from_iter`, `named_params!` | unverified |
| `&dyn ToSql` slices (6) | none, parameters are `IntoParams` values | unverified |
| `transaction` (35), `TransactionBehavior` (7) | `Connection::transaction`, `transaction_with_behavior` | unverified |
| `unchecked_transaction` (5), `Transaction::new_unchecked` (1) | none, a libsql transaction borrows no connection | unverified |
| `pragma_update` (22) | none, run `PRAGMA` through `execute_batch` | unverified |
| `busy_timeout` (23) | `Connection::busy_timeout` | unverified |
| `last_insert_rowid` (8) | `Connection::last_insert_rowid` | unverified |
| `is_autocommit` (2) | `Connection::is_autocommit` | unverified |
| `Row::get`, `get_ref` (1), `types::ValueRef` (1) | `Row::get`, `get_value` returning `Value` | unverified |
| `Error::SqliteFailure`, `ErrorCode`, `ffi` codes (about 30) | none, libsql wraps the code in its own error type | unverified |
| FTS5 `MATCH` (code index, memory search) | the bundled SQLite supports it, no API involved | unverified |

## Golden file

`crates/data/tests/golden/memory-open.txt` holds the `journal_mode` and the SQL of every schema object that the `memory.db` write path leaves on disk. `crates/data/tests/open_golden.rs` compares a fresh capture with it, so a change of engine cannot alter the schema or the pragma silently. Set `UPDATE_GOLDEN=1` to rewrite it after an intended change.
