#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# cla-check.test.sh: offline tests for cla-check.sh. Each case builds a small
# git repository in a temp directory: a main branch with a signature file, and
# a pull request branch with the commits under test.
#
# Usage: cla-check.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."

SCRIPT="${CLA_CHECK_SCRIPT:-$PWD/scripts/cla-check.sh}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

STATEMENT='I have read the CLA Document and I hereby sign the CLA.'
FILE='.github/CONTRIBUTORS.md'
EXIT_PASS=0
EXIT_FAIL=1
EXIT_USAGE=2

pass=0
fail() { echo "FAIL $*" >&2; exit 1; }

RC=0
OUT=""
REPO=""

# repo <name> <rows...>: a repository whose main branch lists the given rows.
# The first commit is made by the maintainer.
repo() {
  local name="$1"
  shift
  REPO="$TMP/$name"
  mkdir -p "$REPO/.github"
  git -C "$REPO" init -q -b main
  git -C "$REPO" config commit.gpgsign false
  # A global hook that adds Signed-off-by would hide a missing trailer.
  git -C "$REPO" config core.hooksPath /dev/null
  {
    echo '# Contributors'
    echo '<!-- CLA-BOT:START - Do not remove this marker -->'
    echo '| GitHub Username | Date | Email |'
    echo '| --------------- | ---- | ----- |'
    for r in "$@"; do echo "$r"; done
    echo '<!-- CLA-BOT:END - Do not remove this marker -->'
    echo 'Corporate: none.'
  } >"$REPO/$FILE"
  commit_as "Maintainer" "maint@example.com" "chore: seed"
  git -C "$REPO" checkout -q -b pr
}

# commit_as <name> <email> <message>: commits the working tree.
commit_as() {
  git -C "$REPO" add -A
  GIT_AUTHOR_NAME="$1" GIT_AUTHOR_EMAIL="$2" GIT_COMMITTER_NAME="$1" GIT_COMMITTER_EMAIL="$2" \
    git -C "$REPO" commit -q --allow-empty -m "$3"
}

# add_row <row>: inserts a row above the end marker.
add_row() {
  awk -v row="$1" '/CLA-BOT:END/ { print row } { print }' "$REPO/$FILE" >"$REPO/.rows.tmp"
  mv "$REPO/.rows.tmp" "$REPO/$FILE"
}

work() { echo "x" >>"$REPO/work.txt"; }

# run_cla <extra args...> and run_dco <extra args...>: run the check on $REPO.
run_cla() {
  RC=0
  (cd "$REPO" && bash "$SCRIPT" cla --base main --head pr "$@") >"$TMP/out" 2>&1 || RC=$?
  OUT="$(cat "$TMP/out")"
}
run_dco() {
  RC=0
  (cd "$REPO" && bash "$SCRIPT" dco --base main --head pr) >"$TMP/out" 2>&1 || RC=$?
  OUT="$(cat "$TMP/out")"
}

# expect <label> <exit code> [text the output must contain]
expect() {
  local label="$1" code="$2" text="${3:-}"
  [ "$RC" -eq "$code" ] || fail "$label: exit $RC, want $code; output: $OUT"
  if [ -n "$text" ]; then
    case "$OUT" in
      *"$text"*) ;;
      *) fail "$label: output lacks \"$text\": $OUT" ;;
    esac
  fi
  pass=$((pass + 1))
}

# ---- CLA: signed already
repo signed '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change"
run_cla --pr-author alice
expect "signed already" "$EXIT_PASS" "every author is on the signature list"

# ---- CLA: signs in this pull request
repo signs-here
add_row '| @alice | 2026-11-06 | alice@example.com |'
commit_as "Alice" "alice@example.com" "docs(cla): sign for alice

$STATEMENT"
work
commit_as "Alice" "alice@example.com" "feat: a change"
run_cla --pr-author alice
expect "signs in PR" "$EXIT_PASS"

# ---- CLA: signs for someone else (must fail)
repo for-other '| @alice | 2026-01-02 | |'
add_row '| @carol | 2026-11-06 | carol@example.com |'
commit_as "Alice" "1234+alice@users.noreply.github.com" "docs(cla): sign for carol

$STATEMENT"
run_cla --pr-author alice
expect "signs for someone else" "$EXIT_FAIL" "only the person signing may add their row"

# ---- CLA: missing signature (must fail)
repo missing
work
commit_as "Mallory" "mallory@example.com" "feat: a change"
run_cla --pr-author mallory
expect "missing signature" "$EXIT_FAIL" "has not signed the CLA"

# ---- CLA: bot allowed
repo bot '| @alice | 2026-01-02 | |'
work
commit_as "dependabot[bot]" "49699333+dependabot[bot]@users.noreply.github.com" "chore: bump"
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change"
run_cla --pr-author alice
expect "bot allowed" "$EXIT_PASS"

# ---- CLA: a bot-only pull request needs no signature
repo bot-only
work
commit_as "github-actions[bot]" "41898282+github-actions[bot]@users.noreply.github.com" "chore: sync"
run_cla --pr-author 'dependabot[bot]'
expect "bot only" "$EXIT_PASS"

# ---- CLA: extra bot from the --bots list
repo extra-bot
work
commit_as "Release Robot" "robot@example.com" "chore: release"
run_cla --bots 'robot@example.com'
expect "extra bot" "$EXIT_PASS"

