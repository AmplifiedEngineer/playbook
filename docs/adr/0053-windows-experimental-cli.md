# ADR-0053: Windows Is an Experimental CLI

- **Status:** Accepted. Amended 2026-11-06: the scoop bucket and the winget job were removed. The Windows route is the raw `.exe` release asset, and CI keeps the `windows-smoke` job.
- **Date created:** 2026-10-22
- **Date modified:** 2026-11-04
- **Relates to:** [ADR 0007](0007-rust-binary-for-hooks-and-launcher-quality.md), which listed Windows as a deliberate scope addition with its own go or no-go.

## Context

Issue #1149 asks for winget and scoop packages. Before packaging, the Windows scope has to be stated, because a package promises more than a bare release asset does. ADR 0007 says in one place that Windows is supported and in another that Windows is a scope addition with a PowerShell shim planned for a later release. No Windows user is on record in the repo.

What exists on `main` at v0.39.0 (checked 2026-10-22):

- The release builds `playbook-X.Y.Z-x86_64-pc-windows-msvc.exe` on `windows-latest` and runs it with `--version` before upload.
- CI only compiles Windows (`cargo check`). It runs no test.
- `playbook update` is refused on Windows, with manual steps.
- `install.sh` is Bash only.
- `setup` writes shell wiring for zsh, bash, fish and `.profile`. It has no PowerShell path.
- The `pb` link is not created on non-unix systems.
- The `rm` guard splits paths on `/` and does not read backslash paths.
- `pbdoc` and the `doc` command do not exist on Windows.
- `pbtop` ships as a tar.gz on Windows, which no Windows package tool installs cleanly.

## Decision

Windows is an **experimental CLI**. The maintainer accepted this on 2026-10-22 (option D1(b) of the winget and scoop plan).

Supported, and checked by a CI smoke test on `windows-latest`:

- `playbook.exe` starts and reports its version.
- `playbook doctor check` runs and reports.
- Hooks read their input and exit cleanly when called by name.

Not supported, and documented as such:

- PATH wiring by `setup`, the `pb` short name and PowerShell profile wiring.
- `playbook update`. Use the package manager instead.
- `pbdoc` and the `doc` command.
- `pbtop` on Windows.
- Backslash path parsing in the `rm` guard.
- ARM64 Windows. No build target exists.
- Whether Claude Code on Windows resolves `playbook hook <name>` from `settings.json`. The smoke test proves only the binary side.

Full parity (a PowerShell shim, backslash parsing, `update` on Windows) needs its own plan and its own ADR. This ADR does not promise it.

Distribution: scoop, from a bucket in its own repository, labelled experimental. winget is not started. The maintainer judged it premature: it has a human moderator, an unresolved signing question and a classic token requirement. Revisit it after scoop has shipped one real release.

### Amendment, 2026-11-04: winget starts

The maintainer delegated the winget questions and took the recommended answer for each:

- Build the `winget` release job now (S4 of the plan for #1149), without waiting for a real scoop release. It warns and passes while its secret is missing, so an unconfigured channel never blocks a release.
- Tool: `wingetcreate update`, run on a `windows-latest` job, with the token passed by environment variable and never on the command line.
- Identifier: `PragmaticEngineer.Playbook`.
- Signing: "no unsigned artifacts" means checksum plus build attestation for the `.exe`. No Authenticode certificate is bought. If winget validation rejects the unsigned file, the job stays off and this record is revisited.
- Account: a dedicated bot account holds the fork and a classic `public_repo` token in environment `winget`, limited to `main`. The maintainer creates both, and submits and lands the first manifest by hand before the job can open an update.

## Alternatives considered

- **A. Not supported, no packaging.** Honest and free, but it leaves Windows users with a raw asset and no update path, and the issue stays open.
- **B. Experimental CLI (chosen).** A small, reversible channel with the limits written down.
- **C. Full parity first.** A large product task that would block a small packaging task. Kept as a later option.

## Consequences

- The install guide gets a Windows section that says "experimental" and lists the gaps above.
- Windows failures in the smoke test steps 1 and 2 block a pull request. Later steps report only, until a follow-up moves them into scope.
- A Windows bug report is triaged against this list. Items listed as unsupported are feature requests, not bugs.
- Moving to full parity, or dropping Windows, replaces this ADR.
