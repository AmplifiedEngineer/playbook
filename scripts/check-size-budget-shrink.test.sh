#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# check-size-budget-shrink.test.sh: offline tests for check-size-budget-shrink.sh.
# Every repository is a temp one with a base and a head commit, so no test
# depends on the CI fetch depth.
#
# Usage: check-size-budget-shrink.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."

SCRIPT="$PWD/scripts/check-size-budget-shrink.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

export GIT_CONFIG_GLOBAL="$TMP/gitconfig"
export GIT_CONFIG_SYSTEM=/dev/null
git config --global user.name tester
git config --global user.email tester@example.com
git config --global commit.gpgsign false
git config --global init.defaultBranch main

LIST=crates/plugin-checks/tests/list.txt

pass=0
fail() { echo "FAIL $*" >&2; exit 1; }

# check <label> <base text> <head text> <exit code> [stderr text]
check() {
  local label="$1" base_text="$2" head_text="$3" want="$4" text="${5:-}" page="${6:-}"
  local repo="$TMP/repo-$pass"
  mkdir -p "$repo"
  (
    cd "$repo"
    git init -q
    mkdir -p "$(dirname "$LIST")"
    printf '%s' "$base_text" >"$LIST"
    git add -A
    git commit -q -m base
    printf '%s' "$head_text" >"$LIST"
    if [ -n "$page" ]; then
      mkdir -p docs
      printf '%s' "$page" >docs/renamed.md
    fi
    git add -A
    git commit -q -m head --allow-empty
  )
  local rc=0 err
  (cd "$repo" && CI=1 bash "$SCRIPT" "$LIST" <&- >"$TMP/out" 2>"$TMP/err") || rc=$?
  err="$(cat "$TMP/err")"
  [ "$rc" -eq "$want" ] || fail "$label: exit $rc, want $want; stderr: $err"
  if [ -n "$text" ]; then
    case "$err" in
      *"$text"*) ;;
      *) fail "$label: stderr lacks '$text': $err" ;;
    esac
  fi
  pass=$((pass + 1))
}

size_base=$'# comment\nsize commands/a.md 250 30000\nfallbacks commands/a.md 5\n'

check "size list: an unchanged file passes" "$size_base" "$size_base" 0
check "size list: a lowered number passes" "$size_base" $'size commands/a.md 240 30000\nfallbacks commands/a.md 5\n' 0
check "size list: a removed row passes" "$size_base" $'size commands/a.md 250 30000\n' 0
check "size list: an added row fails" "$size_base" "${size_base}size commands/b.md 300 40000"$'\n' 1 "a row was added"
check "size list: a raised number fails" "$size_base" $'size commands/a.md 250 30001\nfallbacks commands/a.md 5\n' 1 "went up"
check "size list: a raised fallback count fails" "$size_base" $'size commands/a.md 250 30000\nfallbacks commands/a.md 6\n' 1 "went up"

check "size list: a moved row with the same numbers passes" "$size_base" $'size commands/b.md 250 30000\nfallbacks commands/b.md 5\n' 0
check "size list: a moved row with a lower number passes" "$size_base" $'size commands/b.md 240 30000\nfallbacks commands/a.md 5\n' 0
check "size list: a moved row with a higher number fails" "$size_base" $'size commands/b.md 251 30000\nfallbacks commands/a.md 5\n' 1 "a row was added"
check "size list: a moved row of another kind fails" "$size_base" $'size commands/a.md 250 30000\nsize commands/b.md 5 5\n' 1 "a row was added"

graph_base=$'invoke commands/fix.md pr-create\nmention commands/x.md plan\n'

check "call graph list: an unchanged file passes" "$graph_base" "$graph_base" 0
check "call graph list: a removed row passes" "$graph_base" $'invoke commands/fix.md pr-create\n' 0
check "call graph list: an added row fails" "$graph_base" "${graph_base}invoke commands/y.md review"$'\n' 1 "a row was added"
check "call graph list: a renamed file passes" "$graph_base" $'invoke commands/fix2.md pr-create\nmention commands/x.md plan\n' 0 "" $'| `/playbook:fix` | `/playbook:fix2` | 1.0.0 |\n'
check "call graph list: a renamed target passes" "$graph_base" $'invoke commands/fix.md pr-open\nmention commands/x.md plan\n' 0 "" $'| `/playbook:pr-create` | `/playbook:pr-open` | 1.0.0 |\n'
check "call graph list: a renamed file and target pass together" "$graph_base" $'invoke commands/fix2.md pr-open\nmention commands/x.md plan\n' 0 "" $'| `/playbook:fix` | `/playbook:fix2` | 1.0.0 |\n| `/playbook:pr-create` | `/playbook:pr-open` | 1.0.0 |\n'
check "call graph list: a renamed skill folder passes" $'rule-load skills/old/SKILL.md x\n' $'rule-load skills/new/SKILL.md x\n' 0 "" $'| `/playbook:old` | `/playbook:new` | 1.0.0 |\n'
check "call graph list: a change no renamed page explains fails" "$graph_base" $'invoke commands/fix2.md pr-create\nmention commands/x.md plan\n' 1 "a row was added"
check "call graph list: a new file and a new target fail" "$graph_base" $'invoke commands/y.md review\nmention commands/x.md plan\n' 1 "a row was added"
check "call graph list: a row of another kind fails" "$graph_base" $'mention commands/fix.md pr-create\nmention commands/x.md plan\n' 1 "a row was added"

# A list that is new in the change has no base and passes.
repo="$TMP/repo-new"
mkdir -p "$repo"
(
  cd "$repo"
  git init -q
  git commit -q --allow-empty -m base
  mkdir -p "$(dirname "$LIST")"
  printf '%s' "$size_base" >"$LIST"
  git add -A
  git commit -q -m head
)
rc=0
(cd "$repo" && CI=1 bash "$SCRIPT" "$LIST" <&- >/dev/null 2>&1) || rc=$?
[ "$rc" -eq 0 ] || fail "a new list: exit $rc, want 0"
pass=$((pass + 1))

# No argument is a usage error.
rc=0
(cd "$repo" && CI=1 bash "$SCRIPT" <&- >/dev/null 2>&1) || rc=$?
[ "$rc" -eq 2 ] || fail "no argument: exit $rc, want 2"
pass=$((pass + 1))

echo "check-size-budget-shrink.test.sh: $pass checks passed"
