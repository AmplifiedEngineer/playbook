# Playbook docs

How to use and extend this Claude Code config. The [README](../README.md) covers install, Homebrew tap trust and a first session. These pages go deeper: why the config is shaped the way it is, real workflows end to end, how to add your own pieces, and how the machine works underneath.

Read them in order, or jump to what you need.

## Guides

Task-oriented workflows.

- [Install](guides/00-install.md): requirements, every install route (Homebrew first), Homebrew tap trust, upgrade and uninstall.
- [Plan and implement](guides/01-plan-and-implement.md): design with `/playbook:plan`, build with `/playbook:implement`, fix a small bug with `/playbook:fix`.
- [Review and PR flow](guides/02-review-and-pr-flow.md): commit, review, and work through feedback.
- [Decisions and memory](guides/03-decisions-and-memory.md): record choices with `/playbook:adr`, build project knowledge with `/playbook:learn-project`.
- [Auto mode](guides/04-auto-mode.md): run commands without questions, with a spend cap.
- [What setup installs](guides/05-what-setup-installs.md): every change `playbook setup` makes, what stays on your machine, and how to undo each part.
- [Reading documents](guides/06-reading-documents.md): read a PDF or Office file as cheaper text with the bundled converter.
- [The rm guard](guides/07-rm-guard.md): the short rule that denies `rm` of `/` or your home folder, and what it leaves to you.
- [The policy guard](guides/08-policy-guard.md): the optional guard that is off by default, exactly what it denies, how to turn it on and off and its pitfalls.
- [The launcher](guides/09-launcher.md): `playbook cc`, `playbook ccd`, the `pb` alias and worktrees.
- [Telemetry](guides/10-telemetry.md): the local collector, what setup writes and how to turn it on or off.
- [pbtop](guides/11-pbtop.md): the Agent Budget Dashboard on its own, and the retired `playbook-telemetry`.
- [Settings](guides/12-settings.md): `playbook config`, tiers and examples.
- [Security defaults](guides/13-security-defaults.md): the opt-in `permissions` block and its limits.
- [Models and effort](guides/14-models-and-effort.md): the model tiers, fallbacks, effort ceiling and routing.
- [Memory](guides/15-memory.md): the local memory store and `memory.source`.
- [Watch acceptance run](guides/16-watch-acceptance-run.md): the day-long run that proves watch mode on a real repository, step by step.

## Reference

Pages to look things up in.

- [Config keys](reference/config-keys.md): every `playbook config` key with its default, allowed values and what it does.
- [CLI and config reference](reference/cli-and-config.md): every command, flag and config key, generated from the code so it cannot drift. For a short list, run `playbook commands`.
- [Hook reference](reference/hooks.md): every hook setup wires, with its event, matcher, what it reads and writes, and how to turn it off.
- [Policy table](reference/policy-table.md): the model and effort each agent and command runs at.
- [Codex support](reference/codex-support.md): what works in Codex, what does not and why.
- [Slash commands](reference/commands.md): every `/playbook:*` command in one table.
- [Layers](reference/layers.md): the seven layers `/playbook:doctor` reports.
- [Repository layout](reference/repository-layout.md): the crates and the plugin folder.

## Authoring

Extend the config.

- [Commands, skills, and hooks](authoring/01-commands-skills-hooks.md): templates for each extension point.
- [Authoring agents](authoring/02-authoring-agents.md): the frontmatter schema, both binding mechanisms, and the parametrize or split rule.

## Internals

How the machine works, and why it is shaped this way.

- [Launcher and hooks](internals/01-launcher-and-hooks.md): the `playbook cc` launcher and the hook lifecycle.
- [Model routing and memory](internals/02-model-routing-and-memory.md): tiers, effort policy, `playbook route` and the memory graph.
- [Worktree engine](internals/03-worktree.md): the `playbook cc launch worktree` background subshell, node_modules cloning, and rebase conflict resolution.
- [Agent Budget Dashboard](internals/04-usage-dashboard.md): `playbook usage`, the terminal view and its keys, where the numbers come from and the store.
- [Headless mode](internals/05-headless-mode.md): what runs without a person at the keyboard, real `claude -p` exit codes, how the hooks behave headless, and a first CI use case.
- [Release channels](internals/06-release-channels.md): how a tagged release reaches the GitHub assets and the Homebrew tap.
- [Migrations](internals/07-migrations.md): how `playbook setup` carries an install across breaking changes, the three migration kinds, and how edits to placed files are detected.
- [State store](internals/08-state-store.md): where playbook keeps its internal state, how to inspect it with `playbook state list`, and the one-time import of the old files.
- [Sessions](internals/09-sessions.md): `playbook sessions`, the read only list of recent sessions with repo, branch, worktree, PR, state, context size and cost, and what it never does.
- [Document conversion benchmark](internals/10-document-conversion-benchmark.md): the measurement behind the `docs.convert` default.
- [Data folder](internals/11-data-folder.md): the one folder that holds every database, what each file is, how older files move in, and how to clean up.
- [The system prompt](internals/12-system-prompt.md): what the custom prompt defines, and why a custom prompt behaves better than the default.
- [The memory system](internals/13-memory-system.md): the global, org, and project fact store, typed edges, and how it feeds the commands.
- [Why the pieces are shaped this way](internals/14-why-the-pieces-are-shaped-this-way.md): why commands, skills, agents and hooks are separate, why each model and effort level was chosen, and where to change them.
- [Codex skills](internals/15-codex-skills.md): where `playbook skills sync` puts the playbook skills for Codex, how names, ownership and rendering work, and what Codex lacks.
- [CLA and DCO checks](internals/18-cla-and-dco-checks.md): how a contributor signs by commit, what the two checks verify and how the rollout keeps open PRs unblocked.
- [Cleanup, tombstones and gc](internals/17-gc-and-tombstones.md): what `playbook gc` removes, how a tombstone is kept and restored, the stores and the build output table.
- [PR-Agent and the review pipeline](internals/16-pr-agent-and-the-review-pipeline.md): the decision to try PR-Agent's ideas in the playbook review pipeline first, and which ones.

