# Release channels

A release reaches users through three channels: the GitHub release assets (built by the `build` and `publish` jobs of `release.yml`), the Homebrew tap `pragmatic-engineer/homebrew-tap` and the plugin marketplace `pragmatic-engineer/marketplace`.

After a release is published and checked, the `downstream` job of `release.yml` opens a pull request in the tap and in the marketplace, as the section on the release workflow describes. The tap also checks every 30 minutes for a newer release and bumps a formula itself when the `downstream` job has not done it within an hour.

A release attaches ten assets (two per target), four Linux packages, `release-notes.json` and `SHA256SUMS`, which lists the other fifteen. It builds no plugin archive. The plugin in `plugins/playbook` ships with the same `vX.Y.Z`, as described next.

## The plugin release

The binary and the plugin share one version line ([ADR 0038](../adr/0038-one-release-line.md)).

- The `version` in `plugin.json` equals the workspace version in `Cargo.toml`. `metadata.minBinaryVersion` is the `X.Y.0` of that minor line.
- The `version-sync` check in `plugin-guards.yml` fails a pull request or a push to `main` when they differ. The `plan` job of `release.yml` checks again before a `v` release.
- The release pull request runs `scripts/bump-version.sh X.Y.Z`, which sets all of them and the `PLAYBOOK_REF` pins together. A plugin change between releases needs no bump.

The marketplace repository `pragmatic-engineer/marketplace` lists the plugin as a `git-subdir` source that points at `plugins/playbook` in this repository.

- The `downstream` job pins its `ref` to the release tag `vX.Y.Z` and sets `metadata.version` once the release is published.
- Claude Code copies only that folder at the tag and updates an installed plugin when the `version` at the new pin is higher.
- Until the first release that runs `downstream`, the entry has no `ref` and follows the default branch.
- There is no separate plugin release. The `playbook-plugin-v` tags made before 0.34.0 stay as history. The `payload-guard` check keeps the folder to markdown and the manifest.

## The release workflow

`.github/workflows/release.yml` releases playbook ([ADR 0039](../adr/0039-release-workflow.md)). A maintainer starts it from the Actions tab or `gh`:

```bash
gh workflow run release.yml -f bump=patch          # or minor, or major
gh workflow run release.yml -f version=0.35.0
gh workflow run release.yml -f bump=minor -f dry_run=true
```

`plan` runs `scripts/release-plan.sh`. It reads the versions on `main` and the `v` tags and picks where the run starts:

- `prepare` (the files are below the version): `scripts/release-prepare.sh` runs `scripts/bump-version.sh` and adds the `## playbook X.Y.Z` section to `CHANGELOG.md`. `scripts/release-gh.sh pr` opens the pull request `chore(release): playbook X.Y.Z` on `release/vX.Y.Z` with one commit that GitHub signs and turns on auto-merge. The job waits for the merge queue to land it.
- `tag` (`main` carries the version, the tag is missing): the job creates the lightweight tag `vX.Y.Z` on the release commit with the igorjs app token, which the "Protect release tags" ruleset lets through.
- `publish` (the tag exists): the run builds and publishes. If the release is already published, it only checks it.

### The macOS gate

Before the tag, `macos` calls `rust-ci.yml` to build and test the release commit on macOS. Pull requests and the merge queue test on Linux only, so this is the one macOS check every release must pass.

- It tests the release commit, or the tag when the run starts at `publish`. A dry run that starts at `prepare` tests the dispatch commit. `tag` and `draft` stop the run unless it passed on that same commit.
- The release pull request also builds and tests on macOS, so a macOS break stops the bump before `main`.

When the gate still fails on the release commit, land the fix and tag `vX.Y.Z` on the fixed commit by hand. Then run the workflow again with `version=X.Y.Z`: it starts at `publish` and tests the tag.

### After the tag

