# ADR-0037: Release Notes Built from Conventional Commits

- **Status:** Accepted. Decisions 1.1, 1.3, 2, 3 and 5.6 are amended by [ADR 0038](0038-one-release-line.md): the plugin ships in the `vX.Y.Z` release, so it has no component, release page, notes or version bump of its own, and its paths count for playbook. Decision 2 is also amended by [ADR 0039](0039-release-workflow.md): the release starts from `workflow_dispatch`, not a pushed tag. The `telemetry` row of decision 1.1 ended with `telemetry-v0.2.0`: [ADR 0038](0038-one-release-line.md) decision 9 moves telemetry onto the `v` line, and [ADR 0041](0041-retire-playbook-telemetry.md) retires the `playbook-telemetry` binary and formula and keeps `pbtop`. Decision 7 is amended by [ADR 0047](0047-repo-tidy.md): the removal lands after 0.37.0. The GitHub release body may start with a model-written summary above the list (issue 1785, `scripts/release-summary.sh`): `release-notes.json` stays the attested record (decision 2) that `playbook update` reads (decision 5.1), and the body list comes from it.
- **Date created:** 2026-10-16
- **Date modified:** 2026-10-17
- **Supersedes:** [ADR 0035](0035-breaking-change-notices.md).
- **Amends:** [ADR 0032](0032-plugin-in-the-playbook-repo.md) decision 5, which takes the plugin release notes from the changelog, [ADR 0034](0034-self-healing-upgrades.md) decisions 13 and 14, which show and require the ADR 0035 record, and [ADR 0036](0036-release-check-and-automatic-updates.md) decisions 6 and 7.7, which read it during an automatic update.

## Context

ADR 0035 asked for a hand written entry in `breaking-changes.json` for every breaking change. Three guards failed a pull request, a plugin version bump and a release when a marked commit had no entry. The plugin entries had a copy in `plugin.json` with a guard of its own, and an environment variable passed the state of the notice between the installers and the binary.

Playbook is before 1.0, so a breaking change can ship in any release. The user wants a heads up when one does. A hard gate on each marked commit is more than that needs. The commits already hold the facts. The project uses Conventional Commits, and a squash merge turns the pull request title and body into the commit message, so a `!` in the title or a `BREAKING CHANGE:` footer in the body reaches the history of `main`.

## Decision

1. **A generator.** `scripts/release-notes.sh` builds the notes of one release from the commit subjects. It uses `git` and `jq`, which every runner has.
   1. *Which commits.* The commits after the previous tag of the same component, up to `HEAD`, that touch the paths the component is built from. Merge commits are left out. The previous tag is the highest tag of the component below the version.

      | Component | Tag | Paths |
      |---|---|---|
      | `playbook` | `vX.Y.Z` | `crates` except `crates/plugin-checks`, the root `Cargo.toml`, `install.sh`, `settings.shared.json`, `prompts`, `Brewfile` |
      | `telemetry` | `telemetry-vX.Y.Z` | `crates/telemetry`, `crates/data`, `crates/common` |
      | `plugin` | `playbook-plugin-vX.Y.Z` | `plugins/playbook` |

      The playbook binary builds `crates/telemetry`, so a change there shows in both. ADR 0035 left it out of playbook. `Cargo.lock` counts for none, since every dependency bump rewrites it.
   2. *Sections.* The notes have these sections, in this order. An empty section is left out.

      | Section | Commits |
      |---|---|
      | Breaking changes | `!` after the type or scope, or a `BREAKING CHANGE:` or `BREAKING-CHANGE:` footer |
      | Features | `feat` |
      | Fixes | `fix` |
      | Performance | `perf` |
      | Other changes | every other type, such as `docs`, `refactor`, `build`, `ci`, `test`, `chore`, `style` and `revert`, and any subject that does not follow Conventional Commits |

      A breaking commit shows only in the first section. Its text is the footer text when the commit has a footer, else the subject text. A trailing `(#123)` in the subject is the pull request number. A subject follows Conventional Commits when it is a lowercase type, an optional scope in parentheses, an optional `!`, a colon and a space, then the text.
   3. *Two outputs.* The script prints `release-notes.json`, and renders markdown from that file, so both say the same thing. The JSON has these fields:

      | Field | Type | Meaning |
      |---|---|---|
      | `format` | integer | `1`. Adding a field or a section name keeps the format, and a reader ignores what it does not know. Removing, renaming or retyping a field raises it. |
      | `component` | string | `playbook`, `telemetry` or `plugin`. |
      | `version` | string | The release, as a bare `MAJOR.MINOR.PATCH`. |
      | `tag` | string | The tag of the release, such as `v0.27.0`. |
      | `previous`, `previous_tag` | string or `null` | The previous release of the component, as a bare version and as a tag, or `null` for the first one. |
      | `date` | string | The day of the release commit in UTC, as `YYYY-MM-DD`. |
      | `sections` | list | Objects `{name, title, items}`, in the order above. `name` is `breaking`, `features`, `fixes`, `performance` or `other`. An empty section is left out. |

      Each item has `type`, a string that is the commit type, or `other` for a subject that does not follow Conventional Commits; `scope`, a non empty string or `null`; `text`, a string; `pr`, a positive integer or `null`; and `breaking`, a boolean.

      The JSON keeps the text as the commit has it, with control characters removed. The markdown puts a backslash before each markup character, so a subject cannot add link or image markup, HTML, emphasis or a mention to the release page. GitHub still links a bare URL. Each line is the text and a link to the pull request. Lines under Other changes start with the type. The markdown ends with a link that compares the two tags.
