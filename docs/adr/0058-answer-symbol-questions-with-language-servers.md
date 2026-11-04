# ADR-0058: Answer Symbol Questions With Language Servers, One Per Call, Behind a Trust Gate

- **Status:** Accepted
- **Date created:** 2026-10-21
- **Date modified:** 2026-11-04

## Context

`playbook code` answers from Tree-sitter and a local index ([ADR 0026](0026-local-code-index-in-the-binary.md)). It cannot tell which declaration a call site means, show a type, or report compile errors. Language servers can, and the maintainer approved adding them to cut the input tokens an agent spends on Grep and whole file reads. The answers must be deterministic, offline at query time, and safe.

ADR 0026 already decided that managed server downloads happen only on request, with hashes in the source, and that servers need a workspace trust gate. This record settles the parts it left open: how servers run, how long they live, what happens when they fail, where installs go, and which commands and config keys the feature adds. The delivery plan is `playbook-code-lsp`.

## Decision

1. **One server process per CLI call.** `definition`, `hover`, position `refs` and `diagnostics` start the server, run one request under a timeout (default 20 seconds), send `shutdown` and `exit`, and kill and reap it after a 2 second grace. No daemon, socket or idle timer. The server leads its own process group so teardown kills its children too (`rust-analyzer` starts `cargo check` and build scripts); a signal handler and a drop guard run that teardown. On Linux it also gets `PR_SET_PDEATHSIG`. A per-workspace lock lets only one server run per workspace: a concurrent call falls back with `busy`. On macOS, if the CLI is SIGKILLed, servers and children exit only on stdin EOF. That limit is documented.
2. **Fail closed on workspace trust.** Servers run workspace code (build scripts, proc macros, the project interpreter, tsconfig plugins). No server starts until the user sets `code.lsp.trustWorkspace` to true for the repository, at the repo or global tier. The org tier refuses it. Agents must not set it without an explicit user instruction. The config store lives outside the checkout, so a project file cannot name a server command or trust itself.
3. **Always answer.** When a server is untrusted, missing, lacks the capability, is slow or crashes, the command returns the Tree-sitter answer labeled with a reason (`untrusted`, `busy`, `server_missing`, `capability_missing`, `timeout`, `server_crashed`). The exit code stays 0. `diagnostics` has no syntax equivalent and returns an empty list with a warning.
4. **Managed installs for three servers; `gopls` is external.** `playbook code lsp install` fetches `rust-analyzer` (GitHub `.gz`), `typescript-language-server` with a companion `typescript` package, and `pyright` (npm tarballs). Versions and integrity values (SHA-256 for release assets, npm SHA-512 for tarballs) are compiled into the binary and checked in memory before extraction. Installs go to `~/.config/playbook/lsp/<id>/<version>/` through a private staging directory and an atomic rename, with a receipt. Nothing needs sudo and no package manager runs. `gopls` has no release binaries; playbook prints the `go install` line and never runs it. Windows `rust-analyzer` (`.zip`) is external only. Pins follow a two week age rule and are bumped by a reviewed source change after two independent downloads agree.
5. **Own archive extraction.** The extractor reads `tar` entries and writes files itself. It rejects absolute paths, `..`, every symlink, hardlink, device and fifo entry, and enforces entry and size caps. It does not call `unpack_in`, because `tar` 0.4.44 had RUSTSEC-2026-0067 and 0068. New dependencies: `flate2 =1.1.10` and `tar =0.4.46` without default features.
6. **Surface added.** Subcommands `code definition`, `code hover`, `code diagnostics`, `code lsp list|status|install|remove`, and a position form of `code refs`. Config keys `code.lsp.<id>.path`, `code.lsp.trustWorkspace`, `code.python.interpreter`. No hook, migration, doctor row or setup part.
7. **The crate boundary stays.** `playbook-code` cannot depend on the cli crate. The cli builds the settings and the HTTP fetcher and passes them in `Env`.

## Alternatives considered

- **A background server pool.** Warm answers, but it adds lifecycle, a socket and orphan handling. [ADR 0048](0048-collector-daemon-started-by-commands.md) shows what that costs. Revisit only if the benchmark shows cold start dominating.
- **Trust the workspace by default.** Rejected: opening a repo would run its build scripts.
- **More Tree-sitter heuristics instead of servers.** Never type aware. Kept as the fallback.
- **Bundle servers in the binary.** Rejected in ADR 0026.
- **Install `gopls` with `go install` for the user.** Runs the network and a compiler on the user's behalf. Rejected.
- **Keep installs in the data folder.** That folder holds databases only ([data folder](../internals/11-data-folder.md)).

## Consequences

- Cold start is paid on every semantic call: seconds for TypeScript and Pyright, possibly much longer for `rust-analyzer`. The timeout and fallback bound it. The benchmark (index versus `rg` plus Read versus LSP) records it, and the docs state only measured numbers.
- Users must opt in twice: install (or point at) a server, and trust the repo.
- The binary grows by the two crates only; servers are not bundled.
- Bumping a server pin is a source change that needs two downloads and review.
- A killed CLI on macOS can leave a stubborn server running until it sees EOF.

## Maintainer answers

On 2026-11-04 the maintainer delegated the open questions and took the recommended answer for each.

- No `code-intelligence` skill ships with this feature.
- `search_counter` keeps ignoring `playbook code` calls made through Bash.
- No policy guard rule is added for `config set code.lsp.trustWorkspace`. The rule that agents must not set it without an explicit user instruction lives in the docs.
- The benchmark bars (90 percent, 25 percent, 5 seconds) are informational, not kill bars.
- The pbdoc hold on landing is lifted.
