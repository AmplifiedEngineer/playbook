# CLI help text style guide

How the help text of a `playbook` command reads. The text comes from doc comments and `#[arg(help = "...")]` in the Rust source. The lint in `crates/cli/tests/help_lint.rs` checks the mechanical parts. The rest is for review, and this page says which is which.

The page is kept by hand. It lists every threshold of the lint, so a change that loosens a rule shows up in the diff of this page next to the code. A test fails when a banned word below is missing from the page.

## How a person reads the text

1. **Summary.** The first sentence of the doc comment. `playbook commands` and the root help show it too. Start with a verb: `Print`, `List`, `Check`, `Create`, `Remove`, `Run`. Name the result or the effect, not the mechanism. No final period on a one-line summary.
2. **Details.** After a blank doc-comment line. Full sentences with periods. Say what the command changes, what it reads, when to use it and the exit codes that matter. Name no source files and no Rust paths.
3. **Example.** A command with three or more visible arguments, or any required one, ends with an `Example:` line, such as `Example: playbook upgrade --check`. The line must parse with the real CLI.
4. **Flags.** A value flag is a noun phrase that names the thing and, for a number, its unit: `Seconds between reads`. A boolean flag is an imperative: `Print JSON instead of text`. Do not repeat a default that clap prints as `[default: x]`.
5. **Positionals.** Say what is expected and its form: `Absolute path of the repo to trust`. Not only the name.
6. **Plain English.** Follow the writing style of the repo. No filler, and no en or em dashes.

## Rules and how they are enforced

Rules H1 to H13 are in `crates/cli/tests/help_lint.rs`. The thresholds sit between the `RULES-BEGIN` and `RULES-END` markers in that file.

| Rule | What it checks | Threshold |
|---|---|---|
| H1 | Every command has an about | none |
| H2 | Every flag and positional has help | none |
| H3 | Summary word count | 6 to 12 for a top-level command, 3 to 14 for a nested one |
| H4 | Summary length | 70 characters at most |
| H5 | Summary does not open with filler | not `Helpers`, `Checks`, `Mode`, `The`, `A`, `An` |
| H6 | First sentence of argument help | 2 to 100 characters |
| H7 | Argument help opening | a capital letter or a backtick, and no leading `The`, `A`, `An` |
| H8 | No repeated default | help has no `(default` and no `default:` when the argument has a default |
| H9 | Example present | a leaf with 3 or more visible arguments, or a required one |
| H10 | Example parses | every `Example:` line parses with the real CLI |
| H11 | Unit named | a flag with a numeric default names a unit word |
| H12 | One word for one concept | none of the banned words below |
| H13 | No final period | a one-line help does not end with a period |

The unit words for H11 are `seconds`, `results`, `rows`, `times`, `calls`, `dollars`, `tokens`, `lines`, `bytes`, `characters` and `days`.

These points are review items. No test checks them, so a reviewer does:

- Whether the first word is a real imperative verb (H5 only blocks the filler words).
- Whether the summary names the result and not the mechanism.
- The wording of the details.
- Whether an example is worth having.
- The wording of positionals.
- The second paragraph rule for long flag help: the first sentence stays short, and the rest goes in a second paragraph.
- A full pass over the text of the area a change touches.
- A loosened rule: an edited threshold, an edited word table, or an exemption that lets more text pass.

## One word for one concept

H12 bans the variants on the left. Text inside backticks and `Example:` lines is exempt, because some of these words are command and flag names. Match is by whole word. Only `WU`, `segment` and their plurals match the exact case, so a sentence-initial `Segment` passes.

| Banned | Use |
|---|---|
| `folder`, `folders` | directory |
| `pull request`, `pull requests` | PR |
| `repository` | repo |
| `WU`, `WUs` | Work Unit |
| `segment`, `segments` | Segment |
| `dir` | directory |

Shared concepts use one phrase everywhere: `Print JSON instead of text`, `PR number`, `Plan slug`. Other fixed terms are `worktree`, `slug`, `settings.json`, `JSON` and `Claude Code`.

## Allowlists

Text that broke a rule before the lint existed sits in `crates/cli/tests/help_lint_allowlist/`, one file per area. The files only shrink. A PR that fixes an allowlisted item deletes its line in the same PR, because a stale line fails the lint. New commands get no allowlist and must pass every rule on day one.
