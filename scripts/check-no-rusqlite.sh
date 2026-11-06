#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# check-no-rusqlite.sh: fails when the word `rusqlite` appears in a Rust file
# or a Cargo.toml outside crates/store, the one crate allowed to name the engine.
#
# Usage: check-no-rusqlite.sh [root]    (default: the repository root)
set -euo pipefail

ROOT="${1:-$(dirname "$0")/..}"
cd "$ROOT"

# -w keeps a longer identifier such as `rusqlite_free_db` from matching.
hits="$(grep -rnw rusqlite . \
  --include='*.rs' --include='Cargo.toml' \
  --exclude-dir=target --exclude-dir=.git --exclude-dir=.worktrees \
  --exclude-dir=store | sed 's|^\./||' || true)"

# --exclude-dir=store also skips any other directory named store, so the store
# crate itself is only exempt by that exact path: re-scan the others by path.
other_stores="$(find . -type d -name store -not -path './crates/store' \
  -not -path './target/*' -not -path './.git/*' -not -path './.worktrees/*' \
  -exec grep -rnw rusqlite {} --include='*.rs' --include='Cargo.toml' \; | sed 's|^\./||' || true)"

violations="$(printf '%s\n%s\n' "$hits" "$other_stores" | sed '/^$/d')"
if [ -n "$violations" ]; then
  echo "rusqlite may only be named inside crates/store. Use playbook_store instead:" >&2
  printf '%s\n' "$violations" | cut -d: -f1,2 >&2
  exit 1
fi
