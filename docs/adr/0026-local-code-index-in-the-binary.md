# ADR-0026: Ship a Local Code Index in the Binary, Accepting About 5 MB of Size

- **Status:** Accepted, amended by [ADR 0056](0056-share-one-code-index-across-worktrees.md) (decision 2, index location) and by [ADR 0060](0060-use-tree-sitter-only-for-the-code-index.md) (decision 3 withdrawn)
- **Date created:** 2026-10-12
- **Date modified:** 2026-11-06

## Context

Claude Code sessions find code with Grep, Glob and whole file reads. The maintainer brief for `playbook code` asks for a local index that returns exact paths, line ranges, symbols and short excerpts. The plan is `playbook-code` (20 Segments, one pull request each). Three parts of it are hard to reverse and cut against earlier decisions, so this record settles them before the Tree-sitter Segment merges.

## Decision

### 1. Binary size

Tree-sitter and four grammars (Rust, Python, JavaScript, TypeScript and TSX) are compiled in, and FTS5 is turned back on in the bundled SQLite. A scratch build with the release profile measured:

| Change | Release size delta |
| --- | --- |
| FTS5 only | about +150 KB |
| Tree-sitter runtime and the Rust grammar | about +1.17 MB |
| Python grammar | about +0.45 MB |
| JavaScript grammar | about +0.41 MB |
| TypeScript grammar | about +1.44 MB |
| TSX grammar | about +1.44 MB |
| Total | about +5.0 MB |

The v0.23.0 binary for `aarch64-apple-darwin` is 6,551,024 bytes, so the estimate is near 11.6 MB, about 77% larger. ADR-0018 rejected a change of +2.2 MB for size. This record accepts a larger one because the feature is the product of a binding brief that requires in-binary parsing, and the alternatives fail it: external tools need installs on every machine and have unstable output, and regex heuristics break on nested declarations and on the comment and string exclusion that `refs` needs.

The size is bounded, not open ended:

- The S4 Segment stops for a new decision if the measured release delta is more than 15% above +4.9 MB.
- The grammar set is Rust, Python, JavaScript, TypeScript and TSX. Go and C grammars are follow up work after the benchmark.
- A retrieval checkpoint after S10 and the benchmark Segment can stop further language and language server work if the feature does not beat `rg` plus Read.
- Options that cut size later: parse `.js` and `.jsx` with the TSX grammar (about 0.41 MB) or parse `.tsx` with the TypeScript grammar (about 1.44 MB, with partial trees). Neither is taken now.

The FTS5 change removes `-USQLITE_ENABLE_FTS5` from `LIBSQLITE3_FLAGS`. FTS3, RTREE and DBSTAT stay off. Queries bind FTS text as parameters and quote every token.

### 2. Index location

The index is a cache at `$HOME/.config/playbook/repos/<owner>/<repo>/<worktree-id>/code/index.sqlite3`, built with the worktree scoped state helper from ADR-0012. The brief proposed `<repo>/.playbook/code/`, which matches the repo local layout of ADR-0010 that ADR-0012 superseded. Nothing is written inside the repository, so no ignore rule is needed. Each worktree has its own index. A repository with no `origin` remote gets an actionable error, as `gate` does. The schema version lives in `PRAGMA user_version` and a mismatch deletes and rebuilds the file, because the index is disposable. Index files are not removed when a worktree is deleted. `playbook code index --rebuild` and manual removal are the documented cleanup.

### 3. Managed language server downloads and trust

> Withdrawn by [ADR 0060](0060-use-tree-sitter-only-for-the-code-index.md).

Language servers are optional. Three rules bound the risk:

- **Network only on request.** Only `playbook code lsp install` touches the network, through the existing HTTPS only fetcher with its size cap. No other command downloads anything.
- **Hashes live in the source.** The pinned manifest compiled into the binary holds a SHA-256 for GitHub assets and the npm SHA-512 integrity value for tarballs. A hash is never read from the download host. The hash is checked in memory before anything is extracted. Archive extraction rejects absolute paths, `..`, every link entry and device entries, caps entries and size, and clears setuid and setgid bits. Installs go to a private staging directory and are renamed into `lsp/<id>/<version>/` with a receipt. A managed install counts only when it matches the manifest's current version. Bumping a pin is a reviewed source change.
- **Workspace trust.** Servers run workspace code (build scripts, proc macros, the project interpreter, plugins). Semantic commands fail closed: no server starts until the user sets `code.lsp.trustWorkspace` to true for the repository. Without it the command falls back to Tree-sitter and says so. A project file never supplies a server command: an explicit path comes only from the user's own config database. Agents must not set the trust key without an explicit instruction from the user.

## Alternatives considered

- **External tools (`ctags`, `ast-grep`, `ripgrep`).** No size cost, but every user needs installs and output differs by version. Rejected.
- **Regex heuristics with the existing `regex` crate.** Almost no size cost, but they fail the syntax aware requirement. Rejected.
- **Index inside the repository.** Matches a superseded layout and risks committing a cache. Rejected.
- **Bundle language servers in the binary.** Far larger and ties releases to server releases. Rejected.

## Consequences

- Release binaries grow about 5 MB and release legs compile C code for each grammar. CI wall time per leg is recorded in the S4 pull request.
- The `x86_64-apple-darwin` release leg is not built by pull request CI, so a manual release dry run is needed before the first tagged release that includes the grammars.
- Third party licenses for the compiled in code (all MIT) are listed in `NOTICE`.
- Language server support rests on pinned hashes that a maintainer must refresh by hand.