- `draft` creates a draft release with the notes and attaches `release-notes.json`.
- `build` compiles the five targets with `--locked` and attaches two assets per target: the `playbook` binary and `pbtop-X.Y.Z-<target>.tar.gz` with `pbtop` alone. Each build job first runs its binaries with `--version` (all but the cross built x86_64 macOS one) and attests the assets it built.
- `publish` checks every draft asset against that attestation, writes `SHA256SUMS` over every asset, attests it and the notes, and makes the release public and the latest.
- `verify` downloads the published release, checks the sums and the attestations of a run from `main`, and runs `--version` on the x86_64 Linux binaries.

No release carries the `playbook-telemetry` archive, which [ADR 0041](../adr/0041-retire-playbook-telemetry.md) retires.

Every step can run again: an existing pull request, tag or release is reused, a tag is never moved, and a published release is never rebuilt or uploaded again. After a failed run, run the workflow again with `version=X.Y.Z`. When GitHub refuses to create the tag, the run summary prints the signed `git tag` command to run by hand, and the next run with `version=X.Y.Z` goes on from the tag.

### Downstream: tap and marketplace

After `verify`, `downstream` moves the tap and the marketplace to the release ([ADR 0039](../adr/0039-release-workflow.md) decision 9).

- The tap's `.github/scripts/bump-formula.sh` runs for `playbook` and `pbtop` in a `formulas` job with no secrets and reads the release's `SHA256SUMS`. The marketplace `ref` becomes `vX.Y.Z` and `metadata.version` becomes `X.Y.Z`.
- Each change goes to a `release/vX.Y.Z` pull request titled `chore(release): playbook X.Y.Z`, made with `scripts/release-gh.sh pr` and the app token, with auto-merge on. When it merges, a workflow in that repository tags the merge commit `vX.Y.Z` and publishes a release.
- The job opens both pull requests first, then waits up to 30 minutes for both merges, and the run summary links them.
- When a repository has not published its release five minutes after its merge, the job tags and releases it through the API. It does this only when the merge commit changes nothing but the formulas or the marketplace file.
- When something here fails, the playbook release stays published, the job fails, and the summary prints the commands to finish it. Running the workflow again with `version=X.Y.Z` goes on from where it stopped.
- When `BOT_APP_CLIENT_ID` or `BOT_APP_PRIVATE_KEY` is not set, the job skips this step with a notice. The summary says to move the tap and the marketplace by hand or run again once the secrets are set.

When `downstream` has moved the tap (its `moved` output, not set when the app secrets are missing), `verify-homebrew-linux` runs `brew tap` and `brew install` for `playbook` and `pbtop` on an Ubuntu runner. It checks that both report the release version, then runs `playbook doctor check` in an empty `HOME`. It blocks no other job. A red result fails the run, and running again with `version=X.Y.Z` starts it again. A dry run skips it, because nothing is published to install.

### Nix flake

After `verify`, the `nix` job moves the Nix flake (root `flake.nix`):

1. It checks `SHA256SUMS` and the four Nix assets against their attestation.
2. `scripts/render-flake.sh` rewrites the `releases` block (the version and one SRI hash per system).
3. The job opens `release/nix-vX.Y.Z` titled `chore(release): nix flake X.Y.Z` with the app token, auto-merge on, and waits up to 30 minutes for the merge.

- `scripts/release-notes.sh` leaves that commit out of the next notes.
- The job skips when the release is not the latest or `flake.nix` is already ahead, so a backport does not move the flake back. Without the app secrets it warns and passes.
- If `main` moved between the checkout and the commit, `release-gh.sh` fails with "Run again": run the workflow with `version=X.Y.Z`.
- A dry run renders from a stand-in `SHA256SUMS`, shows the diff and opens nothing.
- The tag `vX.Y.Z` carries the flake of the release before it, so users pin `main` or the merge commit of the flake pull request.
- To roll the flake back, open a pull request that reverts the flake change. The older hashes stay valid because release assets do not change.

Then `verify-nix-linux` and `verify-nix-macos` run `nix run github:pragmatic-engineer/playbook/<commit>#playbook -- --version` at the commit that carries the flake and compare it with the version. The Linux job also builds a copy with a wrong hash, which must fail. Both are leaf jobs, skipped in a dry run and when `nix` moved nothing.