2. **The release page.** On a `v*` or `telemetry-v*` tag, `release.yml` builds the body from the generator: the upgrade note in `.github/release-notes-upgrade.md` for playbook, or the assets note for telemetry, then the notes. It replaces the notes GitHub generates. It attaches `release-notes.json` as an asset, and the `checksums` job lists it in `SHA256SUMS` and attests it with the binaries. A release that exists before the run keeps its body and gets the asset. A dry run prints the body and the JSON in the job summary. `plugin-release.yml` builds the plugin release body with the same script, from the commits that touch `plugins/playbook`.
3. **No gates.** `breaking-changes.json`, `scripts/check-breaking.sh`, the check in `verify-version`, the breaking part of the `version-bump` plugin guard, the `### Breaking` changelog heading rule, `metadata.breakingChanges` in `plugin.json` and its mirror check are removed. A plugin change still needs a version bump and a changelog section. The `breaking-changes` pull request check stays as a warning that never fails: when the title has `!` and the body has no `BREAKING CHANGE:` footer, it asks for a footer that says what breaks and what to do.
4. **The policy before 1.0.** `CONTRIBUTING.md` states it. A breaking change can ship in any release. Its commit carries `!` or a `BREAKING CHANGE:` footer with a one line action for the user. The release notes show it first. The rule of ADR 0034 decision 14 still holds for playbook's own config, its data files, the entries it writes into Claude Code settings and the contract between the binary and the plugin: a change to them keeps working after an upgrade or ships a migration that runs on first use. Only its last sentence, which asks for an entry in `breaking-changes.json`, is replaced: a change that a user still notices is marked breaking in its commit.
5. **Delivery.**
   1. *`playbook update`.* After it resolves the release, the update downloads that release's `release-notes.json` and checks it against the `SHA256SUMS` it already downloads for the binary. It prints the Breaking changes section, if there is one, and the line `Full notes: <release url>` before the swap. When the user saw the notes, the update records the new version as shown (decision 5.3) right after the swap, before the heal, so the first run line does not repeat them. When only the link showed, it records nothing, and the first run line shows the release. When the running version is below the `previous` of the notes, the update crosses more than one release, and a line names the releases page. When the asset is missing, does not download, does not match its checksum or does not parse, the update prints only the release URL. The notes never fail an update.
   2. *Automatic updates.* The detached process reads the same asset in the same way. It holds the release, and falls back to the notice of ADR 0036 decision 5, when the notes list a breaking change, or when their `previous` is above the running version, since the notes of the releases in between are unknown. An asset that does not match, does not parse or has a `format` this binary does not know holds the release for a day. A release without the asset has no known breaking change. The report of a finished update is the line `playbook updated from A to B. Release notes: <url>` and its warnings.
   3. *First run after any other upgrade.* After `brew upgrade`, the install script or a manual download, the first step that sees the new binary prints one line on stderr: `playbook updated from A to B. Release notes: <url>`. That step is the first run step of ADR 0034 decision 13, `playbook migrate`, `playbook doctor --fix` or `playbook init`. The `session-init` hook adds the same line to the session message. The line needs no network. It reads `notices/binary` in the `state` table of `config.db`, else `versions/binary`, and moves `notices/binary` to the running version in one transaction, so it shows once. When neither key exists and `notices::older_binary_ran` finds a home that a binary of 0.26 or earlier used, the line is `playbook updated to B. Release notes: <url>`. `playbook update --print-notes <file>` moves the same key after it prints, so a step that runs after it prints nothing. A fresh install and a downgrade show nothing. `notices/plugin` is no longer written.
   4. *The install script.* After it replaces an older binary, the script downloads `release-notes.json` and checks it against the `SHA256SUMS` it already reads. The script prints `playbook updated from A to B.` and runs the new binary with `playbook update --print-notes <file> --from A`, which prints the Breaking changes section and the `Full notes: <url>` line, and moves the key. When the asset is missing or does not match, the binary gets a path with no file and prints only the link, and still moves the key. A binary without the flag gets the link from the script, and its first step prints the first run line. Only an upgrade shows notes.
   5. *Homebrew.* The caveats of each formula name the release notes of the installed version in one line, built from `#{version}`. The bump script writes no block, and the first run line covers the upgrade itself.
   6. *The plugin.* The plugin release page carries its notes. The `session-init` line that names an older binary than the plugin needs stays (ADR 0034 decision 10).
