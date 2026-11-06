#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# check-port-diff.sh: checks that a change which moves code from one SQLite
# crate to another touches nothing but imports and type paths.
#
# Usage: check-port-diff.sh [base-ref]    (default: origin/main)
#
# Over `git diff -U0 base...HEAD` a changed line of a Rust file passes when it
# is a `use` line, or when the removed and the added lines are the same set once
# `rusqlite` and `playbook_store` are read as one name. A file under `tests/`
# may change `use` lines only. A Cargo.toml may change only the `rusqlite` and
# `playbook-store` dependency lines. Cargo.lock is ignored.
set -euo pipefail

base="${1:-origin/main}"
cd "$(git rev-parse --show-toplevel)"

git diff -U0 --no-color --no-renames "$base"...HEAD |
  awk '
    function flush(   key, bad) {
      if (file == "") return
      if (file == "Cargo.lock") return
      if (file ~ /(^|\/)Cargo\.toml$/) {
        for (i = 1; i <= nline; i++)
          if (substr(line[i], 2) !~ /^(rusqlite|playbook-store)[ \t]*=/) report(file, line[i])
        return
      }
      if (file !~ /\.rs$/) {
        for (i = 1; i <= nline; i++) report(file, line[i])
        return
      }
      is_test = (file ~ /(^|\/)tests\//)
      delete count
      for (i = 1; i <= nline; i++) {
        text = substr(line[i], 2)
        if (text ~ /^[ \t]*(pub(\([a-z]+\))?[ \t]+)?use[ \t]/) continue
        if (is_test) { report(file, line[i]); continue }
        norm = text
        gsub(/rusqlite|playbook_store/, "X", norm)
        count[norm] += (substr(line[i], 1, 1) == "+") ? 1 : -1
      }
      for (key in count)
        if (count[key] != 0) report(file, "(unbalanced) " key)
    }
    function report(f, l) { print f ": " l > "/dev/stderr"; failed = 1 }
    /^diff --git / { flush(); file = $4; sub(/^b\//, "", file); nline = 0; next }
    /^(\+\+\+|---) / { next }
    /^[+-]/ { line[++nline] = $0 }
    END { flush(); exit failed }
  ' || {
  echo "check-port-diff: the lines above change more than imports and type paths" >&2
  exit 1
}
echo "check-port-diff: only imports and type paths changed"
