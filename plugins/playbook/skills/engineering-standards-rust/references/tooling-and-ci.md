# Tooling and CI

Formatter, lints, dependency checks and the CI steps that run them. A lint marked (pedantic), (nursery), (restriction) or (cargo) is off by default.

## Contents

- Format and lint: rules 1 to 5
- Toolchain and dependencies: rules 6 to 10
- Features and releases: rules 11 to 13
- Worked example: amplifiedengineer/playbook

## Format and lint

1. **CI runs `cargo fmt --all --check`.**
   Why: formatting then stays out of review.

2. **CI runs Clippy on all targets with warnings denied.** `cargo clippy --workspace --all-targets --all-features`, with `-- -D warnings`, or with `CARGO_BUILD_WARNINGS=deny` on Cargo 1.97 and later. Do not put `#![deny(warnings)]` in source.
   Why: `--all-targets` covers tests and examples, and `--all-features` covers feature-gated code. A deny in source breaks the build on the next toolchain update.

3. **Set lint levels in `Cargo.toml`.** Use `[workspace.lints.clippy]` and `[workspace.lints.rust]` at the root and `[lints] workspace = true` in each member. Keep `clippy.toml` for lint settings such as `msrv` and `allow-unwrap-in-tests`.

   ```toml
   [workspace.lints.clippy]
   pedantic = { level = "warn", priority = -1 }
   missing_errors_doc = "allow"
   unwrap_used = "warn"
   ```

   Why: one reviewed place for every lint. A group needs `priority = -1` so the single-lint lines win over it.

4. **Turn on `pedantic` as a group if you like, and pick `restriction` lints one at a time.** Useful picks: `unwrap_used`, `expect_used`, `dbg_macro`, `todo`, `print_stdout` for libraries, and `allow_attributes_without_reason`.
   Why: restriction lints contradict each other as a group.

5. **Each `#[allow]` carries a reason.** Prefer `#[expect(lint, reason = "...")]`, which warns once the lint no longer fires.
   Check: `clippy::allow_attributes_without_reason` (restriction).

## Toolchain and dependencies

6. **Pin the toolchain for applications and CI** in `rust-toolchain.toml`. Bump it in its own pull request.
   Why: new lints then arrive in their own change.

7. **Libraries declare `rust-version` (the MSRV) and test it in CI** with `cargo +<msrv> check`, `cargo hack --rust-version` or `cargo msrv verify`.
   Why: an MSRV that CI does not test is a guess.

8. **Check dependencies with `cargo deny check`** (licenses, advisories, bans and sources) or `cargo audit` for advisories only. Commit `deny.toml`.

9. **Commit `Cargo.lock`, and build in CI with `--locked`.** Cargo has recommended this for libraries too since 2023. Pair it with a scheduled job or a bot that tests the latest versions.
   Why: CI and `git bisect` see the same versions you tested.

10. **Use caret version requirements** (`"1.4"`). No `"*"`, and avoid exact `=` pins in libraries.
    Why: exact pins block security fixes and cause version conflicts for users.
    Check: `clippy::wildcard_dependencies` (cargo).

## Features and releases

11. **Features are additive.** Turning on a feature adds code and removes nothing. Name it after what it adds (`std`, not `no_std`). Use `dep:` for optional dependencies.
    Why: Cargo merges features across the dependency graph, and one crate's choice applies to all.
    Check: `clippy::negative_feature_names` (cargo).

12. **Test feature combinations.** At least default, `--no-default-features` and `--all-features`. Use `cargo hack --each-feature` for libraries.

13. **Libraries run `cargo semver-checks` before a release, and build docs with warnings denied:** `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`.

## Worked example: amplifiedengineer/playbook

[amplifiedengineer/playbook](https://github.com/amplifiedengineer/playbook), the source of this plugin, applies most of these rules. Its CI does not pass `--locked`. Its `rust-toolchain.toml` pins one Rust version. `[workspace.lints.clippy]` turns on `uninlined_format_args`, `manual_let_else` and `undocumented_unsafe_blocks`. CI runs `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and a second Clippy pass on `--lib --bins` that denies `unwrap_used` and `expect_used`. Tests run under `cargo nextest` with retries set to 0, then `cargo test --doc`, and `cargo audit` checks advisories.