# ---- CLA: the signing commit lacks the sentence (must fail)
repo no-sentence
add_row '| @alice | 2026-11-06 | alice@example.com |'
commit_as "Alice" "alice@example.com" "docs: add me"
run_cla --pr-author alice
expect "no sentence" "$EXIT_FAIL" "lacks the sentence"

# ---- CLA: the sentence survives line wrapping and case
repo wrapped
add_row '| @alice | 2026-11-06 | alice@example.com |'
commit_as "Alice" "alice@example.com" "docs: sign

i have read the CLA Document
and I hereby sign the CLA."
run_cla --pr-author alice
expect "wrapped sentence" "$EXIT_PASS"

# ---- CLA: another person's commit adds the row (must fail)
repo wrong-author
add_row '| @alice | 2026-11-06 | alice@example.com |'
commit_as "Bob" "bob@example.com" "docs(cla): sign for alice

$STATEMENT"
run_cla --pr-author alice
expect "row added by another author" "$EXIT_FAIL" "only the person signing may add their row"

# ---- CLA: a second author must have signed too (must fail)
repo co-author '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change"
work
commit_as "Bob" "bob@example.com" "fix: more"
run_cla --pr-author alice
expect "unsigned co-author" "$EXIT_FAIL" "Bob <bob@example.com> authored a commit but has not signed"

# ---- CLA: changing or removing a signature is refused (must fail)
repo tamper '| @alice | 2026-01-02 | |'
awk '!/@alice/' "$REPO/$FILE" >"$REPO/.rows.tmp"
mv "$REPO/.rows.tmp" "$REPO/$FILE"
commit_as "Alice" "1234+alice@users.noreply.github.com" "chore: tidy"
run_cla --pr-author alice
expect "removed signature" "$EXIT_FAIL" "append-only"

# ---- CLA: the pull request author must be signed (must fail)
repo author-only '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change"
run_cla --pr-author bob
expect "unsigned PR author" "$EXIT_FAIL" "@bob (the pull request author) has not signed"

# ---- CLA: login from the host API map, real email on the commit
repo mapped '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "alice@work.example" "feat: a change"
sha="$(git -C "$REPO" rev-parse pr)"
printf '%s\talice\tUser\tweb-flow\tUser\n' "$sha" >"$TMP/logins.tsv"
run_cla --pr-author alice --logins "$TMP/logins.tsv"
expect "login from map" "$EXIT_PASS"
run_cla --pr-author alice
expect "same commit without the map" "$EXIT_FAIL" "has not signed"

# ---- CLA: a renamed account keeps its numeric id in the noreply address
repo renamed '| @oldname | 2026-01-02 | 77+oldname@users.noreply.github.com |'
work
commit_as "New Name" "77+newname@users.noreply.github.com" "feat: a change"
run_cla
expect "renamed by account id" "$EXIT_PASS"

# ---- CLA: the email on the row matches a commit made with that email
repo row-email '| @oldname | 2026-01-02 | person@example.com |'
work
commit_as "Person" "person@example.com" "feat: a change"
run_cla
expect "match on row email" "$EXIT_PASS"

# ---- CLA: a merge commit from main adds nothing to check
repo merged '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change"
git -C "$REPO" checkout -q main
echo "m" >"$REPO/main.txt"
commit_as "Stranger" "stranger@example.com" "feat: landed on main"
git -C "$REPO" checkout -q pr
GIT_AUTHOR_NAME=Alice GIT_AUTHOR_EMAIL=1234+alice@users.noreply.github.com \
  GIT_COMMITTER_NAME=Alice GIT_COMMITTER_EMAIL=1234+alice@users.noreply.github.com \
  git -C "$REPO" merge -q --no-edit main
run_cla --pr-author alice
expect "merge from main" "$EXIT_PASS"

# ---- CLA: signature file missing on the default branch (must fail)
repo nofile
git -C "$REPO" checkout -q main
git -C "$REPO" rm -q "$FILE"
commit_as "Maintainer" "maint@example.com" "chore: drop file"
git -C "$REPO" checkout -q pr
run_cla
expect "missing file on base" "$EXIT_FAIL" "is missing on the default branch"

# ---- usage errors
repo usage
run_cla --nope x
expect "unknown option" "$EXIT_USAGE" "unknown option"
RC=0
(cd "$REPO" && bash "$SCRIPT" nonsense) >"$TMP/out" 2>&1 || RC=$?
OUT="$(cat "$TMP/out")"
expect "unknown mode" "$EXIT_USAGE" "first argument"

# ---- DCO
repo dco-ok
work
commit_as "Alice" "alice@example.com" "feat: a change

Signed-off-by: Alice <alice@example.com>"
run_dco
expect "dco signed off" "$EXIT_PASS" "every commit is signed off"

repo dco-missing
work
commit_as "Alice" "alice@example.com" "feat: a change"
run_dco
expect "dco missing" "$EXIT_FAIL" "no Signed-off-by trailer"

repo dco-bot
work
commit_as "dependabot[bot]" "49699333+dependabot[bot]@users.noreply.github.com" "chore: bump"
run_dco
expect "dco bot skipped" "$EXIT_PASS"

repo dco-merge
work
commit_as "Alice" "alice@example.com" "feat: a change

Signed-off-by: Alice <alice@example.com>"
git -C "$REPO" checkout -q main
echo "m" >"$REPO/main.txt"
commit_as "Maintainer" "maint@example.com" "feat: landed on main"
git -C "$REPO" checkout -q pr
git -C "$REPO" merge -q --no-edit main
run_dco
expect "dco merge commit skipped" "$EXIT_PASS"

echo "ok: $pass checks passed"