### AUR

After `verify`, the `aur` job publishes the Arch package `playbook-bin` to the AUR.

- `packaging/aur/PKGBUILD.tmpl` is the template. `scripts/render-pkgbuild.sh` fills it with the version and the two Linux `playbook-X.Y.Z-<target>` lines of `SHA256SUMS`.
- Before it renders, the job checks `SHA256SUMS` and the two Linux binaries against their attestation, so the AUR carries the attested hashes.
- It keeps the `pkgrel` and `epoch` of the AUR when the `pkgver` is the same, so a manual fix is not overwritten. A new `pkgver` gives `pkgrel=1`.
- `scripts/aur-verify.sh` runs in the `archlinux:base-devel` container (pinned by digest in `release.yml`). It makes `.SRCINFO` with `makepkg --printsrcinfo` as a non-root user, builds with `makepkg -f`, installs with `pacman -U`, compares `playbook --version` with the release, and downloads the aarch64 asset to check its sum. A failure stops the job before anything is pushed.
- Then the job commits `playbook-bin: X.Y.Z` and pushes `PKGBUILD` and `.SRCINFO` to `ssh://aur@aur.archlinux.org/playbook-bin.git` on `master`, the only branch the AUR takes. It builds the live repository from `https://aur.archlinux.org/playbook-bin.git`, with up to five tries for the AUR to catch up. No change means no commit: a run with the same version pushes nothing.
- The SSH host key is pinned in `packaging/aur/known_hosts` with `StrictHostKeyChecking=yes`. The fingerprints are on https://aur.archlinux.org/ and `scripts/aur-verify.test.sh` checks them. A changed host key fails the job: refresh the file from that page.
- The key file is written to `RUNNER_TEMP` with mode 600 and removed in an `always()` step.
- The job is a leaf. A backport or an older release never rolls the AUR back. A dry run builds the newest published release from its real assets, shows the diff and pushes nothing.
- Without the secret `AUR_SSH_PRIVATE_KEY` in the `aur` environment, the job builds and checks the package, skips the push with a notice and passes.

One-time setup, by a maintainer. The workflow cannot do these:

