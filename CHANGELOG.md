# Changelog

Release notes for playbook: the binary and the plugin in `plugins/playbook`. From 0.34.0 they ship together as one `vX.Y.Z` release with one version ([ADR-0038](docs/adr/0038-one-release-line.md)). Each release has a section named `## playbook <version>`, newest first, which the release pull request adds from the notes that `scripts/release-notes.sh` builds. The sections from 0.33.0 down are plugin releases from before the two lines joined. The binary skips 0.27 to 0.33: it goes from 0.26 to 0.34.0.

## playbook 0.43.0

### Breaking changes

- **plugin**: `/playbook:commit-and-push` is now `/playbook:commit`. ([#1750](https://github.com/AmplifiedEngineer/playbook/pull/1750))
- **plugin**: `/playbook:quick-review` and `/playbook:deep-review` are removed. Use `/playbook:review --quick` or `--deep`. ([#1759](https://github.com/AmplifiedEngineer/playbook/pull/1759))
- **size**: The CLI drops `--allow-large` from `playbook deliver` and removes `playbook segment resplit`. `playbook segment size` loses `--limit` and stops printing `over=`, and the pre-flight JSON stops printing `checks.size`. Scripts that use any of these must change. ([#1786](https://github.com/AmplifiedEngineer/playbook/pull/1786))

### Features

- **pbtop**: add up to 8 panels to a tab and arrange them automatically ([#1745](https://github.com/AmplifiedEngineer/playbook/pull/1745))
- **pbtop**: fold projection into the budget strip and add an hourly chart ([#1749](https://github.com/AmplifiedEngineer/playbook/pull/1749))
- **plugin**: keep commit-and-push as a deprecated stub ([#1752](https://github.com/AmplifiedEngineer/playbook/pull/1752))
- **plugin**: rename /playbook:create-pull-request to pr-create ([#1751](https://github.com/AmplifiedEngineer/playbook/pull/1751))
- **pbtop**: put every chart on the first tab by default ([#1753](https://github.com/AmplifiedEngineer/playbook/pull/1753))
- **pbtop**: redraw the Budget strip with bars, a pace marker and fewer words ([#1754](https://github.com/AmplifiedEngineer/playbook/pull/1754))
- **plugin**: rename /playbook:address-pr-comments to pr-comments ([#1755](https://github.com/AmplifiedEngineer/playbook/pull/1755))
- **plugin**: rename the handoff commands to handoff-save and show ([#1763](https://github.com/AmplifiedEngineer/playbook/pull/1763))
- **plugin**: rename learn-project and repo-audit to learn and audit ([#1767](https://github.com/AmplifiedEngineer/playbook/pull/1767))
- **setup**: repair missing or stale telemetry keys ([#1769](https://github.com/AmplifiedEngineer/playbook/pull/1769))
- **setup**: remove IS\_DEMO that blocks telemetry ([#1770](https://github.com/AmplifiedEngineer/playbook/pull/1770))
- **statusline**: read the last main turn from a transcript tail ([#1772](https://github.com/AmplifiedEngineer/playbook/pull/1772))
- **statusline**: estimate the next message cost and cache state ([#1773](https://github.com/AmplifiedEngineer/playbook/pull/1773))
- **statusline**: show the next message estimate on line 3 ([#1774](https://github.com/AmplifiedEngineer/playbook/pull/1774))
- **usage**: filter telemetry queries by account ([#1778](https://github.com/AmplifiedEngineer/playbook/pull/1778))
- **pbtop**: scope every panel by the picked account ([#1779](https://github.com/AmplifiedEngineer/playbook/pull/1779))
- **usage**: add --account to summary, json and pbtop ([#1780](https://github.com/AmplifiedEngineer/playbook/pull/1780))

### Fixes

- **plugin**: show real checkboxes in the review findings picker ([#1746](https://github.com/AmplifiedEngineer/playbook/pull/1746))
- **usage**: keep account moves across a usage reset ([#1775](https://github.com/AmplifiedEngineer/playbook/pull/1775))
- **usage**: repair tab-joined account keys on ingest ([#1776](https://github.com/AmplifiedEngineer/playbook/pull/1776))
- **telemetry**: stop the false session warning and name IS\_DEMO ([#1768](https://github.com/AmplifiedEngineer/playbook/pull/1768))

### Performance

- **pr**: read the start PR while the stack API answers in pr stack ([#1740](https://github.com/AmplifiedEngineer/playbook/pull/1740))

### Other changes

- docs: show code search and document reading in the README ([#1742](https://github.com/AmplifiedEngineer/playbook/pull/1742))
- refactor(plugin): retire the git agent ([#1743](https://github.com/AmplifiedEngineer/playbook/pull/1743))
- test(plugin): add a size budget for commands and skills ([#1747](https://github.com/AmplifiedEngineer/playbook/pull/1747))
- test(cli): fix the macOS and load sensitive tests ([#1748](https://github.com/AmplifiedEngineer/playbook/pull/1748))
- refactor(plugin): slim /playbook:pr-comments onto playbook pr comments ([#1757](https://github.com/AmplifiedEngineer/playbook/pull/1757))
- docs(writing): never hard-wrap prose in markdown ([#1758](https://github.com/AmplifiedEngineer/playbook/pull/1758))
- test(plugin): guard the skill call graph with a shrink-only list ([#1760](https://github.com/AmplifiedEngineer/playbook/pull/1760))
- refactor(plugin): rule skills stop loading other skills ([#1761](https://github.com/AmplifiedEngineer/playbook/pull/1761))
- refactor(plugin): slim handoff-save and handoff-show onto the binary ([#1766](https://github.com/AmplifiedEngineer/playbook/pull/1766))
- test(usage): pin the token path at session start ([#1777](https://github.com/AmplifiedEngineer/playbook/pull/1777))
- build(deps): bump rustix to 1.1.6 for RUSTSEC-2026-0486 ([#1787](https://github.com/AmplifiedEngineer/playbook/pull/1787))

## playbook 0.42.0

### Features

- **brief**: add an optional code-facts block to Work Unit briefs ([#1661](https://github.com/amplifiedengineer/playbook/pull/1661))
- **doctor**: show the stored schema versions of usage.db and config.db ([#1663](https://github.com/amplifiedengineer/playbook/pull/1663))
- **code-bench**: add the end-to-end task draw and grader ([#1665](https://github.com/amplifiedengineer/playbook/pull/1665))
- **code-bench**: run the end-to-end tasks and build the report ([#1666](https://github.com/amplifiedengineer/playbook/pull/1666))
- **code-bench**: add the acceptance rule for the brief facts default ([#1667](https://github.com/amplifiedengineer/playbook/pull/1667))
- **code-bench**: add the Claude CLI adapter and the weekly evaluation workflow ([#1669](https://github.com/amplifiedengineer/playbook/pull/1669))
- **usage**: record the machine on every usage row ([#1668](https://github.com/amplifiedengineer/playbook/pull/1668))
- **pbtop**: pick a machine with x and scope the panels ([#1671](https://github.com/amplifiedengineer/playbook/pull/1671))
- **usage**: keep the machine id in usage.db ([#1672](https://github.com/amplifiedengineer/playbook/pull/1672))
- **usage**: label each event by the account in effect at its time ([#1674](https://github.com/amplifiedengineer/playbook/pull/1674))
- **usage**: limit an account move to one session ([#1675](https://github.com/amplifiedengineer/playbook/pull/1675))
- **pbsync**: add the pbsync crate with an HTTP transport ([#1677](https://github.com/amplifiedengineer/playbook/pull/1677))
- **pbsync**: define the usage table and create the remote schema ([#1678](https://github.com/amplifiedengineer/playbook/pull/1678))
- **pbsync**: push usage rows with an allowlist and salted hashes ([#1679](https://github.com/amplifiedengineer/playbook/pull/1679))
- **usage**: add the local tables and the all-machines view for sync ([#1680](https://github.com/amplifiedengineer/playbook/pull/1680))
- **pbsync**: add the settings file and the enable and disable commands ([#1682](https://github.com/amplifiedengineer/playbook/pull/1682))
- **pbsync**: pull usage rows from the other machines ([#1681](https://github.com/amplifiedengineer/playbook/pull/1681))
- **pbsync**: add the push and pull commands ([#1684](https://github.com/amplifiedengineer/playbook/pull/1684))
- **doctor**: show the sync state when sync is on ([#1685](https://github.com/amplifiedengineer/playbook/pull/1685))
- **config**: classify which config keys may sync between machines ([#1688](https://github.com/amplifiedengineer/playbook/pull/1688))
- **update**: install and update pbsync beside the playbook binary ([#1686](https://github.com/amplifiedengineer/playbook/pull/1686))
- **pbsync**: add the purge command ([#1687](https://github.com/amplifiedengineer/playbook/pull/1687))
- **config**: apply a synced config proposal after showing the diff ([#1691](https://github.com/amplifiedengineer/playbook/pull/1691))
- **pbsync**: push and pull the shared config snapshot ([#1689](https://github.com/amplifiedengineer/playbook/pull/1689))
- **org**: carry settings and state over from the old org name ([#1697](https://github.com/amplifiedengineer/playbook/pull/1697))
- **plugin**: prefer the amplifiedengineer marketplace id ([#1701](https://github.com/amplifiedengineer/playbook/pull/1701))
- **commit**: add a deterministic commit all command ([#1702](https://github.com/amplifiedengineer/playbook/pull/1702))
- **cli**: run doctor check for a bare playbook doctor ([#1704](https://github.com/amplifiedengineer/playbook/pull/1704))
- **deliver**: refuse merge commits and check commit type ([#1707](https://github.com/amplifiedengineer/playbook/pull/1707))
- **cli**: add --json and --args-stdin to pr prepare ([#1709](https://github.com/amplifiedengineer/playbook/pull/1709))
- **sessions**: add the sessions.db store ([#1711](https://github.com/amplifiedengineer/playbook/pull/1711))
- **pr**: add pr finish-context ([#1713](https://github.com/amplifiedengineer/playbook/pull/1713))
- **plugin**: install a missing plugin in sync when allowed ([#1712](https://github.com/amplifiedengineer/playbook/pull/1712))
- **plugin**: turn on marketplace auto-update after install ([#1716](https://github.com/amplifiedengineer/playbook/pull/1716))
- **cli**: add inline json mode to commit all ([#1710](https://github.com/amplifiedengineer/playbook/pull/1710))
- **cli**: add rule ids and a branch guard to deliver ([#1717](https://github.com/amplifiedengineer/playbook/pull/1717))
- **cli**: add pr land to wait for checks and merge a PR ([#1719](https://github.com/amplifiedengineer/playbook/pull/1719))
- **plugin**: run commit-and-push as one inline call ([#1714](https://github.com/amplifiedengineer/playbook/pull/1714))
- **deliver**: open pull requests in one deliver call ([#1718](https://github.com/amplifiedengineer/playbook/pull/1718))
- **setup**: install and update the playbook plugin ([#1720](https://github.com/amplifiedengineer/playbook/pull/1720))
- **finish**: drive the finish skill with finish-context and pr land ([#1721](https://github.com/amplifiedengineer/playbook/pull/1721))
- **plugin**: point a behind or missing plugin at playbook setup ([#1722](https://github.com/amplifiedengineer/playbook/pull/1722))
- **cli**: add playbook plugin update ([#1724](https://github.com/amplifiedengineer/playbook/pull/1724))
- **cli**: add playbook plugin install and rename plugin update to plugin upgrade ([#1726](https://github.com/amplifiedengineer/playbook/pull/1726))
- **cli**: make playbook upgrade the primary command and keep update as an alias ([#1734](https://github.com/amplifiedengineer/playbook/pull/1734))

### Fixes

- **install**: use the renamed org and accept older release names ([#1695](https://github.com/amplifiedengineer/playbook/pull/1695))
- **org**: try the next name when attestation times out ([#1696](https://github.com/amplifiedengineer/playbook/pull/1696))
- **plugin**: find installs under renamed marketplaces ([#1698](https://github.com/amplifiedengineer/playbook/pull/1698))
- **cli**: point messages and docs to playbook doctor check ([#1703](https://github.com/amplifiedengineer/playbook/pull/1703))
- **cli**: refresh the marketplace before updating the plugin ([#1705](https://github.com/amplifiedengineer/playbook/pull/1705))
- **cli**: check the plugin folder Claude Code loads when updating ([#1706](https://github.com/amplifiedengineer/playbook/pull/1706))
- **config**: list usage.timeZone as a known config key ([#1715](https://github.com/amplifiedengineer/playbook/pull/1715))
- **deps**: move rusqlite to 0.41.1 for the libsqlite3-sys advisory ([#1727](https://github.com/amplifiedengineer/playbook/pull/1727))

### Other changes

- refactor(usage): make the machine id without the uuid crate ([#1664](https://github.com/amplifiedengineer/playbook/pull/1664))
- docs(code-bench): add the code index evaluation page and its numbers test ([#1670](https://github.com/amplifiedengineer/playbook/pull/1670))
- chore(org): move playbook to the amplifiedengineer org ([#1692](https://github.com/amplifiedengineer/playbook/pull/1692))
- chore: point docs and metadata at the amplifiedengineer org ([#1694](https://github.com/amplifiedengineer/playbook/pull/1694))
- docs: add a rename notice and fix drifted docs ([#1699](https://github.com/amplifiedengineer/playbook/pull/1699))
- docs: point install and update to playbook\@amplifiedengineer ([#1700](https://github.com/amplifiedengineer/playbook/pull/1700))
- refactor(cli): nest pr finish-context as pr finish context ([#1723](https://github.com/amplifiedengineer/playbook/pull/1723))
- refactor(cli): rename pr finish context to pr status ([#1725](https://github.com/amplifiedengineer/playbook/pull/1725))
- refactor(telemetry): write one SVG line per row and take pictures from a shot list ([#1728](https://github.com/amplifiedengineer/playbook/pull/1728))
- docs: cut the README to a 70 line budget and guard it in CI ([#1729](https://github.com/amplifiedengineer/playbook/pull/1729))
- test(docs): fail when a guide names a command that does not exist ([#1731](https://github.com/amplifiedengineer/playbook/pull/1731))
- docs: add a docs map, Get started and Concepts, and move the ADR table ([#1732](https://github.com/amplifiedengineer/playbook/pull/1732))
- docs(recipes): add seven recipes and a runner that checks them ([#1730](https://github.com/amplifiedengineer/playbook/pull/1730))
- test(cli): stop load-sensitive test failures ([#1735](https://github.com/amplifiedengineer/playbook/pull/1735))
- docs: add a code search page and a token benchmark to the guide ([#1733](https://github.com/amplifiedengineer/playbook/pull/1733))

## playbook 0.41.0

### Features

- **pbtop**: add a usage reset key with a confirmation ([#1600](https://github.com/pragmatic-engineer/playbook/pull/1600))
- **review**: review each stacked PR alone with a digest above it ([#1602](https://github.com/pragmatic-engineer/playbook/pull/1602))
- **handoff**: write a handoff when /clear ends a session ([#1606](https://github.com/pragmatic-engineer/playbook/pull/1606))
- **code-bench**: add bench crate with metrics and dependency guard ([#1624](https://github.com/pragmatic-engineer/playbook/pull/1624))
- **usage**: add a stable machine id and name ([#1626](https://github.com/pragmatic-engineer/playbook/pull/1626))
- **code-bench**: add corpus manifest and fetcher ([#1628](https://github.com/pragmatic-engineer/playbook/pull/1628))
- **code-bench**: add process runner and universal-ctags adapter ([#1635](https://github.com/pragmatic-engineer/playbook/pull/1635))
- **code-bench**: add frozen task generator and task set v1 ([#1637](https://github.com/pragmatic-engineer/playbook/pull/1637))
- **code-bench**: add tuned rg candidates and baseline ([#1638](https://github.com/pragmatic-engineer/playbook/pull/1638))
- **code-bench**: add scip oracle adapter and audit sample rate ([#1640](https://github.com/pragmatic-engineer/playbook/pull/1640))
- **code-bench**: add pilot runner and report ([#1641](https://github.com/pragmatic-engineer/playbook/pull/1641))
- **code-bench**: add pilot command and runner integration test ([#1642](https://github.com/pragmatic-engineer/playbook/pull/1642))
- **code-bench**: add gate rules in basis points ([#1643](https://github.com/pragmatic-engineer/playbook/pull/1643))
- **code-bench**: add gate cases and the merge-base data check ([#1644](https://github.com/pragmatic-engineer/playbook/pull/1644))
- **code-bench**: add baseline, floor and override data files ([#1645](https://github.com/pragmatic-engineer/playbook/pull/1645))
- **code**: bind refs to the declaring module when a name repeats ([#1653](https://github.com/pragmatic-engineer/playbook/pull/1653))
- **code**: guard the parse with a tick budget and a nesting cap ([#1654](https://github.com/pragmatic-engineer/playbook/pull/1654))
- **store**: refuse a newer usage and config database schema ([#1656](https://github.com/pragmatic-engineer/playbook/pull/1656))
- **code**: skip generated files when indexing ([#1657](https://github.com/pragmatic-engineer/playbook/pull/1657))
- **code**: store the language list of each worktree at index time ([#1658](https://github.com/pragmatic-engineer/playbook/pull/1658))

### Fixes

- **update**: stop pointing at the retired apt and dnf packages ([#1609](https://github.com/pragmatic-engineer/playbook/pull/1609))
- **pbtop**: keep a model's colour the same across ranges and charts ([#1619](https://github.com/pragmatic-engineer/playbook/pull/1619))
- **code**: accept go in the --lang filter of search and symbol ([#1621](https://github.com/pragmatic-engineer/playbook/pull/1621))
- **common**: keep a child's unread stdin from killing the hook ([#1627](https://github.com/pragmatic-engineer/playbook/pull/1627))

### Other changes

- refactor(agents): spawn the base agent with an effort parameter ([#1612](https://github.com/pragmatic-engineer/playbook/pull/1612))
- refactor(agents): remove the per-session agent variants ([#1615](https://github.com/pragmatic-engineer/playbook/pull/1615))
- docs(telemetry): drop text about the retired playbook-telemetry command ([#1620](https://github.com/pragmatic-engineer/playbook/pull/1620))
- refactor(store): add the playbook-store crate over rusqlite ([#1623](https://github.com/pragmatic-engineer/playbook/pull/1623))
- test(code): add hand-labeled fixtures and conformance tests ([#1625](https://github.com/pragmatic-engineer/playbook/pull/1625))
- refactor(store): move common and data onto playbook-store ([#1629](https://github.com/pragmatic-engineer/playbook/pull/1629))
- test(cli): add per-language acceptance tests for code commands ([#1632](https://github.com/pragmatic-engineer/playbook/pull/1632))
- test(code): add insta snapshots of extracted symbols per language ([#1630](https://github.com/pragmatic-engineer/playbook/pull/1630))
- test(code): add fuzz targets and file-system edge tests ([#1631](https://github.com/pragmatic-engineer/playbook/pull/1631))
- refactor(store): move telemetry onto playbook-store ([#1633](https://github.com/pragmatic-engineer/playbook/pull/1633))
- test(cli): stop three tests from reading the host machine ([#1634](https://github.com/pragmatic-engineer/playbook/pull/1634))
- refactor(store): move the code index onto playbook-store ([#1636](https://github.com/pragmatic-engineer/playbook/pull/1636))
- refactor(store): move the cli onto playbook-store ([#1639](https://github.com/pragmatic-engineer/playbook/pull/1639))
- test(code): add metamorphic and equivalence tests on the fixtures ([#1647](https://github.com/pragmatic-engineer/playbook/pull/1647))
- ci(code-bench): add the nightly benchmark workflow ([#1648](https://github.com/pragmatic-engineer/playbook/pull/1648))
- ci(code-bench): add the mutation workflow and score floor ([#1650](https://github.com/pragmatic-engineer/playbook/pull/1650))
- test(code-bench): run the gate on the committed fixtures in nextest ([#1652](https://github.com/pragmatic-engineer/playbook/pull/1652))
- test(code-bench): read the workflow by line and drop serde\_norway ([#1651](https://github.com/pragmatic-engineer/playbook/pull/1651))
- ci(code-bench): add the fuzz workflow ([#1655](https://github.com/pragmatic-engineer/playbook/pull/1655))

## playbook 0.40.2

### Features

- **pbtop**: move with h and l and scroll the help with j and k
- **pbtop**: switch between local time and UTC with z
- **doctor**: warn when the Claude Code token is not the collector's
- **pbtop**: set the manual monthly budget from a window
- **pbtop**: show the manual and the detected monthly budget together
- **telemetry**: sync Claude Code keys on config set
- **pbtop**: detect Enterprise and hide the 5 hour and weekly windows
- **pbtop**: join the burn-up lines into unbroken strokes
- **pbtop**: color the burnup usage by an HSV gradient
- **pbtop**: add an opt-in spectrum theme
- **statusline**: show local spend against a monthly budget ([#1586](https://github.com/pragmatic-engineer/playbook/pull/1586))
- **usage**: add account list and display names
- **usage**: move events between accounts and undo the move
- **routing**: default routing.escalate to auto ([#1595](https://github.com/pragmatic-engineer/playbook/pull/1595))
- **usage**: list sessions and move events by session boundary
- **usage**: compare session boundaries with a dashboard
- **telemetry**: show one merged Tools table with a transcript marker ([#1575](https://github.com/pragmatic-engineer/playbook/pull/1575))
- **hooks**: warn at session start when the telemetry keys are missing ([#1576](https://github.com/pragmatic-engineer/playbook/pull/1576))
- **pbtop**: link pbtop to playbook and warn on an older pbtop ([#1588](https://github.com/pragmatic-engineer/playbook/pull/1588))
- **usage**: label each session by the credential it used ([#1581](https://github.com/pragmatic-engineer/playbook/pull/1581))
- **pbtop**: pick the account with u and show it in the title ([#1582](https://github.com/pragmatic-engineer/playbook/pull/1582))
- **usage**: run the account commands on a copy of the database
- **effort**: return the Agent tool effort from effort resolve ([#1596](https://github.com/pragmatic-engineer/playbook/pull/1596))
- **plugin**: hide helper skills from the slash menu ([#1597](https://github.com/pragmatic-engineer/playbook/pull/1597))
- **review**: pick findings and comments from a checklist ([#1599](https://github.com/pragmatic-engineer/playbook/pull/1599))
- **nix**: move the Nix flake to pragmatic-engineer/nix ([#1603](https://github.com/pragmatic-engineer/playbook/pull/1603))

### Fixes

- **telemetry**: name custom agents from transcripts
- **pbtop**: keep the stale reported amount in the monthly budget bar ([#1601](https://github.com/pragmatic-engineer/playbook/pull/1601))
- **handoff**: stop key collisions and keep the file on a failed archive ([#1598](https://github.com/pragmatic-engineer/playbook/pull/1598))

### Other changes

- ci(auto-update): update armed PRs one at a time ([#1590](https://github.com/pragmatic-engineer/playbook/pull/1590))
- chore(release): drop the AUR, apt and dnf, scoop and winget channels ([#1604](https://github.com/pragmatic-engineer/playbook/pull/1604))

## playbook 0.40.1

### Features

- **doctor**: warn on worktree shell refusal in Claude Code 2.1.318 ([#1553](https://github.com/pragmatic-engineer/playbook/pull/1553))
- **prose**: warn on passive voice and check text against facts ([#1554](https://github.com/pragmatic-engineer/playbook/pull/1554))
- **agents**: add writer agent that drafts text from facts ([#1565](https://github.com/pragmatic-engineer/playbook/pull/1565))

### Fixes

- **prompt**: recover when Bash is refused in a worktree ([#1555](https://github.com/pragmatic-engineer/playbook/pull/1555))
- **update**: refresh the placed system prompt after a plugin update ([#1561](https://github.com/pragmatic-engineer/playbook/pull/1561))

### Other changes

- test(telemetry): bound the ignored-holder wait by the stop budget plus slack ([#1548](https://github.com/pragmatic-engineer/playbook/pull/1548))
- docs(writing-style): add length limits and ban passive voice ([#1552](https://github.com/pragmatic-engineer/playbook/pull/1552))
- docs(agents): isolate shell agents once a session enters a worktree ([#1559](https://github.com/pragmatic-engineer/playbook/pull/1559))

## playbook 0.40.0

### Breaking changes

- **data**: the code index now lives in one shared store per repo, so after upgrading delete the old per-worktree index files at \~/.config/playbook/data/code/\<owner\>/\<repo\>/\<worktree-id\>.db (playbook setup also removes them). They are a cache, and the next `playbook code index` rebuilds into store-v\<N\>.sqlite3. ([#1526](https://github.com/pragmatic-engineer/playbook/pull/1526))

### Features

- **code**: rank core declarations above tests and docs in context ([#1302](https://github.com/pragmatic-engineer/playbook/pull/1302))
- **release**: AUR package template and renderer ([#1306](https://github.com/pragmatic-engineer/playbook/pull/1306))
- **release**: scoop manifest template and renderer ([#1311](https://github.com/pragmatic-engineer/playbook/pull/1311))
- **code**: index Go files with the tree-sitter-go grammar ([#1305](https://github.com/pragmatic-engineer/playbook/pull/1305))
- **cli**: print help when a required argument is missing ([#1317](https://github.com/pragmatic-engineer/playbook/pull/1317))
- **cli**: group root help into user and agent commands ([#1318](https://github.com/pragmatic-engineer/playbook/pull/1318))
- **cli**: add playbook completions for bash, zsh and fish ([#1319](https://github.com/pragmatic-engineer/playbook/pull/1319))
- **cli**: add completion stub writer for bash, zsh and fish ([#1324](https://github.com/pragmatic-engineer/playbook/pull/1324))
- **cli**: report and remove shell completions in doctor and uninstall ([#1331](https://github.com/pragmatic-engineer/playbook/pull/1331))
- **cli**: snapshot flag attributes to verify help rewrites ([#1334](https://github.com/pragmatic-engineer/playbook/pull/1334))
- **cli**: install shell completions during setup ([#1335](https://github.com/pragmatic-engineer/playbook/pull/1335))
- **config**: add the sessions.autoClose key ([#1366](https://github.com/pragmatic-engineer/playbook/pull/1366))
- **hooks**: record turn and waiting state per session ([#1371](https://github.com/pragmatic-engineer/playbook/pull/1371))
- **sessions**: add a dry run sweep for finished sessions ([#1375](https://github.com/pragmatic-engineer/playbook/pull/1375))
- **plugin**: route code questions through playbook code ([#1377](https://github.com/pragmatic-engineer/playbook/pull/1377))
- **handoff**: load the saved handoff after /compact ([#1391](https://github.com/pragmatic-engineer/playbook/pull/1391))
- **code**: add parser key and extractor digest test ([#1397](https://github.com/pragmatic-engineer/playbook/pull/1397))
- **code**: write worktree, content and manifest rows beside files ([#1398](https://github.com/pragmatic-engineer/playbook/pull/1398))
- **system-prompt**: add autonomy rule for failures ([#1409](https://github.com/pragmatic-engineer/playbook/pull/1409))
- **config**: add gate.maxRounds for the gate loops ([#1429](https://github.com/pragmatic-engineer/playbook/pull/1429))
- **statusline**: add a budget line for the 7 day and 5 hour caps ([#1417](https://github.com/pragmatic-engineer/playbook/pull/1417))
- **telemetry**: rework pbtop with an accessible palette, row layouts and a map editor ([#1473](https://github.com/pragmatic-engineer/playbook/pull/1473))
- **gc**: add playbook gc with tombstones, guarded sweeps and build output cleanup ([#1475](https://github.com/pragmatic-engineer/playbook/pull/1475))
- **memory**: recall candidates from an FTS5 index over fact bodies ([#1517](https://github.com/pragmatic-engineer/playbook/pull/1517))
- **review**: make the review stack aware ([#1413](https://github.com/pragmatic-engineer/playbook/pull/1413))
- **worktree**: clone the main target folder into new worktrees and report it in doctor ([#1447](https://github.com/pragmatic-engineer/playbook/pull/1447))
- **code**: index every worktree of a repo into one shared store ([#1479](https://github.com/pragmatic-engineer/playbook/pull/1479))
- **setup**: register the Bash guards in Codex and report them in doctor ([#1449](https://github.com/pragmatic-engineer/playbook/pull/1449))
- **usage**: decide the monthly dollar figure and explain it in doctor ([#1520](https://github.com/pragmatic-engineer/playbook/pull/1520))
- **statusline**: show the monthly dollar figure on line 4 ([#1523](https://github.com/pragmatic-engineer/playbook/pull/1523))
- **watch**: add watch mode for GitHub events ([#1516](https://github.com/pragmatic-engineer/playbook/pull/1516))
- **pbtop**: tag local spend as this machine, show Claude's report ([#1524](https://github.com/pragmatic-engineer/playbook/pull/1524))

### Fixes

- **windows**: run on an 8 MiB stack thread ([#1309](https://github.com/pragmatic-engineer/playbook/pull/1309))
- **telemetry**: prefer a detected monthly limit over the manual budget ([#1328](https://github.com/pragmatic-engineer/playbook/pull/1328))
- **code**: make the context token budget cover the whole text output ([#1308](https://github.com/pragmatic-engineer/playbook/pull/1308))
- **cli**: keep the zsh loader silent when compinit fails ([#1360](https://github.com/pragmatic-engineer/playbook/pull/1360))
- **doc**: report the same memory reason from both watchdogs ([#1373](https://github.com/pragmatic-engineer/playbook/pull/1373))
- **usage**: keep the newest plan limit reading across sessions ([#1376](https://github.com/pragmatic-engineer/playbook/pull/1376))
- **telemetry**: say used is this machine only when stale ([#1394](https://github.com/pragmatic-engineer/playbook/pull/1394))
- **handoff**: keep the handoff file when the move to used/ fails ([#1396](https://github.com/pragmatic-engineer/playbook/pull/1396))
- **worktree**: let the sweep read the pull request state of a branch
- unblock main by fixing the help example and adding ADR 0057 ([#1455](https://github.com/pragmatic-engineer/playbook/pull/1455))
- **worktree**: keep open PR worktrees, count only default branch merges ([#1444](https://github.com/pragmatic-engineer/playbook/pull/1444))
- **config**: list the dotted view words in the usage.ui.layout error ([#1518](https://github.com/pragmatic-engineer/playbook/pull/1518))
- **watch**: audit a stale event once ([#1530](https://github.com/pragmatic-engineer/playbook/pull/1530))
- **config**: accept fractional dollars for watch.limits.usdPerDay ([#1522](https://github.com/pragmatic-engineer/playbook/pull/1522))

### Performance

- **config**: read a ready settings store without the full open ([#1367](https://github.com/pragmatic-engineer/playbook/pull/1367))
- **code**: stop allocating per word and per line in index and search ([#1369](https://github.com/pragmatic-engineer/playbook/pull/1369))

### Other changes

- ci(shell): add a CI job for the shell completion tests ([#1320](https://github.com/pragmatic-engineer/playbook/pull/1320))
- test(cli): replace the commands golden with a structural test ([#1322](https://github.com/pragmatic-engineer/playbook/pull/1322))
- refactor(usage): rename the dashboard title to Agent Budget Dashboard ([#1329](https://github.com/pragmatic-engineer/playbook/pull/1329))
- test(cli): lint command help against allowlists that only shrink ([#1332](https://github.com/pragmatic-engineer/playbook/pull/1332))
- refactor(plugin): merge the cli skill into playbook-usage ([#1330](https://github.com/pragmatic-engineer/playbook/pull/1330))
- docs(cli): add a style guide for command help text ([#1339](https://github.com/pragmatic-engineer/playbook/pull/1339))
- refactor(cli): wire zsh completion through a loader file ([#1341](https://github.com/pragmatic-engineer/playbook/pull/1341))
- refactor(test): drop dead\_code allows from hook and parity test support ([#1352](https://github.com/pragmatic-engineer/playbook/pull/1352))
- test(cli): split shared test helpers into smaller modules ([#1351](https://github.com/pragmatic-engineer/playbook/pull/1351))
- refactor: share duplicate helpers across the crates ([#1359](https://github.com/pragmatic-engineer/playbook/pull/1359))
- refactor: share duplicate helpers inside code and telemetry ([#1361](https://github.com/pragmatic-engineer/playbook/pull/1361))
- docs(config): mark launcher.claudeOverride retired and date the retired flags ([#1355](https://github.com/pragmatic-engineer/playbook/pull/1355))
- refactor(init): rename shim.rs to rcfile.rs and date the launcher cleanup ([#1357](https://github.com/pragmatic-engineer/playbook/pull/1357))
- refactor(pbdoc): narrow pub items to pub(crate) ([#1343](https://github.com/pragmatic-engineer/playbook/pull/1343))
- refactor(cli): narrow update and auto cost items to pub(crate) ([#1348](https://github.com/pragmatic-engineer/playbook/pull/1348))
- build(deps): remove unused ratatui dependency ([#1342](https://github.com/pragmatic-engineer/playbook/pull/1342))
- refactor(telemetry): narrow tz helpers to pub(crate) ([#1344](https://github.com/pragmatic-engineer/playbook/pull/1344))
- refactor(code): narrow testkit and walker items to pub(crate) ([#1345](https://github.com/pragmatic-engineer/playbook/pull/1345))
- refactor(cli): narrow test support items to pub(crate) ([#1349](https://github.com/pragmatic-engineer/playbook/pull/1349))
- refactor(cli): narrow statusline items to pub(crate) ([#1346](https://github.com/pragmatic-engineer/playbook/pull/1346))
- refactor: remove redundant clones and by-value arguments ([#1358](https://github.com/pragmatic-engineer/playbook/pull/1358))
- refactor: remove dead code and gate test-only helpers ([#1353](https://github.com/pragmatic-engineer/playbook/pull/1353))
- test(cli): stop the collector daemon tests from failing under load ([#1362](https://github.com/pragmatic-engineer/playbook/pull/1362))
- refactor(init): remove the no-op shell-init-rc-line migration ([#1354](https://github.com/pragmatic-engineer/playbook/pull/1354))
- ci(windows): fail the Windows check on warnings ([#1356](https://github.com/pragmatic-engineer/playbook/pull/1356))
- refactor(cli): narrow commit message sanitizer items to pub(crate) ([#1347](https://github.com/pragmatic-engineer/playbook/pull/1347))
- build: enable the unreachable\_pub lint workspace-wide ([#1350](https://github.com/pragmatic-engineer/playbook/pull/1350))
- refactor(cli): remove unsafe code from the playbook sources ([#1368](https://github.com/pragmatic-engineer/playbook/pull/1368))
- refactor(pbdoc): drop the unsafe counting allocator and libc calls ([#1365](https://github.com/pragmatic-engineer/playbook/pull/1365))
- refactor: replace unsafe libc calls with rustix ([#1364](https://github.com/pragmatic-engineer/playbook/pull/1364))
- refactor(cli): remove unsafe code from the integration tests ([#1370](https://github.com/pragmatic-engineer/playbook/pull/1370))
- build: forbid unsafe code in the workspace ([#1374](https://github.com/pragmatic-engineer/playbook/pull/1374))
- chore(deps): bump zip from 8.6.0 to 9.0.5 ([#1383](https://github.com/pragmatic-engineer/playbook/pull/1383))
- docs(cli): rewrite help text for the everyday commands
- docs(cli): rewrite setup help text and clear its lint allowlist
- docs(cli): rewrite help text for the launcher and usage commands
- docs(cli): reword help text for config, effort and memory
- docs(cli): reword help text for PR and commit commands
- docs(cli): reword help text for review and gate commands
- docs(cli): reword help text for the plan and segment commands
- docs(cli): tighten help wording and restore deps ensure index entry
- docs(cli): reword help text for code, agents and sanitize
- docs(cli): reword help text for the path and route commands
- docs(cli): wrap and tighten the utility help text
- docs(cli): state the topology flag help once
- docs(cli): make worktree help examples runnable
- docs(cli): fix settings examples and finish help wording
- test(cli): stop the help leak check matching .sh inside .shared
- docs(cli): correct the --no-attribution default and match the --only list by id
- docs(cli): describe what cc clean, fresh and raw do
- docs(cli): tighten the config and effort help wording
- docs(cli): match the prose check help to the type check help
- docs(cli): keep the settings examples free of source file names
- docs(cli): drop index summaries that repeat the help text
- docs(cli): keep file extensions out of help examples
- docs(cli): fix help examples and wording found in deep review
- docs(cli): align path wording with directory in index and error
- test(watch): record real GitHub list responses as fixtures ([#1481](https://github.com/pragmatic-engineer/playbook/pull/1481))
- test(telemetry): fix three tests that flake under parallel load ([#1476](https://github.com/pragmatic-engineer/playbook/pull/1476))
- docs(cli): say PR in the remaining help text and keep main's help leak test
- docs(adr): drop the unbuilt dispatch hook from ADR 0011 ([#1433](https://github.com/pragmatic-engineer/playbook/pull/1433))
- test(memory): add the synthetic recall fixture and its loader ([#1405](https://github.com/pragmatic-engineer/playbook/pull/1405))
- test(usage): pin the Codex continuation, two-session and compressed rollout cases ([#1451](https://github.com/pragmatic-engineer/playbook/pull/1451))
- docs(pbtop): generate pbtop-month.svg from the real theme ([#1519](https://github.com/pragmatic-engineer/playbook/pull/1519))
- test: harden flaky collector, handover and install backup tests ([#1525](https://github.com/pragmatic-engineer/playbook/pull/1525))
- test: stop the two-hooks test waiting on a notice a hook may take ([#1533](https://github.com/pragmatic-engineer/playbook/pull/1533))
- test(doctor): leave the free disk space row out of the order comparison ([#1534](https://github.com/pragmatic-engineer/playbook/pull/1534))
- test: keep three tests green when a release fills in the migration since ([#1536](https://github.com/pragmatic-engineer/playbook/pull/1536))

## playbook 0.39.0

### Breaking changes

- **hooks**: cut the rm guard down to rm of / or the home folder ([#1202](https://github.com/pragmatic-engineer/playbook/pull/1202))
- **hooks**: remove the disk-guard hook ([#1230](https://github.com/pragmatic-engineer/playbook/pull/1230))
- **setup**: remove the disk-ballast part and the disk doctor rows ([#1245](https://github.com/pragmatic-engineer/playbook/pull/1245))
- **disk**: remove the playbook disk command ([#1246](https://github.com/pragmatic-engineer/playbook/pull/1246))
- **config**: remove the disk config keys and ADR 0047 ([#1249](https://github.com/pragmatic-engineer/playbook/pull/1249))

### Features

- **disk**: remove a worktree's cargo target folder with it ([#1181](https://github.com/pragmatic-engineer/playbook/pull/1181))
- **update**: update the playbook plugin from update and migrate ([#1182](https://github.com/pragmatic-engineer/playbook/pull/1182))
- **setup**: offer the disk ballast as an opt-in part ([#1186](https://github.com/pragmatic-engineer/playbook/pull/1186))
- **prompt**: require a memory search before claiming no knowledge ([#1191](https://github.com/pragmatic-engineer/playbook/pull/1191))
- **telemetry**: add writer thread with lifetime lock ([#1190](https://github.com/pragmatic-engineer/playbook/pull/1190))
- **hooks**: hint when memory recall finds nothing or only weak matches ([#1194](https://github.com/pragmatic-engineer/playbook/pull/1194))
- **update**: add the ask and auto modes to update.plugin ([#1192](https://github.com/pragmatic-engineer/playbook/pull/1192))
- **skills**: add the skills sync planner and renderer for Codex ([#1196](https://github.com/pragmatic-engineer/playbook/pull/1196))
- **telemetry**: serve with a thread per connection and shutdown ([#1197](https://github.com/pragmatic-engineer/playbook/pull/1197))
- **hooks**: recall memory facts by crate name and alias ([#1198](https://github.com/pragmatic-engineer/playbook/pull/1198))
- **telemetry**: add daemon ensure and stop for the collector ([#1208](https://github.com/pragmatic-engineer/playbook/pull/1208))
- **telemetry**: add playbook usage stop ([#1212](https://github.com/pragmatic-engineer/playbook/pull/1212))
- **telemetry**: start the collector daemon from hooks and commands ([#1215](https://github.com/pragmatic-engineer/playbook/pull/1215))
- **doctor**: show who holds the collector daemon lock ([#1216](https://github.com/pragmatic-engineer/playbook/pull/1216))
- **skills**: add playbook skills sync for Codex ([#1200](https://github.com/pragmatic-engineer/playbook/pull/1200))
- **update**: update the plugin at the first command after an upgrade ([#1199](https://github.com/pragmatic-engineer/playbook/pull/1199))
- **update**: start the plugin update from the session hook ([#1209](https://github.com/pragmatic-engineer/playbook/pull/1209))
- **skills**: refresh synced skills when playbook is upgraded ([#1213](https://github.com/pragmatic-engineer/playbook/pull/1213))
- **hooks**: make the policy guard optional and off by default ([#1195](https://github.com/pragmatic-engineer/playbook/pull/1195))
- **migrate**: remove the collector service on upgrade ([#1217](https://github.com/pragmatic-engineer/playbook/pull/1217))
- **setup**: retire the collector-service part ([#1218](https://github.com/pragmatic-engineer/playbook/pull/1218))
- **usage**: compare reported spend with local cost ([#1227](https://github.com/pragmatic-engineer/playbook/pull/1227))
- **usage**: model the lines the usage sources carry ([#1228](https://github.com/pragmatic-engineer/playbook/pull/1228))
- **usage**: show crossings and why a limit is missing ([#1229](https://github.com/pragmatic-engineer/playbook/pull/1229))
- **usage**: add the burn-up series for any limit ([#1236](https://github.com/pragmatic-engineer/playbook/pull/1236))
- **usage**: burn-up panel from reported limits ([#1237](https://github.com/pragmatic-engineer/playbook/pull/1237))
- **usage**: mark HTTP 429 replies on the burn-up chart ([#1262](https://github.com/pragmatic-engineer/playbook/pull/1262))
- **release**: support cargo binstall and mise ([#1261](https://github.com/pragmatic-engineer/playbook/pull/1261))
- **usage**: keep a bounded history of window readings ([#1263](https://github.com/pragmatic-engineer/playbook/pull/1263))
- **usage**: add a format version to each rate limit history line ([#1265](https://github.com/pragmatic-engineer/playbook/pull/1265))
- **nix**: add flake with release binaries and hash renderer ([#1275](https://github.com/pragmatic-engineer/playbook/pull/1275))
- **pbdoc**: add the pbdoc helper crate with its CLI contract ([#1291](https://github.com/pragmatic-engineer/playbook/pull/1291))
- **pbdoc**: enforce hard limits in the helper ([#1292](https://github.com/pragmatic-engineer/playbook/pull/1292))
- **pbdoc**: guard the dependency closure and its notices ([#1293](https://github.com/pragmatic-engineer/playbook/pull/1293))
- **doc**: prefer the bundled converter and refuse oversized files ([#1294](https://github.com/pragmatic-engineer/playbook/pull/1294))
- **doc**: serve long conversions in bounded slices ([#1295](https://github.com/pragmatic-engineer/playbook/pull/1295))
- **install**: build, attest and install the pbdoc helper ([#1296](https://github.com/pragmatic-engineer/playbook/pull/1296))
- **update**: swap and remove the pbdoc helper on update and uninstall ([#1297](https://github.com/pragmatic-engineer/playbook/pull/1297))

### Fixes

- **prompt**: do not require task tools that many builds lack ([#1221](https://github.com/pragmatic-engineer/playbook/pull/1221))
- **hooks**: log real compactions with session and context size ([#1250](https://github.com/pragmatic-engineer/playbook/pull/1250))
- **git**: check the worktree and branch first in commit and PR skills ([#1253](https://github.com/pragmatic-engineer/playbook/pull/1253))
- drop retired rm guard rules and keep foreign service files ([#1255](https://github.com/pragmatic-engineer/playbook/pull/1255))
- **update**: recognise .deb and .rpm installs in /usr/bin ([#1266](https://github.com/pragmatic-engineer/playbook/pull/1266))
- **statusline**: honor the auto-compact window in the compaction gap ([#1268](https://github.com/pragmatic-engineer/playbook/pull/1268))

### Performance

- **usage**: cache rate limit history by file mtime and size ([#1270](https://github.com/pragmatic-engineer/playbook/pull/1270))

### Other changes

- chore(deps): bump sha2 from 0.10.9 to 0.11.0 ([#1187](https://github.com/pragmatic-engineer/playbook/pull/1187))
- test(hooks): replay 11 prompts against the memory recall bars ([#1210](https://github.com/pragmatic-engineer/playbook/pull/1210))
- test(hooks): drop the rm guard roots and corpus integration tests ([#1201](https://github.com/pragmatic-engineer/playbook/pull/1201))
- test(cli): pin collector port in setup\_alias tests ([#1222](https://github.com/pragmatic-engineer/playbook/pull/1222))
- docs(hooks): describe the short rm guard rule ([#1203](https://github.com/pragmatic-engineer/playbook/pull/1203))
- refactor(hooks): remove the no-slop-guard hook ([#1193](https://github.com/pragmatic-engineer/playbook/pull/1193))
- test(hooks): drop the rm guard corpus fixture ([#1204](https://github.com/pragmatic-engineer/playbook/pull/1204))
- refactor(hooks): remove the old rm guard module and helpers ([#1205](https://github.com/pragmatic-engineer/playbook/pull/1205))
- refactor(disk): remove the cleanup and ballast code ([#1247](https://github.com/pragmatic-engineer/playbook/pull/1247))
- refactor(disk): remove targets, backups and the classifier ([#1248](https://github.com/pragmatic-engineer/playbook/pull/1248))
- refactor(hooks): remove the first part of the rm guard parser ([#1206](https://github.com/pragmatic-engineer/playbook/pull/1206))
- refactor: prune collector service setup and move account functions ([#1219](https://github.com/pragmatic-engineer/playbook/pull/1219))
- refactor(hooks): remove the rest of the rm guard parser ([#1207](https://github.com/pragmatic-engineer/playbook/pull/1207))
- refactor(service): prune the service manager to its removal paths ([#1220](https://github.com/pragmatic-engineer/playbook/pull/1220))
- docs(commands): add compact and clear checkpoints to plan and implement ([#1251](https://github.com/pragmatic-engineer/playbook/pull/1251))
- docs: document collector daemon and retire service setup ([#1225](https://github.com/pragmatic-engineer/playbook/pull/1225))
- test: fix zone leak, flaky self-heal test and real HOME writes ([#1254](https://github.com/pragmatic-engineer/playbook/pull/1254))
- docs(adr): close the ADR number gaps and pin the sequence ([#1257](https://github.com/pragmatic-engineer/playbook/pull/1257))
- test(hooks): run guard tests in scratch home, fail on HOME use ([#1269](https://github.com/pragmatic-engineer/playbook/pull/1269))
- refactor(plugin): move worktree guard to delegating-subagents ([#1267](https://github.com/pragmatic-engineer/playbook/pull/1267))
- test(hooks): run the Bash guards against Codex-shaped payloads ([#1274](https://github.com/pragmatic-engineer/playbook/pull/1274))
- docs: fix the agent count and add tap trust to the pbtop README ([#1283](https://github.com/pragmatic-engineer/playbook/pull/1283))
- docs(doc): describe the bundled converter ([#1298](https://github.com/pragmatic-engineer/playbook/pull/1298))
- refactor(doc): route every converter lookup through one facade ([#1299](https://github.com/pragmatic-engineer/playbook/pull/1299))

## playbook 0.38.0

### Breaking changes

- **cli**: the `playbook shell-init` command is removed. Start sessions with `pb cc` or `pb ccd`, and delete the `eval "$(playbook shell-init)"` line from your shell rc file, or run `playbook setup` to remove it after a backup. Until 1.0 the old line prints one notice on stderr and the shell still starts. ([#1152](https://github.com/pragmatic-engineer/playbook/pull/1152))

### Features

- **shell**: mark commands that meet a command substitution ([#1127](https://github.com/pragmatic-engineer/playbook/pull/1127))
- **hooks**: add a command parser for the rm guard ([#1126](https://github.com/pragmatic-engineer/playbook/pull/1126))
- **hooks**: list the rm guard's built-in roots per OS ([#1131](https://github.com/pragmatic-engineer/playbook/pull/1131))
- **hooks**: judge rm targets against the built-in roots ([#1132](https://github.com/pragmatic-engineer/playbook/pull/1132))
- **hooks**: follow cd in the rm guard ([#1139](https://github.com/pragmatic-engineer/playbook/pull/1139))
- **hooks**: follow variables in the rm guard ([#1140](https://github.com/pragmatic-engineer/playbook/pull/1140))
- **hooks**: add a built-in deny set to the rm guard ([#1142](https://github.com/pragmatic-engineer/playbook/pull/1142))
- **disk**: add the free space probe and disk.\* config keys ([#1141](https://github.com/pragmatic-engineer/playbook/pull/1141))
- **hooks**: judge more deletion commands in the rm guard ([#1151](https://github.com/pragmatic-engineer/playbook/pull/1151))
- **hooks**: run the rm guard for every Bash command ([#1154](https://github.com/pragmatic-engineer/playbook/pull/1154))
- **migrate**: attach each migration to its release version ([#1137](https://github.com/pragmatic-engineer/playbook/pull/1137))
- **disk**: add disk status and a doctor row ([#1153](https://github.com/pragmatic-engineer/playbook/pull/1153))
- **disk**: add disk clean ([#1155](https://github.com/pragmatic-engineer/playbook/pull/1155))
- **disk**: add disk reserve and disk release ([#1156](https://github.com/pragmatic-engineer/playbook/pull/1156))
- **disk**: list cargo target folders of any project in disk status ([#1158](https://github.com/pragmatic-engineer/playbook/pull/1158))
- **migrate**: name migrations by slug and convert old records ([#1157](https://github.com/pragmatic-engineer/playbook/pull/1157))
- **disk**: classify which Bash commands are builds ([#1166](https://github.com/pragmatic-engineer/playbook/pull/1166))
- **disk**: add the disk-guard hook that stops builds on a full disk ([#1162](https://github.com/pragmatic-engineer/playbook/pull/1162))
- **setup**: rename attribution-off to attribution with ON and OFF ([#1163](https://github.com/pragmatic-engineer/playbook/pull/1163))
- **memory**: add memory stats and print it after a rebuild ([#1169](https://github.com/pragmatic-engineer/playbook/pull/1169))
- **disk**: wire the disk guard into setup, the session and status line ([#1167](https://github.com/pragmatic-engineer/playbook/pull/1167))
- **cli**: add playbook commands, a list built from the clap tree ([#1173](https://github.com/pragmatic-engineer/playbook/pull/1173))
- **setup**: retire the attribution-off migration and add flag aliases ([#1171](https://github.com/pragmatic-engineer/playbook/pull/1171))
- **disk**: clean up old migration backups and stop making more ([#1170](https://github.com/pragmatic-engineer/playbook/pull/1170))
- **hooks**: add playbook hooks explain-rm to show rm guard verdicts ([#1176](https://github.com/pragmatic-engineer/playbook/pull/1176))
- **migrate**: list each item that needs you with its fix ([#1175](https://github.com/pragmatic-engineer/playbook/pull/1175))
- **plugin**: add the playbook:cli skill pointing to playbook commands ([#1177](https://github.com/pragmatic-engineer/playbook/pull/1177))

### Fixes

- **release**: keep full multi-line breaking change footers ([#1125](https://github.com/pragmatic-engineer/playbook/pull/1125))
- **hooks**: let the policy guard allow reads of the config store ([#1129](https://github.com/pragmatic-engineer/playbook/pull/1129))
- **hooks**: judge rm guard targets from real command words ([#1128](https://github.com/pragmatic-engineer/playbook/pull/1128))
- **setup**: name the collector agent com.github.pragmatic-engineer ([#1134](https://github.com/pragmatic-engineer/playbook/pull/1134))
- **migrate**: move the collector agent to its new label ([#1136](https://github.com/pragmatic-engineer/playbook/pull/1136))
- **rm-guard**: allow playbook's folders, protect only irreplaceable data ([#1165](https://github.com/pragmatic-engineer/playbook/pull/1165))
- **policy-guard**: allow store reads, leave deletions to the rm guard ([#1172](https://github.com/pragmatic-engineer/playbook/pull/1172))
- **migrate**: show each pending item once, then once a day ([#1179](https://github.com/pragmatic-engineer/playbook/pull/1179))

### Other changes

- test: give every playbook test a scratch home ([#1130](https://github.com/pragmatic-engineer/playbook/pull/1130))
- docs: add the rm guard guide and ADR 0045 ([#1133](https://github.com/pragmatic-engineer/playbook/pull/1133))
- test(hooks): pin the rm guard's verdicts on a corpus of real commands ([#1138](https://github.com/pragmatic-engineer/playbook/pull/1138))
- docs: describe migrations by slug and release version ([#1160](https://github.com/pragmatic-engineer/playbook/pull/1160))
- refactor(settings): make settings.shared.json the one permissions source ([#1161](https://github.com/pragmatic-engineer/playbook/pull/1161))
- refactor(setup): look for the seed in the seed folder of a checkout ([#1164](https://github.com/pragmatic-engineer/playbook/pull/1164))
- docs(memory): point shipped text at data/memory.db and guard it ([#1174](https://github.com/pragmatic-engineer/playbook/pull/1174))
- refactor: move the shipped seed to seed/ and intents.json to plugins/ ([#1168](https://github.com/pragmatic-engineer/playbook/pull/1168))
- refactor: remove breaking-changes.json and playbook breaking ([#1178](https://github.com/pragmatic-engineer/playbook/pull/1178))
- chore(dev): tidy the justfile, the seed Brewfile and the script tests ([#1180](https://github.com/pragmatic-engineer/playbook/pull/1180))

## playbook 0.37.0

### Breaking changes

- **setup**: `playbook setup --aliases` and `--claude-override` do nothing now. Start sessions with `pb cc` or `playbook cc`, which load the playbook system prompt. A plain `claude` no longer goes through the launcher. ([#1119](https://github.com/pragmatic-engineer/playbook/pull/1119))
- **cc**: the playbook, claude and pb shell functions are gone. `playbook cc` no longer changes the shell's directory, and a plain `claude` no longer loads the system prompt. Run the printed `cd`, and start sessions with `pb cc` or `playbook cc`. ([#1120](https://github.com/pragmatic-engineer/playbook/pull/1120))
- **setup**: `setup --yes` now installs the system prompt and links `pb`. Pass `--no-system-prompt` or `--no-short-name` to keep them off. ([#1122](https://github.com/pragmatic-engineer/playbook/pull/1122))

### Features

- **setup**: link pb from the short-name part ([#1118](https://github.com/pragmatic-engineer/playbook/pull/1118))
- **setup**: remove the retired launcher rc line with consent ([#1121](https://github.com/pragmatic-engineer/playbook/pull/1121))

### Other changes

- docs: describe pb cc without the shell launcher ([#1123](https://github.com/pragmatic-engineer/playbook/pull/1123))

## playbook 0.36.0

### Features

- **setup**: turn off Claude Code attribution with consent ([#1101](https://github.com/pragmatic-engineer/playbook/pull/1101))
- **usage**: add a launchd and systemd service for the collector ([#1104](https://github.com/pragmatic-engineer/playbook/pull/1104))
- **cli**: install, start and remove the collector service ([#1108](https://github.com/pragmatic-engineer/playbook/pull/1108))
- **setup**: keep the usage collector running in the background ([#1105](https://github.com/pragmatic-engineer/playbook/pull/1105))
- **migrate**: remind existing installs once about the collector service ([#1107](https://github.com/pragmatic-engineer/playbook/pull/1107))

### Fixes

- **eval**: find agent files under plugins/playbook ([#1096](https://github.com/pragmatic-engineer/playbook/pull/1096))
- **ci**: run the repo checks on the plugins/playbook layout ([#1095](https://github.com/pragmatic-engineer/playbook/pull/1095))
- **cc**: clear inherited agent variants before a launch ([#1102](https://github.com/pragmatic-engineer/playbook/pull/1102))
- **pbtop**: say why a telemetry view is empty and how to fix it ([#1106](https://github.com/pragmatic-engineer/playbook/pull/1106))
- **auto**: raise the default spend caps to measured session cost ([#1115](https://github.com/pragmatic-engineer/playbook/pull/1115))

### Other changes

- chore(cli): remove code nothing calls ([#1092](https://github.com/pragmatic-engineer/playbook/pull/1092))
- docs: list every ADR in the index and fix stale names ([#1097](https://github.com/pragmatic-engineer/playbook/pull/1097))
- build(toolchain): pin Rust 1.99.1 ([#1098](https://github.com/pragmatic-engineer/playbook/pull/1098))
- chore(deps): bump patch versions and refresh the lockfile ([#1094](https://github.com/pragmatic-engineer/playbook/pull/1094))
- chore(settings): use current tool names in the shared allowlist ([#1099](https://github.com/pragmatic-engineer/playbook/pull/1099))
- test(memory): read the touched time the staleness check reads ([#1090](https://github.com/pragmatic-engineer/playbook/pull/1090))
- docs(plugin): stop telling agents to call TaskOutput ([#1100](https://github.com/pragmatic-engineer/playbook/pull/1100))
- chore: fix gaps a review found in the sweep PRs ([#1103](https://github.com/pragmatic-engineer/playbook/pull/1103))
- test: stop waiting out the migrate lock budget ([#1109](https://github.com/pragmatic-engineer/playbook/pull/1109))

## playbook 0.35.0

### Breaking changes

- **plugin**: give /playbook:setup the flags and questions of setup ([#1073](https://github.com/pragmatic-engineer/playbook/pull/1073))
- /playbook:setup --yes now leaves the launcher and the ([#1077](https://github.com/pragmatic-engineer/playbook/pull/1077))
- **telemetry**: the playbook-telemetry command is gone. Run pbtop with ([#1082](https://github.com/pragmatic-engineer/playbook/pull/1082))

### Features

- **cli**: add playbook setup and deprecate init ([#1064](https://github.com/pragmatic-engineer/playbook/pull/1064))
- **plugin**: add Python engineering standards skill ([#1066](https://github.com/pragmatic-engineer/playbook/pull/1066))
- **plugin**: add the remaining Python standards references ([#1067](https://github.com/pragmatic-engineer/playbook/pull/1067))
- **cli**: name playbook setup in every message and hint ([#1068](https://github.com/pragmatic-engineer/playbook/pull/1068))
- **install**: install pbtop alone with --pbtop ([#1072](https://github.com/pragmatic-engineer/playbook/pull/1072))
- **install**: add install-pbtop.sh as the pbtop entry point ([#1078](https://github.com/pragmatic-engineer/playbook/pull/1078))
- **cli**: read release-notes.json in playbook update ([#1079](https://github.com/pragmatic-engineer/playbook/pull/1079))
- **cli**: print release notes line on first run after upgrade ([#1083](https://github.com/pragmatic-engineer/playbook/pull/1083))
- **install**: print the release notes when replacing a binary ([#1084](https://github.com/pragmatic-engineer/playbook/pull/1084))

### Fixes

- **cli**: finish the review of the setup naming change ([#1076](https://github.com/pragmatic-engineer/playbook/pull/1076))

### Other changes

- docs: finish the init to setup rename ([#1070](https://github.com/pragmatic-engineer/playbook/pull/1070))
- ci(release): run every release step from one workflow ([#1071](https://github.com/pragmatic-engineer/playbook/pull/1071))

## playbook 0.34.0

The first release with one version for the binary and the plugin. The binary goes from 0.26 to 0.34.0, so binary versions 0.27 to 0.33 do not exist.

### Features

- **telemetry**: start the collector from SessionStart, add doctor rows ([#1015](https://github.com/pragmatic-engineer/playbook/pull/1015))
- **review**: follow Conventional Comments in review skills ([#1021](https://github.com/pragmatic-engineer/playbook/pull/1021))
- **telemetry**: add the pbtop layout editor ([#1018](https://github.com/pragmatic-engineer/playbook/pull/1018))
- **review**: add review suggest for a quick or deep call ([#1022](https://github.com/pragmatic-engineer/playbook/pull/1022))
- **telemetry**: add a projection panel to pbtop ([#1026](https://github.com/pragmatic-engineer/playbook/pull/1026))
- **telemetry**: add usage doctor to list limit sources ([#1028](https://github.com/pragmatic-engineer/playbook/pull/1028))
- **review**: add /playbook:review to pick a quick or deep review ([#1027](https://github.com/pragmatic-engineer/playbook/pull/1027))
- **telemetry**: read cached limits and plan in pbtop ([#1031](https://github.com/pragmatic-engineer/playbook/pull/1031))
- **cli**: record versions and heal owned files at session start ([#1037](https://github.com/pragmatic-engineer/playbook/pull/1037))
- **cli**: add doctor --fix and heal after playbook update ([#1038](https://github.com/pragmatic-engineer/playbook/pull/1038))
- **cli**: add migrations for the 0.25 to 0.26 upgrade gap ([#1039](https://github.com/pragmatic-engineer/playbook/pull/1039))
- **cli**: check for newer releases and print a notice ([#1043](https://github.com/pragmatic-engineer/playbook/pull/1043))
- **cli**: add playbook migrate as the front door to migrations ([#1046](https://github.com/pragmatic-engineer/playbook/pull/1046))
- **cli**: add the auto update that installs a newer release ([#1047](https://github.com/pragmatic-engineer/playbook/pull/1047))
- **install**: show breaking changes when replacing a binary ([#1049](https://github.com/pragmatic-engineer/playbook/pull/1049))
- **cli**: heal once on the first run of a new binary ([#1051](https://github.com/pragmatic-engineer/playbook/pull/1051))
- **plugin**: check the breaking changes mirror in plugin.json ([#1052](https://github.com/pragmatic-engineer/playbook/pull/1052))
- **cli**: turn on update.mode auto in the release check ([#1050](https://github.com/pragmatic-engineer/playbook/pull/1050))
- **cli**: show breaking changes in playbook update ([#1048](https://github.com/pragmatic-engineer/playbook/pull/1048))
- **update**: show the release check status and add ADR 0036 ([#1054](https://github.com/pragmatic-engineer/playbook/pull/1054))
- **cli**: show breaking changes once when a new binary heals ([#1055](https://github.com/pragmatic-engineer/playbook/pull/1055))
- **plugin**: add Rust engineering standards skill ([#1060](https://github.com/pragmatic-engineer/playbook/pull/1060))
- **plugin**: add the remaining Rust standards references ([#1061](https://github.com/pragmatic-engineer/playbook/pull/1061))
- **release**: ship the binary and the plugin as one release line

### Fixes

- **update**: stop warning that the plugin differs from the binary ([#1023](https://github.com/pragmatic-engineer/playbook/pull/1023))
- **plugin**: stop using a bare plugin root path ([#1029](https://github.com/pragmatic-engineer/playbook/pull/1029))
- **plugin**: hint to upgrade an old binary ([#1032](https://github.com/pragmatic-engineer/playbook/pull/1032))
- **plugin**: find the plugin root with one resolver ([#1033](https://github.com/pragmatic-engineer/playbook/pull/1033))
- **plugin**: add path plugin-root and fallbacks for an old setup ([#1035](https://github.com/pragmatic-engineer/playbook/pull/1035))
- **doctor**: check the plugin against the supported version range ([#1042](https://github.com/pragmatic-engineer/playbook/pull/1042))
- pass the breaking notice flag only to the steps that read it ([#1056](https://github.com/pragmatic-engineer/playbook/pull/1056))
- **cli**: hide only the breaking entries the caller already showed ([#1058](https://github.com/pragmatic-engineer/playbook/pull/1058))
- **release**: address review findings for one release line

### Other changes

- docs(telemetry): document telemetry on by default and add ADR 0033 ([#1019](https://github.com/pragmatic-engineer/playbook/pull/1019))
- docs(plugin): add a Conventional Commits reference ([#1025](https://github.com/pragmatic-engineer/playbook/pull/1025))
- docs(telemetry): describe tabs, limit sources and usage doctor ([#1034](https://github.com/pragmatic-engineer/playbook/pull/1034))
- test(telemetry): give each collector test its own port ([#1036](https://github.com/pragmatic-engineer/playbook/pull/1036))
- docs(adr): add ADR 0035 and the breaking changes record ([#1041](https://github.com/pragmatic-engineer/playbook/pull/1041))
- docs(plugin): match doctor.md to the new layer 6 checks ([#1044](https://github.com/pragmatic-engineer/playbook/pull/1044))
- docs(plugin): describe playbook migrate in the doctor command ([#1053](https://github.com/pragmatic-engineer/playbook/pull/1053))
- test(cli): ignore SQLite sidecars in the smoke idempotence check ([#1057](https://github.com/pragmatic-engineer/playbook/pull/1057))

## playbook 0.33.0

- `engineering-standards-rust` gets eight more references: async and concurrency, iterators and collections, performance, unsafe and FFI, security and input, testing, tooling and CI, and project structure. The rules index in `SKILL.md` lists each one with a load-when line.
- The tooling reference ends with the Rust setup of pragmatic-engineer/playbook as a worked example.
- `systematic-debugging` and its condition-based-waiting guide point to the Rust skill, the same way they point to the JavaScript one.
- Secrets in `Debug` and error responses for network services now have one home, in the security reference.

## playbook 0.32.0

- A new skill, `engineering-standards-rust`, holds the Rust rules for code and reviews. It sits next to `engineering-standards-javascript` and adds only the Rust specifics. Its first references cover ownership and borrowing, error handling, types and traits, API design, style and naming, and a checklist of antipatterns. Most rules name the Clippy lint or command that checks them.
- `engineering-standards` points to the new skill for Rust, the same way it points to the JavaScript one.
- A skills lint test now checks that each `references/<name>.md` named in a `SKILL.md` exists.

## playbook 0.31.3

- The doctor command text matches the binary again: the upgrade record row hints `run: playbook migrate`, and the next session start or `playbook migrate` brings the record up to date.
- A short note after layer 6 says what to run after an upgrade: `playbook migrate`, or `playbook doctor --fix`, which runs the same step. It also says where each update channel shows breaking changes.

## playbook 0.31.2

- The doctor command text now matches the binary: it says a version difference still passes, describes the layer 6 upgrade record row, and names `playbook init --hooks` as the fix for dead hook entries under `~/.claude/hooks`.

## playbook 0.31.1

- The plugin now declares `minBinaryVersion` 0.26.0 in its metadata. The field is informational for now: nothing in the binary reads it yet.
- commit-and-push and create-pull-request tell the user to upgrade the binary when the rules step fails on an older binary (the error says 'skill reference not found' or 'no plugin root'), then fall back to reading the skill files.

## playbook 0.31.0

- A new command, `/playbook:review`, is the one entry point for reviews. It recommends a quick or a deep review from the diff, asks which you want with the recommendation marked first, then runs it. `--quick` and `--deep` skip the question. In auto mode it takes the recommendation.
- The recommendation comes from a fixed rule list: size, files, packages touched, risky paths (auth, crypto, secrets, CI, release, hooks, migrations, dependencies, the command surface), a security label, the test ratio, and changes that cannot alter behavior (docs, lock files, renames, tests). With the playbook binary it is `playbook review suggest`. Without it the same rules are in `skills/grounding-review/references/review-routing.md`.
- When no rule is decisive, one cheap `review-triage` pass gives a second opinion.
- `/playbook:quick-review` and `/playbook:deep-review` are unchanged and stay as direct entry points.

## playbook 0.30.1

- Fix: the commit-format reference is no longer read through a bare `$CLAUDE_PLUGIN_ROOT` path, which Claude Code does not fill in, so skills failed with "could not find the plugin root folder". The commands and the git agent now load the writing-style skill and read the file from its base directory.
- The same bare variable is gone from the fallback text in create-pull-request and commit-and-push.

## playbook 0.30.0

- A new reference, `conventional-commits`, sits in the writing-style references. It covers the commit format, the type table, scope and ticket rules, breaking changes, the description rules and good and bad examples.
- commit-and-push, create-pull-request (title only), the git agent and implement's per-Work-Unit commit step read it when they write a commit message or a PR title.
- It is based on Conventional Commits 1.0.0 (https://www.conventionalcommits.org/, CC BY 3.0).

## playbook 0.28.0

- Review comments now follow the Conventional Comments standard (https://conventionalcomments.org/): `<label> [decorations]: <subject>` with an optional discussion.
- The grounding-review skill has one definition of the nine labels and the decorations, in a new reference file with a label table and examples.
- A bare `blocking:` becomes `issue (blocking):`. Old comments in the bare form are still read.
- quick-review, deep-review, address-pr-comments, implement, finish-pull-request and the reviewer agents read that reference. The posting tiers now group comments by what must be fixed.
- The format is for code review comments only.

## playbook 0.27.3

- The install command in the doctor hint now names pragmatic-engineer/marketplace, the new name of the marketplace repository.
- The plugin manifest points at the playbook repository and no longer mentions the playbook-hooks plugin.

## playbook 0.27.2

- The plugin now lives in this repository. It moved from pragmatic-engineer/skills at commit a27dbe7c2c996b9d01008eb080edbc7dac639b3b, where its history stays readable.
- No change to the skills, commands, agents or output style.
