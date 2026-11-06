#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# check-port-diff.test.sh: offline tests for check-port-diff.sh on temp repos.
#
# Usage: check-port-diff.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."
SCRIPT="$PWD/scripts/check-port-diff.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

export GIT_CONFIG_GLOBAL="$TMP/gitconfig"
export GIT_CONFIG_SYSTEM=/dev/null
git config --global user.name tester
git config --global user.email tester@example.com
git config --global commit.gpgsign false
git config --global init.defaultBranch main

fail() { echo "FAIL $*" >&2; exit 1; }

# fixture <name>: a repo with one source file and one test file on branch main.
fixture() {
  local dir="$TMP/$1"
  mkdir -p "$dir/src" "$dir/tests"
  cd "$dir"
  git init -q
  printf 'use rusqlite::Connection;\n\nfn open() -> rusqlite::Connection {\n    Connection::open("x").unwrap()\n}\n' >src/lib.rs
  printf 'use rusqlite::Connection;\n\n#[test]\nfn t() {\n    let _ = Connection::open_in_memory();\n}\n' >tests/t.rs
  printf '[dependencies]\nrusqlite = "=1"\nserde = "1"\n' >Cargo.toml
  git add -A
  git commit -qm base
  git checkout -qb port
}

# expect <pass|fail> <label>: commits the working tree and runs the script.
expect() {
  git add -A
  git commit -qm change
  local rc=0
  bash "$SCRIPT" main >/dev/null 2>&1 || rc=$?
  if [ "$1" = pass ] && [ "$rc" -ne 0 ]; then fail "$2: expected a pass, got $rc"; fi
  if [ "$1" = fail ] && [ "$rc" -eq 0 ]; then fail "$2: expected a failure"; fi
}

fixture import_only
sed -i.bak 's/rusqlite/playbook_store/g' src/lib.rs && rm src/lib.rs.bak
sed -i.bak 's/^use rusqlite/use playbook_store/' tests/t.rs && rm tests/t.rs.bak
sed -i.bak 's/^rusqlite = .*/playbook-store = { path = "..\/store" }/' Cargo.toml && rm Cargo.toml.bak
expect pass "an import and type path change"

fixture logic_change
sed -i.bak 's/unwrap()/expect("open")/' src/lib.rs && rm src/lib.rs.bak
expect fail "a non-import change"

fixture test_body_change
sed -i.bak 's/open_in_memory/open/' tests/t.rs && rm tests/t.rs.bak
expect fail "a test line outside use lines"

fixture other_dependency
sed -i.bak 's/serde = "1"/serde = "2"/' Cargo.toml && rm Cargo.toml.bak
expect fail "a dependency other than the two engines"

echo "check-port-diff.test.sh: all passed"