6. **Rollout.** The change lands in this order: the generator and the release workflows; the removal of the pull request, plugin and release gates; the binary, which reads `release-notes.json` and stops reading `breaking-changes.json` in 0.35.0; the Homebrew tap; then, in 0.36.0, the removal of the file, its release asset and `playbook breaking`, which a 0.34 binary still uses when it updates to 0.35.0. The ADR 0035 rules that are still in the tree apply until the change that removes them merges.
7. **Removed code.** These go in 0.36.0, since a 0.34 binary that updates itself to 0.35.0 still reads `breaking-changes.json` and runs the new binary's `playbook breaking`. The `playbook breaking` command, the record reader in `crates/cli/src/breaking.rs`, the `PLAYBOOK_BREAKING_SHOWN` variable and its handling in the install script, the update and the heal, the tap's breaking block and its cases in the bump script, and the tests of all of these. ADR 0035 stays as it was, marked superseded.

## Alternatives considered

- **Keep the record of ADR 0035.** Rejected: before 1.0 a gate on each marked commit costs more than the notice is worth, and the plugin copy needs a version bump for a wording fix.
- **The notes GitHub generates.** Rejected: they list pull request titles with no sections and no data file, so the binary cannot find the breaking changes.
- **A release tool such as git-cliff or release-please.** Rejected for now: each adds a dependency and a configuration format to learn. The script is a short `git log` and one `jq` program.
- **Notes built into the binary.** Rejected: a running binary cannot know about later releases.
- **Fetch the notes on the first run.** Rejected: the first run step and the session hook must stay offline and fast. They print a link.

## Consequences

- A breaking change needs no extra file. Its commit says what breaks, and the release notes show it first.
- The notes are as good as the commit subjects and footers. A breaking commit without a marker shows as a normal change. The pull request warning is the only reminder.
- The notes of a release cover that release alone. An update that crosses several releases shows the breaking changes of the newest one and links the releases page for the rest.
- The first run line after `brew upgrade` or a manual download carries a link, with no list of changes.
- In `update.mode = auto`, any release with a breaking change waits for the user. They see its notes when they run `playbook update`.
- A 0.34 binary reads `breaking-changes.json` when it updates itself, and asks the new binary for `playbook breaking`, so both stay through the 0.35 line. A 0.34 binary that skips 0.35 and updates straight to 0.36.0 or later finds no file and does not hold; it treats a missing file as a release with no breaking changes. Its first run line still names the release notes.