1. Create a dedicated bot account on https://aur.archlinux.org/. An AUR SSH key reaches every package of its account, so do not use a personal account that owns other packages.
2. Check that `playbook-bin` is free (https://aur.archlinux.org/packages/playbook-bin). The first push creates it.
3. Make a key pair with `ssh-keygen -t ed25519 -f aur_key -C playbook-release`. Add `aur_key.pub` to the bot account under My Account, SSH Public Key. Store the private key as the secret `AUR_SSH_PRIVATE_KEY` of a new GitHub environment `aur` that deploys from `main` only. Delete the local key files.
4. Add a personal AUR account as co-maintainer of `playbook-bin`, so a rollback does not depend on CI.
5. Rotate by adding a new public key to the bot account, replacing the secret, then removing the old public key.

To fix a bad push, clone `ssh://aur@aur.archlinux.org/playbook-bin.git`, commit a correction and push to `master`.

A fix at the same version raises `pkgrel`. A real rollback to an older `pkgver` looks like a downgrade to pacman, so raise `epoch` in the same commit (the template keeps an `epoch` that is there).

A run with `version=` of an older release does not apply, because the job only publishes the latest release. Orphaning or deleting the package goes through the AUR web page.

### apt and dnf repository

After `verify`, two leaf jobs publish a signed repository to GitHub Pages ([ADR 0052](../adr/0052-signed-apt-and-dnf-repository.md)). A failure turns the run red and blocks no other channel. Run `gh workflow run release.yml -f version=X.Y.Z` to try again.

- `package-repo-sign` runs `scripts/package-repo-fetch.sh`, which takes the newest five releases that list packages and checks the attestation of `SHA256SUMS` and of every package. `scripts/package-repo.sh` rebuilds both indexes from them and signs them. `scripts/package-repo-verify.sh` then installs from the built folder in Debian, Ubuntu and Fedora containers, checks that a tampered index is refused and, when two versions exist, that an upgrade keeps a seeded file under `~/.config/playbook`. This job holds the key and has `contents: read` only.
- `package-repo-deploy` holds the Pages permissions and no key. After the deploy it reads the public URL, retries for up to five minutes, and checks both signatures with the published key.
- A missing key fails the run on purpose. A dry run signs with a throwaway key and deploys nothing.
- The signing subkey is RSA 4096 and lasts three years. The build warns under 90 days and fails under 30.
- To yank a version, add it to `packaging/apt/yanked.txt` in a pull request and run the release workflow again. A rerun alone does not remove a bad release. Pages also keeps the earlier deployment for a manual redeploy.
- If the key leaks, revoke the subkey, publish a new key and an advisory, replace both secrets and rebuild. Clients fetch the new `playbook.gpg`.

One time setup for the maintainer. The jobs cannot do these:

1. Generate an RSA 4096 primary key offline, with a signing subkey that expires in three years. Keep the primary on an offline token or encrypted media, with a second copy.
2. Export the subkey only: `gpg --armor --export-secret-subkeys <fingerprint>!`.
3. Create environment `package-repo` limited to `main`, with secrets `PACKAGE_SIGNING_KEY` (the exported subkey) and `PACKAGE_SIGNING_PASSPHRASE`.
4. In Settings, Pages, set the source to GitHub Actions, and create environment `github-pages`.
5. Publish the fingerprint in the release notes and this guide. If you own a custom domain, point it at Pages and change the URL in the install guide before the first release.
6. Run `gh workflow run release.yml -f version=<latest> -f dry_run=true` to prove the build, then without `dry_run` to publish.

The first real run decides the Pages egress hosts and whether block mode covers container traffic, which is why both jobs use audit mode for now.

### Scoop

After `verify`, the `scoop` job moves the Windows scoop bucket, the repository `pragmatic-engineer/scoop-bucket` (ADR 0053, Windows is experimental).

- It checks `SHA256SUMS` and the Windows `.exe` against their attestation.
- `scripts/render-scoop.sh` fills `packaging/scoop/playbook.json.tmpl` with the version and the hash of that `.exe`.
- The job opens `release/vX.Y.Z` titled `chore(release): playbook X.Y.Z` in the bucket with the app token, auto-merge on, and waits up to 30 minutes for the merge.
- It skips when the release is not the latest or the bucket is already ahead, so a backport does not move the bucket back.
- Without the app secrets, or when the app is not installed on the bucket repository, it warns and passes.
- Token custody is the same as for the tap: `BOT_APP_CLIENT_ID` and `BOT_APP_PRIVATE_KEY`, with the app installed on the bucket repository.
- A dry run renders from a stand-in `SHA256SUMS` into the run summary and opens nothing.

Then `verify-scoop` installs scoop on a clean Windows runner from a pinned installer script, installs the rendered manifest from a file, checks `playbook --version`, removes it, and installs again from the bucket (with retries) once the pull request has merged. It is a leaf job with no secret, skipped in a dry run.

To roll back, revert the bucket pull request. Users who already installed keep their version until `scoop update`, and scoop treats a lower version as older, so ship a higher patch release to replace a bad one.

### winget

After `verify`, the `winget` job on a Windows runner opens a pull request to `microsoft/winget-pkgs` for the `.exe` (ADR 0053, Windows is experimental). The package identifier is `PragmaticEngineer.Playbook`.

- It checks `SHA256SUMS` and the `.exe` against their attestation.
- `wingetcreate update` writes the manifest. The job then compares the installer hash in it with the attested hash and only then submits. It never passes the token on the command line: the token reaches the tool as `WINGET_CREATE_GITHUB_TOKEN`.
- `wingetcreate` is a pinned release with its SHA256 kept in the workflow.
- It skips when the release is not the latest or the version already has a pull request, so a rerun opens no second one.
- It warns and passes while `WINGET_PAT` is not set in environment `winget`.
- Token custody: a classic `public_repo` token of a dedicated bot account that owns the fork of `winget-pkgs`, stored only as `WINGET_PAT` of environment `winget`, limited to `main`.
- The first manifest is made with `wingetcreate new` and landed by hand, because `update` needs an accepted package. Until then the job fails at `wingetcreate update`.
- A dry run prints the command and opens nothing.

Microsoft moderators decide the pull request, and validation can take days. Closing the pull request before it merges undoes it. After the merge, a bad version leaves through a pull request to `winget-pkgs`, so treat winget as best effort. The binary has no Authenticode signature, so validation may flag it.

### Dry run

A dry run plans, shows the release pull request diff in the run summary, and builds the five targets from `main`. It shows the tap and marketplace diffs (placeholder checksums for the formulas) and checks the app tokens with read-only calls. It opens, tags, uploads and publishes nothing.

### Release notes

`scripts/release-notes.sh` builds the body of the release from the Conventional Commits since the previous `v` tag that touch the playbook paths, the plugin included (ADR 0037).

The sections are Breaking changes, Features, Fixes, Performance and Other changes, in that order. An empty one is left out, and so is the release commit itself. The same notes go into `release-notes.json`, which `SHA256SUMS` lists and the workflow attests.

No check fails a release for a breaking change: the notes list the commits marked breaking first. Releases no longer carry `breaking-changes.json` (ADR 0047). A 0.34 binary, the last that read it, treats the missing asset as a release with no breaking changes.

## Credentials

The workflow uses two tokens:

- `GITHUB_TOKEN`: read, build the draft and publish.
- igorjs GitHub App token: the release pull request, the tag and the `downstream` pull requests.

The app secrets and their guards:

- `BOT_APP_CLIENT_ID` and `BOT_APP_PRIVATE_KEY` are organization secrets that the playbook, tap and marketplace repositories can read.
- Each step asks the app only for the permissions it needs. `downstream` mints one token per repository.
- The workflow runs only from `workflow_dispatch`, anything but a dry run only from `main`, and `downstream` only from `main`.
- Any workflow in those three repositories can read the key. The guards are the rulesets and the checks in each workflow: the downstream release workflows act only on a merged pull request from the app on a `release/v*` branch.
- An organization admin rotates the key and updates both secrets. Pull requests made with the app token run CI, which pull requests made with `GITHUB_TOKEN` do not.
- The app is on the bypass list of the "Protect release tags" ruleset, so it can create `v*` tags. The tag is a lightweight ref on the squash commit of the merge queue, which GitHub signs.

## Rolling back a release

A published release is not deleted and its tag is not moved, because installs and the tap may already point at them. To pull a bad release:

1. Mark it as a pre-release, so it stops being the latest: `gh release edit vX.Y.Z --prerelease --latest=false`. Then mark the previous release as the latest with `gh release edit vA.B.C --latest`. `install.sh` and `playbook update` read the latest release.
2. Add a warning at the top of its notes, saying what is wrong and which version to use. `--notes-file` replaces the whole body, so start from the current notes: `gh release view vX.Y.Z --json body --jq .body > notes.md`, add the warning at the top of `notes.md`, then `gh release edit vX.Y.Z --notes-file notes.md`.
3. Move the plugin back: open a pull request in `pragmatic-engineer/marketplace` that sets the `ref` of the `playbook` entry to `vA.B.C` and `metadata.version` to `A.B.C`. New installs then get the previous plugin. Claude Code never installs a lower plugin version over an installed one, so users who already updated keep the bad plugin until the next patch.
4. Move the formulas back: in `pragmatic-engineer/homebrew-tap`, run `SOURCE_REPO=playbook TAG_PREFIX=v .github/scripts/bump-formula.sh A.B.C Formula/<formula>.rb` for `playbook` and `pbtop`, and open a pull request with the result.
   - The tap's scheduled check never moves a formula back to a lower version, and it skips a pre-release.
   - Before 0.35.0 the tap had no `pbtop` formula: for such an A.B.C, move only `playbook` this way and remove `Formula/pbtop.rb` by hand.
   - The retired `playbook-telemetry` formula stays on `telemetry-v0.2.0`. When `pbtop.rb` goes, also remove its `disable!` line, since it names `pbtop` as the replacement.
5. Ship the fix as the next patch release, which moves the tap and the marketplace forward again.

## Internal tool crates

`playbook-telemetry` and `playbook-code` are workspace members under `crates/`. `playbook` depends on them by path, so there is one `Cargo.lock`.

`playbook-code` is a library of the `playbook` binary and has no release of its own. `playbook-telemetry` uses the workspace version, and its `pbtop` binary ships in the `vX.Y.Z` release (ADR 0038, ADR 0041). The `telemetry-v` tags before 0.34.0 stay as history.

Release assets by tool:

- playbook: `playbook-<version>-<target>`, the raw binary.
- pbtop: `pbtop-<version>-<target>.tar.gz` holding `pbtop` alone.
- pbdoc: `pbdoc-<version>-<target>.tar.gz` holding `pbdoc` and `NOTICE-pbdoc`, for the four macOS and Linux targets.
- Debian, Ubuntu: `playbook_<version>_amd64.deb` and `playbook_<version>_arm64.deb`.
- Fedora, RHEL: `playbook-<version>.x86_64.rpm` and `playbook-<version>.aarch64.rpm`.

The Homebrew tap maps each formula to its asset.

### Linux packages

Each package holds `playbook`, the `pb` link and `pbtop` in `/usr/bin`, and the license. It has no dependencies, no config file and no install or remove script, so an upgrade or a removal never touches user config.

- `scripts/nfpm.yaml` describes it. `scripts/package-linux.sh` builds it with a pinned nfpm (version and SHA-256 are in the script; bump both together).
- The two Linux build legs build the packages from their own static binaries, install them in Debian, Ubuntu and Fedora containers with `scripts/package-smoke.sh`, and attest them with the other assets. A bad package stops the release before the draft is published.
- The `publish` job puts them in `SHA256SUMS` and expects 18 assets.
- After `verify`, the `verify-packages` job downloads the x86_64 pair from the published release, checks the sums and the attestation, and runs the same container test. It blocks no other channel. A red result fails the run, and a run with the same version starts it again.
- A dry run builds and tests the packages and publishes nothing. The container images are pinned by digest in `scripts/package-smoke.sh`.

### pbdoc

Only `install.sh` and Homebrew ship `pbdoc`, the document converter. The `.deb` and `.rpm` packages, `cargo binstall`, mise and the nix flake install `playbook` without it, and `playbook doc` then reports that no converter is installed. This is a known limit.

Release checklist for the release that adds `pbdoc`: the tap pull request that adds `resource "pbdoc"` to `Formula/playbook.rb` (homebrew-tap #53) merges first, and the release workflow and the installer changes ship together. The `downstream` job fails when the tap formula has no `resource "pbdoc"`, so a tag cut before that pull request merges cannot ship silently.

### cargo binstall and mise

`cargo binstall` and mise install the raw `playbook-<version>-<target>` binary and depend on that name.

- `[package.metadata.binstall]` in `crates/cli/Cargo.toml` holds the URL template. mise needs nothing, since it matches the operating system and architecture words in the asset name.
- `scripts/asset-names.test.sh` expands the template for every target of the build matrix and fails when it differs from the name `release.yml` gives the asset.
- After `verify`, the `verify-installers` job runs `scripts/verify-installers.sh`. Pinned cargo-binstall and mise install the published release in an empty `HOME`, and `cargo binstall` may not compile. The installed file must report the version, match its `SHA256SUMS` line and verify against its attestation.
- It blocks no other channel. A red result fails the run, and a run with the same version starts it again.
- A dry run installs the last released version instead, which checks the job and its egress list without publishing anything.
- The crates.io name `playbook` belongs to another project, so the documented command uses `--git` and the metadata disables the quick-install strategy.
