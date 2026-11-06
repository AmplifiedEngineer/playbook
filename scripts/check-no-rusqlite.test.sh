#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# check-no-rusqlite.test.sh: offline tests for check-no-rusqlite.sh on temp trees.
#
# Usage: check-no-rusqlite.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."
SCRIPT="$PWD/scripts/check-no-rusqlite.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() { echo "FAIL $*" >&2; exit 1; }

# tree <name> <file> <content>: a temp tree with a clean store file and one more file.
tree() {
  local dir="$TMP/$1"
  mkdir -p "$dir/crates/store/src" "$dir/crates/cli/src" "$dir/crates/cli/tests"
  printf 'pub use rusqlite::Connection;\n' >"$dir/crates/store/src/lib.rs"
  printf '[dependencies]\nrusqlite = "1"\n' >"$dir/crates/store/Cargo.toml"
  printf '%s\n' "$3" >"$dir/$2"
  echo "$dir"
}

# expect <pass|fail> <label> <dir> [text]: runs the script and checks the result.
expect() {
  local rc=0 out
  out="$(bash "$SCRIPT" "$3" 2>&1)" || rc=$?
  if [ "$1" = pass ] && [ "$rc" -ne 0 ]; then fail "$2: expected a pass, got $rc: $out"; fi
  if [ "$1" = fail ] && [ "$rc" -eq 0 ]; then fail "$2: expected a failure"; fi
  if [ -n "${4:-}" ] && ! grep -qF "$4" <<<"$out"; then fail "$2: output lacks $4: $out"; fi
}

expect pass "a clean tree with rusqlite only in the store" \
  "$(tree clean crates/cli/src/lib.rs 'use playbook_store::Connection;')"
expect pass "an identifier that only contains the word" \
  "$(tree longer crates/cli/src/lib.rs 'fn rusqlite_free_db() {}')"
expect fail "a use line outside the store" \
  "$(tree use_line crates/cli/src/lib.rs 'use rusqlite::params;')" crates/cli/src/lib.rs:1
expect fail "a fully qualified path" \
  "$(tree qualified crates/cli/src/lib.rs 'fn open() -> rusqlite::Connection { todo!() }')" crates/cli/src/lib.rs:1
expect fail "an integration test" \
  "$(tree in_tests crates/cli/tests/t.rs 'use rusqlite::Connection;')" crates/cli/tests/t.rs:1
expect fail "a Cargo.toml dependency line" \
  "$(tree dep_line crates/cli/Cargo.toml $'[dependencies]\nrusqlite = "1"')" crates/cli/Cargo.toml:2

echo "ok"
