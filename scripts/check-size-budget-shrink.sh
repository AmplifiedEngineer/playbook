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

base_rows="$(git show "$base:$list" | rows)"
head_rows="$(rows <"$list")"

status=0
while IFS=$'\t' read -r key nums; do
  [ -n "$key" ] || continue
  old="$(printf '%s\n' "$base_rows" | awk -F'\t' -v k="$key" '$1 == k { print $2; exit }')"
  if ! printf '%s\n' "$base_rows" | awk -F'\t' -v k="$key" '$1 == k { found = 1 } END { exit !found }'; then
    problem "$list: a row was added: $key; the lists only shrink"
    status=1
    continue
  fi
  i=0
  for n in $nums; do
    i=$((i + 1))
    o="$(printf '%s\n' "$old" | awk -v i="$i" '{ print $i }')"
    if [ -n "$o" ] && [ "$n" -gt "$o" ]; then
      problem "$list: $key: number $i went up from $o to $n; the lists only shrink"
      status=1
    fi
  done
done <<EOF
$head_rows
EOF

exit "$status"
