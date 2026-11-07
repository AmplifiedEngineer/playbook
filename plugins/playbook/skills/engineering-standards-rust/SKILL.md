---
name: engineering-standards-rust
description: Rust rules for code and reviews, covering ownership, errors, types, API design, async, unsafe, testing and Clippy lints. Use when writing or reviewing Rust, or a diff that touches .rs files.
user-invocable: false
---

# Engineering Standards: Rust

Language-specific engineering standards for Rust, derived from the team's base standards. RFC 2119 keywords (MUST, SHOULD) carry their standard meanings.

**REQUIRED BACKGROUND:** engineering-standards owns the general rules (PR readiness, size, test types and requirements, the mocking philosophy, deployment). Read it first. This skill adds only the Rust specifics and does not restate the base.

## How to use this skill

- The index below holds one line per rule. Each topic names a reference file in this skill's `references/` folder and says when to load it. Load only the files the work touches.
- When you review a diff that touches `.rs` files, load the reference for each topic the diff touches. Cite the rule and the lint or command that checks it.
- The project's own setup wins. When `Cargo.toml`, `clippy.toml`, CI or the contributing guide sets a rule, follow it over this skill.
- A Clippy lint in the `pedantic`, `nursery` or `restriction` group is off by default. The references mark those lints. Turn one on in the `[lints.clippy]` table of `Cargo.toml` (or `[workspace.lints.clippy]`) before you rely on it.

## Rules index

### Ownership and borrowing

- Borrow to read, own to keep. Take `&str`, `&[T]` and `&Path` in parameters.
- A clone MUST have a reason a reviewer can see. Shorten the borrow or use `mem::take` before you clone.
- Prefer one owner. Use an arena with indices for trees and graphs, and `Rc<RefCell<T>>` only when sharing and mutation are both real.

Load `references/ownership-and-borrowing.md` when you fix a borrow checker error, add a `.clone()`, `Rc`, `RefCell` or a lifetime parameter, or choose a parameter type.

### Error handling

- A library MUST return typed errors (`thiserror` or a hand-written enum). An application MAY use `anyhow` where callers only report the error.
- Library and production code MUST NOT `unwrap` or `expect` on a path that can fail at run time. Panic only for a broken invariant, and say which one in the `expect` message.
- Add context at each fallible step. Expose the cause through `source()` and keep it out of the `Display` text.

Load `references/error-handling.md` when you define an error type, write `?`, `unwrap`, `expect` or `panic!`, or add a dependency whose errors cross your API.

### Types and traits

- Wrap meaningful primitives in newtypes and validate once in the constructor.
- Use an enum in place of a `bool` parameter or a string that holds one of a fixed set of values.
- Implement `From` and `TryFrom`, avoid lossy `as` casts, and derive the common traits on public types.
- Write a concrete type first. Make it generic when a second type needs it.

Load `references/types-and-traits.md` when you add a type or a trait, add a conversion between types, or parse untrusted input at a boundary.

### API design

- Public items follow the Rust API Guidelines: `as_`, `to_` and `into_` prefixes, getters with no `get_`, private fields, `#[non_exhaustive]` on types that will grow.
- Public items have doc comments with examples that use `?`, and `# Errors`, `# Panics` and `# Safety` sections where they apply.

Load `references/api-design.md` when you add or change a `pub` item, a crate feature or a doc comment, or prepare a library release.

### Style and naming

- Copy the project's naming scheme. No glob imports from other crates. Use `let else` and `if let` over a check then `unwrap`.

Load `references/style-and-naming.md` when you name things, write imports or `match` blocks, or review a diff for readability.

### Async and concurrency

- Do not block an async runtime thread. Move blocking work to `spawn_blocking`, `rayon` or a dedicated thread.
- Do not hold a `std` lock guard or a `RefCell` borrow across `.await`. Use bounded channels, track every spawned task, and put a timeout on each remote call.

Load `references/async-and-concurrency.md` when you write `async` code, spawn a task or thread, or add a lock, channel or atomic.

### Iterators and collections

- Prefer iterator adapters to index loops, and do not `collect` only to loop again.
- Use the entry API for insert-or-update, and `Vec` and `HashMap` unless the access pattern asks for another type.

Load `references/iterators-and-collections.md` when you write a loop, an iterator chain or a `collect`, or pick a collection type.

### Performance

- Profile a release build before you optimize, and benchmark with `criterion` or `divan`.
- On a hot path, size collections up front, reuse buffers and avoid clones.

Load `references/performance.md` when a change targets speed or memory, touches a hot path, or edits a build profile.

### Unsafe and FFI

- Write `unsafe` only when safe Rust cannot express the code, behind a safe API. Each `unsafe` block has a `// SAFETY:` comment and each `pub unsafe fn` a `# Safety` section.
- Run tests under Miri for crates with unsafe code. Do not let a panic cross an `extern "C"` function.

Load `references/unsafe-and-ffi.md` when a diff adds or changes `unsafe`, `extern`, raw pointers or `#[repr(C)]`.

### Security and input

