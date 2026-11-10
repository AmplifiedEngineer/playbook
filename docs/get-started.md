# Get started

Install Playbook, set it up, and take one change from an idea to a pull request. This page is for someone who already uses Claude Code in a Git repository.

## When to use it

- You are new to Playbook and want the shortest path to a first use.
- You installed it and want to know what to run next.

## You need

Claude Code (`claude`), `bash` or `zsh`, `git` and `shasum`. The `gh` command is optional. The [install guide](guides/00-install.md#requirements) lists why each one matters.

## 1. Install the binary

```bash
brew install amplifiedengineer/tap/playbook
```

With no Homebrew, run the install script. It checks the release checksum before it installs anything:

```bash
curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | bash
```

To add the plugin to a binary that did not come from Homebrew, run `playbook plugin install`. The [install guide](guides/00-install.md) covers Nix and Windows.

## 2. Run setup

```bash
playbook setup
```

In a terminal, setup shows one step at a time, says what each step changes and asks before it does anything. It offers to install the Claude Code plugin when it finds `claude`. To preview every change first, run `playbook setup --dry-run`. [What setup installs](guides/05-what-setup-installs.md) lists each change and how to undo it.

Check the result:

```bash
playbook doctor check
```

The table shows which of the seven [layers](reference/layers.md) are in place. Inside Claude Code, `/playbook:doctor` shows the same table.

## 3. Start a session

```bash
pb cc
```

`pb` is a short name for `playbook`. `pb cc` starts Claude Code with the Playbook system prompt, the right model chain and your effort ceiling. It resumes the last session in this directory, or starts a new one. `pb cc list` shows recent sessions.

## 4. Plan, build and review

In the session, give Playbook an idea:

```text
/playbook:plan "add a --json flag to the export command"
```

The plan command reads your code, asks only what it cannot find out, and checks the plan with three reviewers before it asks you to approve. It saves the plan in your plans folder.

Build the plan:

```text
/playbook:implement
```

With no argument, implement lists your saved plans and builds the one you pick. It splits the plan into Segments. Each Segment becomes one small pull request, and each Work Unit becomes one commit. Before it writes code it asks how to deliver them.

Review the result:

```text
/playbook:review
```

Review picks quick or deep, says why, and runs it.

For a small bug, `/playbook:fix` goes from a failing test to one pull request. For a decision that is hard to undo, `/playbook:adr` records it.

## Keep it current

On Homebrew, run `brew upgrade playbook`. For an install script install, run `playbook upgrade`, which updates the plugin too. See [Upgrade](guides/00-install.md#upgrade) in the install guide.

## Next

- [Plan and implement](guides/01-plan-and-implement.md) walks through both commands in detail.
- [Concepts](concepts.md) explains how the plugin, the binary and the hooks fit together.
- [Docs map](README.md) links every page.
