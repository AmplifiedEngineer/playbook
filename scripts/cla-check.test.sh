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
  git -C "$REPO" config user.name "Test Runner"
  git -C "$REPO" config user.email "runner@example.com"
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

# ---- CLA: co-authors
repo co-unsigned '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Jane Roe <jane@example.com>"
run_cla --pr-author alice
expect "human co-author unsigned" "$EXIT_FAIL" "Jane Roe <jane@example.com> is a co-author of commit"
case "$OUT" in
  *"must add their own row"*"in a commit that they author"*) pass=$((pass + 1)) ;;
  *) fail "co-author message does not say what to do: $OUT" ;;
esac

repo co-signed '| @alice | 2026-01-02 | |' '| @jane | 2026-01-03 | jane@example.com |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Jane Roe <JANE@example.com>"
run_cla --pr-author alice
expect "human co-author signed (email)" "$EXIT_PASS"

repo co-signed-id '| @alice | 2026-01-02 | |' '| @jane | 2026-01-03 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Jane Roe <55+jane@users.noreply.github.com>"
run_cla --pr-author alice
expect "human co-author signed (noreply login)" "$EXIT_PASS"

repo co-signs-here '| @alice | 2026-01-02 | |'
add_row '| @jane | 2026-11-06 | jane@example.com |'
commit_as "Jane Roe" "jane@example.com" "docs(cla): sign for jane

$STATEMENT"
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Jane Roe <jane@example.com>"
run_cla --pr-author alice
expect "row for a person other than the PR author" "$EXIT_FAIL" "only the pull request author can sign"

repo co-ai '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Claude <noreply@anthropic.com>
Co-authored-by: Copilot <175728472+Copilot@users.noreply.github.com>
Co-authored-by: ChatGPT Codex <noreply@openai.com>
Co-authored-by: Gemini <gemini-code-assist[bot]@users.noreply.github.com>
Co-authored-by: Cursor Agent <cursoragent@cursor.com>
Co-authored-by: Devin AI <devin-ai-integration[bot]@users.noreply.github.com>"
run_cla --pr-author alice
expect "AI co-authors ignored" "$EXIT_PASS"

repo co-bot '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>
Co-authored-by: github-actions[bot] <41898282+github-actions[bot]@users.noreply.github.com>"
run_cla --pr-author alice
expect "bot co-authors ignored" "$EXIT_PASS"

repo co-extra '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Mystery Tool <mystery@tool.example>"
run_cla --pr-author alice
expect "unknown tool fails by default" "$EXIT_FAIL" "Mystery Tool"
run_cla --pr-author alice --extra-ignored-coauthors '@tool.example'
expect "extra ignore pattern" "$EXIT_PASS"
run_cla --pr-author alice --ignored-coauthors 'only-this'
expect "replaced ignore list" "$EXIT_FAIL" "Mystery Tool"

repo co-name-only '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Jane Roe"
run_cla --pr-author alice
expect "malformed trailer fails closed" "$EXIT_FAIL" "not in the form"

repo co-empty '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

co-authored-by:"
run_cla --pr-author alice
expect "empty trailer fails closed" "$EXIT_FAIL" "not in the form"

repo co-ai-no-email '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Claude"
run_cla --pr-author alice
expect "AI trailer without email fails closed" "$EXIT_FAIL" "not in the form"

repo co-many '| @alice | 2026-01-02 | |' '| @jane | 2026-01-03 | jane@example.com |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

CO-AUTHORED-BY: Jane Roe <jane@example.com>
Co-authored-by: Bob Low <bob@example.com>
Co-authored-by: Cy Dee <cy@example.com>
Co-authored-by: Claude <noreply@anthropic.com>"
run_cla --pr-author alice
expect "many co-authors, two unsigned" "$EXIT_FAIL" "Bob Low <bob@example.com>"
case "$OUT" in
  *"Cy Dee <cy@example.com>"*) pass=$((pass + 1)) ;;
  *) fail "second unsigned co-author not named: $OUT" ;;
esac
case "$OUT" in
  *"Jane Roe"*) fail "signed co-author wrongly named: $OUT" ;;
  *) pass=$((pass + 1)) ;;
esac

repo co-same '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Alice A <1234+alice@users.noreply.github.com>"
run_cla --pr-author alice
expect "author is also co-author" "$EXIT_PASS"

repo co-same-unsigned
work
commit_as "Bob" "bob@example.com" "feat: a change

Co-authored-by: Bob <bob@example.com>"
run_cla
expect "unsigned author listed as co-author" "$EXIT_FAIL" "authored a commit but has not signed"

repo co-on-bot-commit '| @alice | 2026-01-02 | |'
work
commit_as "dependabot[bot]" "49699333+dependabot[bot]@users.noreply.github.com" "chore: bump

Co-authored-by: Jane Roe <jane@example.com>"
run_cla --pr-author alice
expect "co-author on a bot commit is checked" "$EXIT_FAIL" "Jane Roe"

