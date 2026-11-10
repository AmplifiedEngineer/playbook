#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# check-size-budget-shrink.sh: fails a change that grows a plugin check list.
#
# Usage: check-size-budget-shrink.sh <list file>
#
# A row is every line that is not blank and not a comment. Its key is the words
# before the first number, and its numbers are the ceilings after the key. The
# change fails when a row is added (a key the base lacks) or a number is raised.
# A removed row and a lowered number pass. A list that is new in the change has
# no base and passes.
#
# In CI (env CI set) the base is HEAD^1, the main tip of a pull_request merge
# commit, so the checkout needs fetch-depth: 2. Elsewhere the base is the merge
# base with origin/main.
set -euo pipefail

problem() { echo "check-size-budget-shrink: $*" >&2; }

if [ "$#" -ne 1 ]; then
  problem "usage: check-size-budget-shrink.sh <list file>"
  exit 2
fi
list="$1"

cd "$(git rev-parse --show-toplevel)"

if [ -n "${CI:-}" ]; then
  if ! base="$(git rev-parse -q --verify 'HEAD^1')"; then
    problem "HEAD^1 does not resolve; the checkout needs fetch-depth: 2"
    exit 1
  fi
else
  if ! git rev-parse -q --verify refs/remotes/origin/main >/dev/null; then
    if ! git fetch -q origin main 2>/dev/null; then
      echo "check-size-budget-shrink: skipped, could not fetch origin main; CI is the guard"
      exit 0
    fi
  fi
  if ! base="$(git merge-base origin/main HEAD 2>/dev/null)"; then
    problem "no merge base with origin/main; run 'git fetch --unshallow' and retry"
    exit 1
  fi
fi

if ! git cat-file -e "$base:$list" 2>/dev/null; then
  exit 0
fi

# rows <text>: one line per row, "key<TAB>numbers".
rows() {
  awk '
    /^[[:space:]]*(#|$)/ { next }
    {
      key = ""; nums = ""
      for (i = 1; i <= NF; i++) {
        if ($i ~ /^[0-9]+$/) { nums = nums (nums == "" ? "" : " ") $i } else if (nums == "") { key = key (key == "" ? "" : " ") $i }
      }
      print key "\t" nums
    }'
}

base_file="$(mktemp)"
head_file="$(mktemp)"
trap 'rm -f "$base_file" "$head_file"' EXIT
git show "$base:$list" | rows >"$base_file"
rows <"$list" >"$head_file"

# A row whose key is new passes only as a move: a base row of the same kind
# that is gone from the head and has numbers at least as high. A rename of a
# file moves its rows this way. A row without numbers is never a move.
awk -F'\t' -v list="$list" '
  FNR == NR { base[$1] = $2; order[++n] = $1; next }
  { head[$1] = $2; hkeys[++m] = $1 }
  END {
    status = 0
    for (j = 1; j <= m; j++) {
      key = hkeys[j]; nums = head[key]
      if (key in base) {
        split(base[key], old, " "); cnt = split(nums, now, " ")
        for (i = 1; i <= cnt; i++) {
          if (old[i] != "" && now[i] + 0 > old[i] + 0) {
            printf "check-size-budget-shrink: %s: %s: number %d went up from %s to %s; the lists only shrink\n", list, key, i, old[i], now[i] > "/dev/stderr"
            status = 1
          }
        }
        continue
      }
      moved = 0
      if (nums != "") {
        split(key, kw, " ")
        for (i = 1; i <= n && !moved; i++) {
          bk = order[i]
          if ((bk in head) || (bk in used)) continue
          split(bk, bw, " ")
          if (bw[1] != kw[1]) continue
          split(base[bk], old, " "); cnt = split(nums, now, " ")
          ok = 1
          for (c = 1; c <= cnt; c++) if (now[c] + 0 > old[c] + 0) ok = 0
          if (ok) { used[bk] = 1; moved = 1 }
        }
      }
      if (!moved) {
        printf "check-size-budget-shrink: %s: a row was added: %s; the lists only shrink\n", list, key > "/dev/stderr"
        status = 1
      }
    }
    exit status
  }' "$base_file" "$head_file"
