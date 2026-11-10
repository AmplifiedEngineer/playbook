# Without the binary

Read this file only when the session's tool map lacks an intent. A recipe names the intent, and each section below is that intent's fallback. The heading is the intent id.

## run-mode

If `$ARGUMENTS` has both `--auto` and `--ask`, stop with one line: "--auto and --ask conflict; pass one." If it has `--auto`, the mode is `auto`. In every other case the mode is `ask`, the default.

## config-get

Use the defaults of the key the recipe names. For `fix.maxFiles` the default is 3, and for `fix.maxLines` it is 500.

## route-model

Spawn the agent the recipe names, with the model its agent file pins.

## pr-stack

Detect the stack by hand as `skills/grounding-review/references/stack-review.md` section 1 describes.

## review-suggest

Read `skills/grounding-review/references/review-routing.md` in this plugin and apply Part 1 by hand with plain `git` and `gh`. For the whole stack, Part 1 runs on the union of the open PRs' files. The same applies when the command fails, for example an older binary without `--scope` that exits 2.

## effort-resolve

Spawn the base agent with no `effort`, and say in your report that no ceiling was checked.

## effort-choose

Skip the automatic choice and use the effort table in the `playbook:delegating-subagents` skill.

## atlassian-check

The `playbook:atlassian` skill needs `acli` 1.3.39 or newer. Run `acli --version` and compare the `X.Y.Z` in `acli version X.Y.Z-stable` with 1.3.39. No output or no `X.Y.Z` means unreadable; a lower version means too old. Run `command -v twg` to see whether `twg` is present.
