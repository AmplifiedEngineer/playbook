# Repository layout

This repository holds the `playbook` binary and `install.sh`. The skills, commands and agents live in `plugins/playbook`.

The Rust code is a Cargo workspace under `crates/`:

- `cli`: the `playbook` binary.
- `common`
- `code`
- `telemetry`: the `playbook-telemetry` package, which builds `pbtop`.
- `data`
- `plugin-checks`: tests for the plugin.
- `store`: the `playbook-store` package, a shim over the SQLite engine.

See [CONTRIBUTING.md](../../CONTRIBUTING.md#repository-layout). A `vX.Y.Z` release ships the binary and the plugin with the same version ([ADR-0038](../adr/0038-one-release-line.md)).
