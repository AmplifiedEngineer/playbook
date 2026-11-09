# Architecture decision records

Each file here records one decision and why it was made. A decision can have a blueprint, a quality report or a test mapping file next to it. Only the decision itself has a row below. New records come from `/playbook:adr`. Back to the [docs map](../README.md).

| ADR | Title | Status | Date |
|---|---|---|---|
| [0001](0001-package-toolkit-as-plugin.md) | Package the toolkit as an opt-in Claude Code plugin | Accepted, amended by 0002, 0006, 0007, 0030 | 2026-07-23 |
| [0002](0002-plugin-based-install-safety-hooks.md) | Plugin based install with always-on safety hooks | Accepted, amended by 0006, 0007, 0031 | 2026-07-30 |
| [0003](0003-purpose-built-subagents.md) | Purpose-built subagents over generic fallbacks | Accepted | 2026-08-07 |
| [0004](0004-graph-first-memory.md) | Graph-first memory retrieval and triggered capture | Accepted | 2026-08-10 |
| [0005](0005-python-hooks-and-config-scripts.md) | Migrate the hooks and the config scripts from shell to Python | Superseded by 0007 | 2026-08-11 |
| [0006](0006-separate-product-tree-from-runtime-dir.md) | Separate the product tree from the Claude Code runtime directory | Accepted | 2026-08-12 |
| [0007](0007-rust-binary-for-hooks-and-launcher.md) | Move the hooks and the launcher into a single Rust binary | Accepted, amended by 0044 | 2026-08-13 |
| [0008](0008-bounded-memory-injection-with-prompt-recall-and-handoff-continuity.md) | Bounded memory injection, prompt-time recall, and handoff continuity across /clear | Accepted | 2026-08-24 |
| [0009](0009-evidence-verified-memory-capture-and-bounded-handoff-nudging.md) | Evidence-verified memory capture and bounded handoff nudging | Accepted | 2026-08-25 |
| [0010](0010-agent-agnostic-repo-local-storage.md) | Agent-agnostic repo-local storage | Superseded by 0012 | 2026-08-30 |
| [0011](0011-memory-signals-usage-staleness-similarity-and-consolidation.md) | Memory signals: usage promotion, staleness, similarity edges, and consolidation | Accepted | 2026-08-27 |
| [0012](0012-unify-state-under-config-playbook.md) | Unify home and repo-local state under `$HOME/.config/playbook/` | Accepted | 2026-09-03 |
| [0013](0013-org-level-memory-scoping.md) | Org-level memory scoping | Accepted | 2026-09-04 |
| [0014](0014-retire-memory-md-index.md) | Retire MEMORY.md as a parallel memory index | Accepted | 2026-09-09 |
| [0015](0015-multi-agent-support.md) | Multi-agent support via a thin agent-neutral core and per-agent adapters | Accepted | 2026-10-07 |
| [0016](0016-migration-framework.md) | A registry of ordered migrations run from `playbook init` | Accepted, amended by 0034 and 0046 | 2026-10-07 |
| [0017](0017-per-component-effort-ceilings.md) | Per-component effort ceilings | Accepted | 2026-10-09 |
| [0018](0018-tls-platform-verifier-not-adopted.md) | Keep the bundled root certificates, do not adopt the TLS platform verifier yet | Accepted | 2026-10-09 |
| [0019](0019-pr-stack-review.md) | Review a whole pull request stack with shared context | Accepted | 2026-10-09 |
| [0020](0020-usage-terminal-ui.md) | A terminal UI for `playbook usage`, with ratatui and crossterm | Accepted, amended | 2026-10-10 |
| [0021](0021-reviewer-at-opus-medium.md) | Ship the reviewer agent at Opus medium | Accepted | 2026-10-10 |
| [0022](0022-effort-auto-tune-from-evidence.md) | Tune effort from local evidence, in three stages | Accepted | 2026-10-10 |
| [0023](0023-commands-at-medium.md) | Ship the Opus and Sonnet commands at medium | Accepted | 2026-10-11 |
| [0024](0024-session-management-view-and-lifecycle.md) | Session management view with a derived record and suggest first lifecycle | Accepted | 2026-10-11 |
| [0025](0025-self-improving-playbook.md) | A self improving playbook proposes config patches behind a staged trust ladder | Proposed, amended on 2026-10-21 | 2026-10-11 |
| [0026](0026-local-code-index-in-the-binary.md) | Ship a local code index in the binary, accepting about 5 MB of size | Accepted | 2026-10-12 |
| [0027](0027-anydoc-as-optional-local-tool.md) | Use anydoc as an optional local tool found on PATH, never vendored | Superseded by 0051 | 2026-10-12 |
| [0028](0028-per-agent-auto-budget.md) | Budget auto mode per agent, with the closing steps allowed at a cap | Accepted, amended on 2026-10-17 | 2026-10-12 |
| [0029](0029-cargo-workspace-monorepo.md) | Keep the Rust crates in one Cargo workspace | Accepted, amended by 0030, 0038 and 0039 | 2026-10-13 |
| [0030](0030-skills-and-commands-in-their-own-repo.md) | Keep the skills, commands and agents in their own repository | Accepted, partly superseded by 0031 and 0032 | 2026-10-13 |
| [0031](0031-hooks-in-settings-json.md) | Put the hooks in settings.json | Accepted, amended by 0034 | 2026-10-14 |
| [0032](0032-plugin-in-the-playbook-repo.md) | Keep the plugin in the playbook repository | Accepted, amended by 0037, partly superseded by 0038 | 2026-10-14 |
| [0033](0033-telemetry-on-by-default.md) | Turn Claude Code telemetry on by default at install | Accepted, amended by 0044 | 2026-10-15 |
| [0034](0034-self-healing-upgrades.md) | One plugin root resolver and migrations between versions | Accepted, amended by 0037, 0038, 0042, 0044 and 0046 | 2026-10-15 |
| [0035](0035-breaking-change-notices.md) | Breaking change notices on every update channel | Superseded by 0037 | 2026-10-15 |
| [0036](0036-release-check-and-automatic-updates.md) | A release check and automatic updates | Accepted, amended by 0037 | 2026-10-15 |
| [0037](0037-release-notes-from-conventional-commits.md) | Release notes built from Conventional Commits | Accepted, amended by 0038, 0039 and 0047 | 2026-10-16 |
| [0038](0038-one-release-line.md) | One release line for the binary and the plugin | Accepted, amended by 0041 | 2026-10-16 |
| [0039](0039-release-workflow.md) | A release workflow that runs every step | Accepted, amended by 0041, 0046 and on 2026-10-17 | 2026-10-16 |
| [0040](0040-rename-init-to-setup.md) | Rename `playbook init` to `playbook setup` and keep `init` as a deprecated name | Accepted, amended by 0044 | 2026-10-16 |
| [0041](0041-retire-playbook-telemetry.md) | Retire the `playbook-telemetry` binary and formula, and keep `pbtop` | Accepted, amended by 0043, 0047 and 0048 | 2026-10-16 |
| [0042](0042-attribution-off-in-setup.md) | Turn off Claude Code attribution in setup | Accepted, amended by 0044 and on 2026-10-19 | 2026-10-17 |
| [0043](0043-collector-service-in-setup.md) | Keep the usage collector running with a per-user service | Superseded by 0048 | 2026-10-17 |
| [0044](0044-drop-the-shell-launcher.md) | Drop the shell launcher and turn every setup part on with `--yes` | Accepted, amended on 2026-10-19 and by 0048 | 2026-10-18 |
| [0045](0045-rm-guard-roots-and-parser.md) | Read rm commands as real words and build in the safe roots (cut down 2026-10-19) | Accepted | 2026-10-18 |
| [0046](0046-migrations-by-release-version.md) | Attach each migration to the release that introduced it | Accepted, amended on 2026-10-19 and by 0048 | 2026-10-18 |
| [0047](0047-repo-tidy.md) | Tidy the repo root, the settings seed and the leftovers | Accepted, amended on 2026-10-20 | 2026-10-19 |
| [0048](0048-collector-daemon-started-by-commands.md) | Start the usage collector from playbook commands and keep it running | Accepted | 2026-10-19 |
| [0049](0049-spend-limit-credential-access.md) | Read the Claude Code login token to fetch the server spend limit | Superseded by 0050 | 2026-10-20 |
| [0050](0050-limits-from-claude-data-not-the-login-token.md) | Derive usage limits from Claude Code's own data, not the login token | Accepted | 2026-10-20 |
| [0051](0051-bundle-anydoc-as-pbdoc-helper.md) | Bundle the anydoc converter as the pbdoc helper | Accepted | 2026-10-21 |
| [0052](0052-signed-apt-and-dnf-repository.md) | Signed apt and dnf Package Repository | Superseded | 2026-10-22 |
| [0053](0053-windows-experimental-cli.md) | Windows Is an Experimental CLI | Accepted | 2026-10-22 |
| [0054](0054-completion-wiring-in-shell-files.md) | Setup Wires Shell Completion Into Shell Files | Accepted | 2026-10-23 |
| [0055](0055-watch-mode-for-github-events.md) | A polling watch mode for GitHub events, with rules, autonomy levels and a durable queue | Proposed | 2026-10-25 |
| [0056](0056-share-one-code-index-across-worktrees.md) | Share One Code Index Across the Worktrees of a Repository | Accepted | 2026-10-27 |
| [0057](0057-index-memory-fact-bodies-with-fts5.md) | Index Memory Fact Bodies with SQLite FTS5 | Accepted | 2026-11-04 |
| [0058](0058-answer-symbol-questions-with-language-servers.md) | Answer Symbol Questions With Language Servers, One Per Call, Behind a Trust Gate | Superseded by 0060 | 2026-10-21 |
| [0059](0059-tombstone-format.md) | Tombstone format for on disk state | Accepted | 2026-11-05 |
| [0060](0060-use-tree-sitter-only-for-the-code-index.md) | Answer Code Questions From the Tree-sitter Index Only, With a Precision and Recall Gate | Accepted | 2026-11-06 |
| [0061](0061-opt-in-usage-sync.md) | Sync Usage Between One User's Computers Only When the User Turns It On | Proposed | 2026-11-07 |
