# Playbook

An opinionated toolkit for Claude Code. It gives you slash commands, skills and subagents for planning, building, reviewing and remembering, plus a small Rust binary that runs the safety guards, hooks, launcher and status line.

The skills ship as a Claude Code plugin and work on their own. The binary, which adds the guards, hooks, status line and launcher, is optional. Only want the Agent Budget Dashboard? See [pbtop](docs/guides/11-pbtop.md).

![pbtop on the monthly view, with the spend and token charts, using sample data](docs/images/pbtop-month.svg)

*`pbtop`, the Agent Budget Dashboard, on the monthly view with sample data.*

## Install

Pick the first route that fits. Each one installs the `playbook` binary. Then run `playbook setup`.

1. **Homebrew** (macOS and Linux):

   ```bash
   brew install amplifiedengineer/tap/playbook
   playbook setup
   ```

   Use the full name. It trusts only this formula. See [Homebrew tap trust](#homebrew-tap-trust).

2. **Install script**, when you have no Homebrew. It downloads the release binary, checks its SHA256 checksum, and its build attestation when `gh` can check it, puts it on `PATH`, and asks whether to run `playbook setup`:

   ```bash
   curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | bash
   ```

3. **Marketplace plugin**, for the skills only. It adds no binary, hooks or guards:

   ```bash
   claude plugin marketplace add amplifiedengineer/marketplace
   claude plugin install playbook@pragmatic-engineer
   ```

For the best result, install the binary first and the plugin after it. Nix, Windows (experimental, with the raw `.exe`), a pinned version and a route with no `curl | bash` are in the [install guide](docs/guides/00-install.md). To see what `playbook setup` changes before you agree, read [What setup installs](docs/guides/05-what-setup-installs.md).

Pin a version of the install script with `PLAYBOOK_REF=v0.41.0`. Update with `brew upgrade playbook` on Homebrew, or `playbook update` on the other routes. On a brew install, `playbook update` stops and tells you to run `brew upgrade playbook`. Run `playbook doctor check` to see what is in place, or `/playbook:doctor` in a Claude Code session.

### Homebrew tap trust

Since Homebrew 6.0.0, a tap that is not official needs your trust. Installing a fully qualified name trusts only that formula:

```bash
brew install amplifiedengineer/tap/playbook
```

The install records that trust, so a later `brew upgrade playbook` works by short name. To install by short name without using the full name first, trust the formula:

```bash
brew tap amplifiedengineer/tap
brew trust --formula amplifiedengineer/tap/playbook
```

`pbtop` is a separate formula: `brew trust --formula amplifiedengineer/tap/pbtop`. To trust every current and future formula in the tap, run `brew trust amplifiedengineer/tap`. This is broader, so prefer per-formula trust. Run `brew trust` to list trusted entries, and `brew untrust <target>` to remove one. If Homebrew says a formula is not trusted, run the trust command it names. More in the [install guide](docs/guides/00-install.md#homebrew-tap-trust).

Moving from the old tap `pragmatic-engineer/tap`: run `brew uninstall playbook`, `brew untap pragmatic-engineer/tap`, then `brew install amplifiedengineer/tap/playbook`. To keep the installed formula, run `git -C "$(brew --repository pragmatic-engineer/tap)" remote set-url origin https://github.com/amplifiedengineer/homebrew-tap.git` and then `brew update`. Details are in the [install guide](docs/guides/00-install.md#moving-from-the-old-tap-name).

## A first session

```text
/playbook:plan          turn an idea into a verified plan
/playbook:implement     build it with subagents and TDD, one commit per work unit
/playbook:create-pull-request
/playbook:review        recommends a quick or a deep review, then runs the one you pick
```

For a small bug, `/playbook:fix` goes from a failing test to one pull request. For a big choice, `/playbook:adr` records the decision. The [guides](docs/guides/01-plan-and-implement.md) walk through each flow. The full list of commands is in the [slash command table](docs/reference/commands.md).

## Docs

Start at the [docs index](docs/README.md).

- [Install](docs/guides/00-install.md): every route, upgrade and uninstall.
- [Launcher](docs/guides/09-launcher.md), [telemetry](docs/guides/10-telemetry.md) and [pbtop](docs/guides/11-pbtop.md).
- [Settings](docs/guides/12-settings.md), [security defaults](docs/guides/13-security-defaults.md), [models and effort](docs/guides/14-models-and-effort.md), [memory](docs/guides/15-memory.md) and [auto mode](docs/guides/04-auto-mode.md).
- [Usage sync](docs/guides/17-sync.md): the opt-in way to share usage between your own machines.
- [CLI and config reference](docs/reference/cli-and-config.md), [hook reference](docs/reference/hooks.md), [config keys](docs/reference/config-keys.md), [slash commands](docs/reference/commands.md), [layers](docs/reference/layers.md) and [repository layout](docs/reference/repository-layout.md).
- [Guides](docs/README.md#guides), [Reference](docs/README.md#reference), [Authoring](docs/README.md#authoring) and [Internals](docs/README.md#internals).
- [Decisions](docs/adr/): the architecture decision records.

## Security

To report a vulnerability, see [SECURITY.md](SECURITY.md).

## License

Apache License 2.0. See `LICENSE`.
