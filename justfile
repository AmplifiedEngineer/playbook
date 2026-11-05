# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# Task runner for this repo: `just --list` shows the recipes.

set shell := ["bash", "-uc"]

# Where to read the live settings from. Override for testing:
#   just src=/tmp/fixture.json settings-shared
src := env_var_or_default("SRC", env_var("HOME") / ".claude/settings.json")
out := "seed/settings.shared.json"
bin := "./target/release/playbook"

# List the available recipes.
default:
    @just --list

# The permissions block of the seed is kept as it is: edit it by hand.
#
# Refresh the seed's hooks and status line from a live settings.json (dry run by default).
settings-shared write="0": build
    #!/usr/bin/env bash
    # Reads the maintainer's live settings.json, so it never runs in CI. Dry
    # run by default: anything that drifted in the live file lands in the
    # seed, so read the diff before you pass write=1.
    set -uo pipefail
    test -r "{{ src }}" || { echo "just: source settings not readable: {{ src }}; nothing regenerated" >&2; exit 1; }
    {{ bin }} settings gen "{{ src }}" "{{ out }}" > "{{ out }}.tmp" || exit 1
    if diff -u "{{ out }}" "{{ out }}.tmp" > /dev/null 2>&1; then
        rm -f "{{ out }}.tmp"
        echo "just: {{ out }} is already up to date with {{ src }}"
        exit 0
    fi
    echo "just: proposed changes to {{ out }} (from {{ src }}):"
    diff -u "{{ out }}" "{{ out }}.tmp" || true
    if [ "{{ write }}" != "1" ]; then
        rm -f "{{ out }}.tmp"
        echo
        echo "just: DRY RUN, nothing written. Review the diff above, then:"
        echo "just: only the .hooks block should differ; any other changed key is"
        echo "just: personal drift from {{ src }} and must not be committed."
        echo "just: a removed or emptied guard in .hooks is a regression, not drift."
        echo "just:   just settings-shared write=1"
        exit 0
    fi
    mv "{{ out }}.tmp" "{{ out }}"
    echo "just: regenerated {{ out }} from {{ src }}"
    {{ bin }} settings check "{{ out }}" .

# Build the release binary the other recipes call.
build:
    @cargo build --release --quiet

# Print the lean build settings and a private target folder for this worktree (eval the output).
dev-env:
    @scripts/dev-disk.sh env

# Remove idle target folders of the other worktrees of this repo (add --dry-run to preview).
dev-clean *args:
    @scripts/dev-disk.sh clean {{ args }}

# The checks CI runs, so a green local run means a green PR.
check: build
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo clippy --workspace --lib --bins -- -D warnings -D clippy::unwrap_used -D clippy::expect_used
    just test
    just test-shell
    git ls-files '*.sh' | xargs shellcheck --severity=warning
    @for f in $(git ls-files '*.sh'); do bash -n "$f"; done
    {{ bin }} ci --dir . --strict
    bash scripts/check-links.sh
    scripts/check-identifiers.sh
    scripts/check-plugin.sh payload
    scripts/check-plugin.sh version
    INTENTS_STRICT=1 scripts/check-intents.sh {{ bin }}

# Lint the GitHub workflows with zizmor, as the zizmor workflow does (brew install zizmor).
#
# Dev tooling only. Online audits run when GH_TOKEN is set, or gh is signed in.
zizmor:
    GH_TOKEN="${GH_TOKEN:-$(gh auth token 2>/dev/null || true)}" zizmor --persona auditor --min-severity medium --no-progress .

# cargo-nextest schedules tests across every test binary at once.
#
# Run the test suite with cargo-nextest, then the doc tests.
test *args:
    cargo nextest run --workspace --config-file .github/nextest.toml {{ args }}
    cargo test --workspace --doc

# Start 1000 collectors at once and check that one stays (done bar D2).
#
# Debug profile on purpose: the test-only stamp variable is read only by
# debug builds, so the release `bin` above would pass for the wrong reason.
stress-collector:
    cargo nextest run -p playbook --config-file .github/nextest.toml --test collector_daemon --run-ignored only -E 'test(d2_stress)'

# Time `playbook code` against rg on generated repos and write a JSON report with a verdict.
#
# Needs rg on PATH. BENCH_CODE_LSP=1 adds the language server arm (servers from PATH, nothing is installed). Set BENCH_CODE_SIZES (default 100,5000,50000) and BENCH_CODE_REPORT to change the run.
bench-code:
    cargo test --release -p playbook --test code_bench -- --ignored --nocapture

# Run every `*.test.sh` suite the way shell-ci does.
test-shell:
    #!/usr/bin/env bash
    set -uo pipefail
    export GIT_CONFIG_GLOBAL="$(mktemp)"
    git config --global user.email "ci@github.local"
    git config --global user.name "CI"
    total=0; fails=0
    while IFS= read -r t; do
      total=$((total+1))
      bash "$t" >/dev/null 2>&1 || { fails=$((fails+1)); echo "FAIL: $t"; }
    done < <(git ls-files '*.test.sh')
    echo "suites=$total failed=$fails"
    [ "$fails" -eq 0 ]