# ---- CLA: security regressions
# A row for another login that borrows the committer's own email.
repo forge-email '| @mallory | 2026-01-02 | |'
add_row '| @alice | 2026-11-06 | mallory@example.com |'
commit_as "Mallory" "mallory@example.com" "docs(cla): sign

$STATEMENT"
run_cla --pr-author mallory
expect "row for another login with own email" "$EXIT_FAIL" "only"

# A row for another login, with a spoofed noreply address on the commit.
repo forge-noreply '| @mallory | 2026-01-02 | |'
add_row '| @alice | 2026-11-06 | |'
commit_as "Alice" "9+alice@users.noreply.github.com" "docs(cla): sign

$STATEMENT"
run_cla --pr-author mallory
expect "row for another login via noreply" "$EXIT_FAIL" "only the pull request author can sign"

# The email on the row must be the email of the commit that adds it.
repo row-email-borrowed
add_row '| @alice | 2026-11-06 | bob@example.com |'
commit_as "Alice" "alice@example.com" "docs(cla): sign

$STATEMENT"
run_cla --pr-author alice
expect "row email not the commit email" "$EXIT_FAIL" "only the person signing"

# A git name ending in [bot] does not make a person a bot when the host says User.
repo bot-name
work
commit_as "me[bot]" "me@example.com" "feat: sneaky"
sha="$(git -C "$REPO" rev-parse pr)"
printf '%s\tme\tUser\tme\tUser\n' "$sha" >"$TMP/logins.tsv"
run_cla --logins "$TMP/logins.tsv"
expect "bot-looking name, host says User (cla)" "$EXIT_FAIL" "has not signed"
RC=0
(cd "$REPO" && bash "$SCRIPT" dco --base main --head pr --logins "$TMP/logins.tsv") >"$TMP/out" 2>&1 || RC=$?
OUT="$(cat "$TMP/out")"
expect "bot-looking name, host says User (dco)" "$EXIT_FAIL" "no Signed-off-by"
# A commit missing from the host list (more than 250 commits) is not trusted as a bot.
: >"$TMP/logins.tsv"
run_cla --logins "$TMP/logins.tsv"
expect "bot-looking name, commit not in host list" "$EXIT_FAIL" "has not signed"
# The host says Bot: skipped.
printf '%s\tdependabot[bot]\tBot\tweb-flow\tUser\n' "$sha" >"$TMP/logins.tsv"
run_cla --logins "$TMP/logins.tsv"
expect "host says Bot" "$EXIT_PASS"

# A renamed PR author is matched by account id.
repo renamed-author '| @oldname | 2026-01-02 | 77+oldname@users.noreply.github.com |'
work
commit_as "New" "77+newname@users.noreply.github.com" "feat: a change"
run_cla --pr-author newname --pr-author-id 77
expect "renamed PR author by id" "$EXIT_PASS"

# A row with an empty date still signs.
repo empty-date
add_row '| @alice | | alice@example.com |'
commit_as "Alice" "alice@example.com" "docs(cla): sign

$STATEMENT"
run_cla --pr-author alice
expect "empty date" "$EXIT_PASS"

# The branch point predates the signature file.
REPO="$TMP/old-branch"
mkdir -p "$REPO"
git -C "$REPO" init -q -b main
git -C "$REPO" config commit.gpgsign false
git -C "$REPO" config core.hooksPath /dev/null
commit_as "Maintainer" "maint@example.com" "chore: first"
git -C "$REPO" checkout -q -b pr
git -C "$REPO" checkout -q main
mkdir -p "$REPO/.github"
printf '<!-- CLA-BOT:START -->\n| @alice | 2026-01-02 | |\n<!-- CLA-BOT:END -->\n' >"$REPO/$FILE"
commit_as "Maintainer" "maint@example.com" "docs: add the list"
git -C "$REPO" checkout -q pr
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change"
run_cla --pr-author alice
expect "branch point without the file" "$EXIT_PASS"

# The pull request deletes the signature file.
repo deletes-file '| @alice | 2026-01-02 | |'
git -C "$REPO" rm -q "$FILE"
commit_as "Alice" "1234+alice@users.noreply.github.com" "chore: remove"
run_cla --pr-author alice
expect "file deleted by the PR" "$EXIT_FAIL" "append-only"

# A human co-author with an AI-like display name is still checked.
repo co-human-claude '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Claude Smith <bob@example.com>
Co-authored-by: Copilot <carol@example.com>"
run_cla --pr-author alice
expect "AI-like name with a human email" "$EXIT_FAIL" "Claude Smith <bob@example.com>"

# An AI-vendor address cannot be extended by suffix tricks.
repo co-vendor-lookalike '| @alice | 2026-01-02 | |'
work
commit_as "Alice" "1234+alice@users.noreply.github.com" "feat: a change

Co-authored-by: Bob <noreply@anthropic.com.evil.example>"
run_cla --pr-author alice
expect "lookalike vendor address" "$EXIT_FAIL" "Bob"

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