- Parse untrusted input into typed values at the boundary, cap its size, and do not panic on it.
- Pass process arguments one by one, bind SQL parameters, and keep secrets out of `Debug` and logs.

Load `references/security-and-input.md` when code reads input from a network, a file or a user, runs a process, builds SQL, or handles a secret.

### Testing

- Unit tests sit in a `#[cfg(test)]` module, integration tests in `tests/`, and public items have doc tests.
- Tests share no state and do not sleep. Tests may `unwrap`.

Load `references/testing.md` when you write or review a test, or reproduce a bug as a failing test.

### Tooling and CI

- CI runs `cargo fmt --check` and Clippy on all targets with warnings denied. Lint levels live in `[workspace.lints]`.
- Check dependencies with `cargo deny` or `cargo audit`, keep features additive, and test the MSRV.

Load `references/tooling-and-ci.md` when you change `Cargo.toml` lints, CI, the toolchain, features or dependencies. It ends with the setup of amplifiedengineer/playbook, this plugin's source, as a worked example.

### Project structure

- One workspace for several crates, with shared versions in `[workspace.dependencies]`. Keep `main.rs` thin and items private by default.

Load `references/project-structure.md` when you add a crate or module, change visibility, or move code between crates.

### Antipatterns

Load `references/antipatterns.md` when you review a Rust diff. It is a checklist of the most common smells, each with a fix, and a check where one exists.

## Sources

The source list started from [mre/idiomatic-rust](https://github.com/mre/idiomatic-rust), curated by Matthias Endler and contributors and dedicated to the public domain under CC0 1.0. The rules here are a summary in our own words. No text is copied from the sources below.

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) (MIT or Apache-2.0)
- [The Rust Programming Language](https://doc.rust-lang.org/book/) (MIT or Apache-2.0)
- [Rust Design Patterns](https://rust-unofficial.github.io/patterns/) (MPL-2.0)
- [Canonical's Rust best practices](https://canonical.github.io/rust-best-practices/) (GPL-3.0, ideas restated). Where it differs from the other sources, the reference says so and names the rule this skill picks.
- [Clippy lint list](https://rust-lang.github.io/rust-clippy/stable/index.html). Lint groups were checked against Clippy 1.97.
- [thiserror](https://github.com/dtolnay/thiserror) and [anyhow](https://github.com/dtolnay/anyhow) by David Tolnay
- [Error Handling in Rust](https://burntsushi.net/rust-error-handling/) by Andrew Gallant
- [To panic or not to panic](https://www.ncameron.org/blog/to-panic-or-not-to-panic/) by Nick Cameron
- [Context-preserving error handling](https://kazlauskas.me/entries/errors) by Simonas Kazlauskas
- [Designing Error Types in Rust Libraries](https://d34dl0ck.me/rust-bites-designing-error-types-in-rust-libraries/index.html) by Sven Kanoldt
- [Elegant Library APIs in Rust](https://deterministic.space/elegant-apis-in-rust.html) by Pascal Hertleif
- [When should I use String vs &str?](https://steveklabnik.com/writing/when-should-i-use-string-vs-str/) by Steve Klabnik
- corrode.dev articles by Matthias Endler: [Be Simple](https://corrode.dev/blog/simple/), [Defensive Programming](https://corrode.dev/blog/defensive-programming/), [Pitfalls of Safe Rust](https://corrode.dev/blog/pitfalls-of-safe-rust/)
- [Tokio tutorial](https://tokio.rs/tokio/tutorial) and API docs (MIT)
- [Async: What is blocking?](https://ryhl.io/blog/async-what-is-blocking/) and [Actors with Tokio](https://ryhl.io/blog/actors-with-tokio/) by Alice Ryhl
- [Cancelling async Rust](https://sunshowers.io/posts/cancelling-async-rust/) by Rain
- [Rust Atomics and Locks](https://marabos.nl/atomics/) by Mara Bos
- [The Rust Performance Book](https://nnethercote.github.io/perf-book/) by Nicholas Nethercote and contributors (MIT or Apache-2.0)
- [The Rustonomicon](https://doc.rust-lang.org/nomicon/), the [Rust Reference](https://doc.rust-lang.org/reference/) and the [Miri README](https://github.com/rust-lang/miri)
- [The Cargo Book](https://doc.rust-lang.org/cargo/), the [rustdoc book](https://doc.rust-lang.org/rustdoc/) and the [Clippy book](https://doc.rust-lang.org/clippy/)
- Tool docs: [cargo-nextest](https://nexte.st/), [cargo-deny](https://embarkstudios.github.io/cargo-deny/), [insta](https://insta.rs/), [proptest](https://proptest-rs.github.io/proptest/) and [Command Line Applications in Rust](https://rust-cli.github.io/book/)
- [Effective Rust](https://effective-rust.com/) by David Drysdale (CC BY-NC-ND 4.0, ideas only)
- [projectrules.ai Rust rules](https://www.projectrules.ai/rules/rust), a secondary source. A few security ideas were kept and rewritten.