## Decisions

Architecture decision records in [`docs/adr/`](adr/). A decision can have a blueprint, quality report or test mapping file next to it; only the decision itself is listed here.

| ADR | Title | Status | Date |
|---|---|---|---|
| [0001](adr/0001-package-toolkit-as-plugin.md) | Package the toolkit as an opt-in Claude Code plugin | Accepted, amended by 0002, 0006, 0007, 0030 | 2026-07-23 |
| [0002](adr/0002-plugin-based-install-safety-hooks.md) | Plugin based install with always-on safety hooks | Accepted, amended by 0006, 0007, 0031 | 2026-07-30 |
| [0003](adr/0003-purpose-built-subagents.md) | Purpose-built subagents over generic fallbacks | Accepted | 2026-08-07 |
| [0004](adr/0004-graph-first-memory.md) | Graph-first memory retrieval and triggered capture | Accepted | 2026-08-10 |
| [0005](adr/0005-python-hooks-and-config-scripts.md) | Migrate the hooks and the config scripts from shell to Python | Superseded by 0007 | 2026-08-11 |
| [0006](adr/0006-separate-product-tree-from-runtime-dir.md) | Separate the product tree from the Claude Code runtime directory | Accepted | 2026-08-12 |
| [0007](adr/0007-rust-binary-for-hooks-and-launcher.md) | Move the hooks and the launcher into a single Rust binary | Accepted, amended by 0044 | 2026-08-13 |
| [0008](adr/0008-bounded-memory-injection-with-prompt-recall-and-handoff-continuity.md) | Bounded memory injection, prompt-time recall, and handoff continuity across /clear | Accepted | 2026-08-24 |
| [0009](adr/0009-evidence-verified-memory-capture-and-bounded-handoff-nudging.md) | Evidence-verified memory capture and bounded handoff nudging | Accepted | 2026-08-25 |
| [0010](adr/0010-agent-agnostic-repo-local-storage.md) | Agent-agnostic repo-local storage | Superseded by 0012 | 2026-08-30 |
| [0011](adr/0011-memory-signals-usage-staleness-similarity-and-consolidation.md) | Memory signals: usage promotion, staleness, similarity edges, and consolidation | Accepted | 2026-08-27 |
| [0012](adr/0012-unify-state-under-config-playbook.md) | Unify home and repo-local state under `$HOME/.config/playbook/` | Accepted | 2026-09-03 |
| [0013](adr/0013-org-level-memory-scoping.md) | Org-level memory scoping | Accepted | 2026-09-04 |
| [0014](adr/0014-retire-memory-md-index.md) | Retire MEMORY.md as a parallel memory index | Accepted | 2026-09-09 |
| [0015](adr/0015-multi-agent-support.md) | Multi-agent support via a thin agent-neutral core and per-agent adapters | Accepted | 2026-10-07 |
| [0016](adr/0016-migration-framework.md) | A registry of ordered migrations run from `playbook init` | Accepted, amended by 0034 and 0046 | 2026-10-07 |
| [0017](adr/0017-per-component-effort-ceilings.md) | Per-component effort ceilings | Accepted | 2026-10-09 |
| [0018](adr/0018-tls-platform-verifier-not-adopted.md) | Keep the bundled root certificates, do not adopt the TLS platform verifier yet | Accepted | 2026-10-09 |
| [0019](adr/0019-pr-stack-review.md) | Review a whole pull request stack with shared context | Accepted | 2026-10-09 |
| [0020](adr/0020-usage-terminal-ui.md) | A terminal UI for `playbook usage`, with ratatui and crossterm | Accepted, amended | 2026-10-10 |
| [0021](adr/0021-reviewer-at-opus-medium.md) | Ship the reviewer agent at Opus medium | Accepted | 2026-10-10 |
| [0022](adr/0022-effort-auto-tune-from-evidence.md) | Tune effort from local evidence, in three stages | Accepted | 2026-10-10 |
| [0023](adr/0023-commands-at-medium.md) | Ship the Opus and Sonnet commands at medium | Accepted | 2026-10-11 |
| [0024](adr/0024-session-management-view-and-lifecycle.md) | Session management view with a derived record and suggest first lifecycle | Accepted | 2026-10-11 |
| [0025](adr/0025-self-improving-playbook.md) | A self improving playbook proposes config patches behind a staged trust ladder | Proposed, amended on 2026-10-21 | 2026-10-11 |
| [0026](adr/0026-local-code-index-in-the-binary.md) | Ship a local code index in the binary, accepting about 5 MB of size | Accepted | 2026-10-12 |
| [0027](adr/0027-anydoc-as-optional-local-tool.md) | Use anydoc as an optional local tool found on PATH, never vendored | Superseded by 0051 | 2026-10-12 |
| [0028](adr/0028-per-agent-auto-budget.md) | Budget auto mode per agent, with the closing steps allowed at a cap | Accepted, amended on 2026-10-17 | 2026-10-12 |
| [0029](adr/0029-cargo-workspace-monorepo.md) | Keep the Rust crates in one Cargo workspace | Accepted, amended by 0030, 0038 and 0039 | 2026-10-13 |
| [0030](adr/0030-skills-and-commands-in-their-own-repo.md) | Keep the skills, commands and agents in their own repository | Accepted, partly superseded by 0031 and 0032 | 2026-10-13 |
| [0031](adr/0031-hooks-in-settings-json.md) | Put the hooks in settings.json | Accepted, amended by 0034 | 2026-10-14 |
| [0032](adr/0032-plugin-in-the-playbook-repo.md) | Keep the plugin in the playbook repository | Accepted, amended by 0037, partly superseded by 0038 | 2026-10-14 |
| [0033](adr/0033-telemetry-on-by-default.md) | Turn Claude Code telemetry on by default at install | Accepted, amended by 0044 | 2026-10-15 |
| [0034](adr/0034-self-healing-upgrades.md) | One plugin root resolver and migrations between versions | Accepted, amended by 0037, 0038, 0042, 0044 and 0046 | 2026-10-15 |
| [0035](adr/0035-breaking-change-notices.md) | Breaking change notices on every update channel | Superseded by 0037 | 2026-10-15 |
| [0036](adr/0036-release-check-and-automatic-updates.md) | A release check and automatic updates | Accepted, amended by 0037 | 2026-10-15 |
| [0037](adr/0037-release-notes-from-conventional-commits.md) | Release notes built from Conventional Commits | Accepted, amended by 0038, 0039 and 0047 | 2026-10-16 |
| [0038](adr/0038-one-release-line.md) | One release line for the binary and the plugin | Accepted, amended by 0041 | 2026-10-16 |
| [0039](adr/0039-release-workflow.md) | A release workflow that runs every step | Accepted, amended by 0041, 0046 and on 2026-10-17 | 2026-10-16 |
| [0040](adr/0040-rename-init-to-setup.md) | Rename `playbook init` to `playbook setup` and keep `init` as a deprecated name | Accepted, amended by 0044 | 2026-10-16 |
| [0041](adr/0041-retire-playbook-telemetry.md) | Retire the `playbook-telemetry` binary and formula, and keep `pbtop` | Accepted, amended by 0043, 0047 and 0048 | 2026-10-16 |
| [0042](adr/0042-attribution-off-in-setup.md) | Turn off Claude Code attribution in setup | Accepted, amended by 0044 and on 2026-10-19 | 2026-10-17 |
| [0043](adr/0043-collector-service-in-setup.md) | Keep the usage collector running with a per-user service | Superseded by 0048 | 2026-10-17 |
| [0044](adr/0044-drop-the-shell-launcher.md) | Drop the shell launcher and turn every setup part on with `--yes` | Accepted, amended on 2026-10-19 and by 0048 | 2026-10-18 |
| [0045](adr/0045-rm-guard-roots-and-parser.md) | Read rm commands as real words and build in the safe roots (cut down 2026-10-19) | Accepted | 2026-10-18 |
| [0046](adr/0046-migrations-by-release-version.md) | Attach each migration to the release that introduced it | Accepted, amended on 2026-10-19 and by 0048 | 2026-10-18 |
| [0047](adr/0047-repo-tidy.md) | Tidy the repo root, the settings seed and the leftovers | Accepted, amended on 2026-10-20 | 2026-10-19 |
| [0048](adr/0048-collector-daemon-started-by-commands.md) | Start the usage collector from playbook commands and keep it running | Accepted | 2026-10-19 |
| [0049](adr/0049-spend-limit-credential-access.md) | Read the Claude Code login token to fetch the server spend limit | Superseded by 0050 | 2026-10-20 |
| [0050](adr/0050-limits-from-claude-data-not-the-login-token.md) | Derive usage limits from Claude Code's own data, not the login token | Accepted | 2026-10-20 |
| [0051](adr/0051-bundle-anydoc-as-pbdoc-helper.md) | Bundle the anydoc converter as the pbdoc helper | Accepted | 2026-10-21 |
| [0055](adr/0055-watch-mode-for-github-events.md) | A polling watch mode for GitHub events, with rules, autonomy levels and a durable queue | Proposed | 2026-10-25 |
| [0059](adr/0059-tombstone-format.md) | Tombstone format for on disk state | Accepted | 2026-11-05 |
