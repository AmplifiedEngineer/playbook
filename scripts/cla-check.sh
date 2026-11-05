#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# cla-check.sh: the CLA and DCO checks for a pull request, from the git
# history alone. A signature is a row in the signatures file that a contributor
# adds in a commit of their own. Nothing is stored outside the repository.
#
# Usage:
#   cla-check.sh cla|dco --base REF --head REF [options]
#
#   --base REF        the default branch (signatures already accepted)
#   --head REF        the tip of the pull request
#   --file PATH       signatures file (default .github/CONTRIBUTORS.md)
#   --pr-author LOGIN the pull request author; must be signed
#   --logins FILE     optional TSV from the host API, one line per commit:
#                     sha, author login, author type, committer login,
#                     committer type. Without it the script reads logins from
#                     GitHub noreply addresses and matches on email.
#   --bots LIST       extra bot logins or emails, comma separated
#   --ignored-coauthors LIST
#                     replaces the built-in list of AI and bot co-author
#                     patterns (comma separated, see below)
#   --extra-ignored-coauthors LIST
#                     adds patterns to the list in use
#   --statement TEXT  sentence a signing commit message must contain
#   --cla-url URL     where the CLA text lives, for the failure message
#
# Co-authors: every Co-authored-by trailer in a pull request commit names a
# person who must be signed, like an author. A pattern in the ignore list skips
# AI tools and bots. A pattern with "@" or "[" is a substring of the email or
# name ("[bot]", "noreply@anthropic.com"). Any other pattern is a whole word in
# the name or email ("claude", "copilot"). A trailer that is not
# "Name <email>" and does not match the list fails.
#
# Run it inside a clone. It reads git objects and the optional TSV only; it
# uses no network and no credentials. Prints a Markdown report. Exit 0 when
# the check passes, 1 when it fails, 2 on a usage error.
set -euo pipefail

EXIT_PASS=0
EXIT_FAIL=1
EXIT_USAGE=2
DEFAULT_FILE='.github/CONTRIBUTORS.md'
DEFAULT_STATEMENT='I have read the CLA Document and I hereby sign the CLA.'
DEFAULT_IGNORED_COAUTHORS='[bot],noreply@anthropic.com,noreply@github.com,github-actions,dependabot,renovate,claude,anthropic,copilot,chatgpt,openai,codex,gemini,cursor,devin,aider,windsurf,codeium,tabnine,sweep'
US=$'\037'
TAB=$'\t'

die() {
  echo "cla-check: $*" >&2
  exit "$EXIT_USAGE"
}

mode="${1:-}"
case "$mode" in
  cla | dco) shift ;;
  *) die "first argument must be cla or dco" ;;
esac

base='' head='' file="$DEFAULT_FILE" pr_author='' logins='' bots=''
statement="$DEFAULT_STATEMENT" cla_url=''
ignored_co="$DEFAULT_IGNORED_COAUTHORS" extra_co=''
while [ "$#" -gt 0 ]; do
  [ "$#" -ge 2 ] || die "$1 needs a value"
  case "$1" in
    --base) base="$2" ;;
    --head) head="$2" ;;
    --file) file="$2" ;;
    --pr-author) pr_author="$2" ;;
    --logins) logins="$2" ;;
    --bots) bots="$2" ;;
    --statement) statement="$2" ;;
    --cla-url) cla_url="$2" ;;
    --ignored-coauthors) ignored_co="$2" ;;
    --extra-ignored-coauthors) extra_co="$2" ;;
    *) die "unknown option $1" ;;
  esac
  shift 2
done
[ -n "$ignored_co" ] || ignored_co="$DEFAULT_IGNORED_COAUTHORS"
[ -z "$extra_co" ] || ignored_co="$ignored_co,$extra_co"
[ -n "$statement" ] || statement="$DEFAULT_STATEMENT"
[ -n "$file" ] || file="$DEFAULT_FILE"
[ -n "$base" ] || die "--base is required"
[ -n "$head" ] || die "--head is required"
[ -z "$logins" ] || [ -r "$logins" ] || die "cannot read $logins"
git rev-parse --verify --quiet "$base^{commit}" >/dev/null || die "unknown base $base"
git rev-parse --verify --quiet "$head^{commit}" >/dev/null || die "unknown head $head"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
: >"$TMP/errors"

lower() { printf '%s' "$1" | tr '[:upper:]' '[:lower:]'; }
norm_space() { tr -s '[:space:]' ' ' | sed 's/^ //; s/ $//'; }

# is_bot LOGIN TYPE NAME EMAIL: true for an automation account.
is_bot() {
  local l n e extra item
  l="$(lower "$1")"
  n="$(lower "$3")"
  e="$(lower "$4")"
  [ "$2" = "Bot" ] && return 0
  case "$l" in *'[bot]') return 0 ;; esac
  case "$n" in *'[bot]') return 0 ;; esac
  case "$e" in *'[bot]@'*) return 0 ;; esac
  extra="$(lower "$bots")"
  while [ -n "$extra" ]; do
    item="${extra%%,*}"
    if [ "$extra" = "$item" ]; then extra=''; else extra="${extra#*,}"; fi
    item="$(printf '%s' "$item" | tr -d ' ')"
    [ -n "$item" ] || continue
    if [ "$item" = "$l" ] || [ "$item" = "$e" ]; then return 0; fi
  done
  return 1
}

# keys LOGIN EMAIL: identity keys of one person, one per line. A person matches
# a signature when any key is shared. A GitHub noreply address carries the
# numeric account id, which survives a login rename.
keys() {
  local l e
  l="$(lower "$1")"
  e="$(lower "$2")"
  l="${l#@}"
  [ -z "$l" ] || echo "l:$l"
  if [ -n "$e" ]; then
    echo "e:$e"
    case "$e" in
      *@users.noreply.github.com)
        local local_part="${e%@*}"
        case "$local_part" in
          *+*) echo "g:${local_part%%+*}"; echo "l:${local_part#*+}" ;;
          *) echo "l:$local_part" ;;
        esac
        ;;
    esac
  fi
}

# parse_rows MARKERS: reads text on stdin, prints canonical rows
# "login<TAB>date<TAB>email" (lowercase login and email). With MARKERS=1 only
# the lines between CLA-BOT:START and CLA-BOT:END count.
parse_rows() {
  awk -v markers="$1" -F'|' '
    function trim(s) { gsub(/^[ \t\r]+|[ \t\r]+$/, "", s); return s }
    markers == 1 && /CLA-BOT:START/ { on = 1; next }
    markers == 1 && /CLA-BOT:END/ { on = 0; next }
    markers == 1 && !on { next }
    /^[ \t]*\|/ {
      login = trim($2)
      if (login !~ /^@[A-Za-z0-9][A-Za-z0-9-]*$/) next
      date = trim($3); email = trim($4)
      printf "%s\t%s\t%s\n", tolower(login), date, tolower(email)
    }'
}

rows_at() {
  git show "$1:$file" 2>/dev/null | parse_rows 1 | sort -u
}

# login_from_map SHA FIELD: column FIELD (2 author login, 3 author type,
# 4 committer login, 5 committer type) for SHA from the optional TSV.
from_map() {
  [ -n "$logins" ] || return 0
  awk -F'\t' -v s="$1" -v c="$2" '$1 == s { print $c; exit }' "$logins"
}

add_error() { printf '%s\n' "$1" >>"$TMP/errors"; }

# commits_in_range: PR commits, oldest first, merge commits left out. A merge
# only combines commits that were checked on their own.
commits_in_range() { git rev-list --no-merges --reverse "$base..$head"; }

report_and_exit() {
  local title="$1" okmsg="$2" n
  n="$(wc -l <"$TMP/errors" | tr -d ' ')"
  if [ "$n" -eq 0 ]; then
    echo "$okmsg"
    exit "$EXIT_PASS"
  fi
  echo "### $title"
  echo
  sort -u "$TMP/errors" | sed 's/^/- /'
  if [ -n "${hint:-}" ]; then
    echo
    printf '%s\n' "$hint"
  fi
  exit "$EXIT_FAIL"
}

# ---------------------------------------------------------------- DCO
if [ "$mode" = dco ]; then
  hint="Add the trailer with \`git commit --amend --signoff\` (or \`git rebase --signoff\`) and push again."
  for sha in $(commits_in_range); do
    IFS="$US" read -r an ae cn ce < <(git log -1 --format="%an$US%ae$US%cn$US%ce" "$sha")
    al="$(from_map "$sha" 2)"; at="$(from_map "$sha" 3)"
    cl="$(from_map "$sha" 4)"; ct="$(from_map "$sha" 5)"
    # Bot commits (dependabot, the update bot) cannot carry a trailer.
    if is_bot "$al" "$at" "$an" "$ae" || is_bot "$cl" "$ct" "$cn" "$ce"; then
      continue
    fi
    if ! git log -1 --format=%B "$sha" | grep -Eq '^Signed-off-by: .+'; then
      add_error "commit $(printf '%.7s' "$sha") has no Signed-off-by trailer"
    fi
  done
  report_and_exit 'DCO check failed' 'DCO: every commit is signed off'
fi

# ---------------------------------------------------------------- CLA
hint="Sign once in your own pull request. Add a row \`| @your-login | YYYY-MM-DD | you@example.com |\` to \`$file\`, between the CLA-BOT markers, in a commit you author, and put this sentence in that commit message: $statement"
[ -z "$cla_url" ] || hint="$hint (CLA text: $cla_url)"

if ! git cat-file -e "$base:$file" 2>/dev/null; then
  add_error "$file is missing on the default branch. Add it in a separate pull request first."
  report_and_exit 'CLA check failed' ''
fi

rows_at "$base" >"$TMP/base_rows"
rows_at "$head" >"$TMP/head_rows"
mb="$(git merge-base "$base" "$head" 2>/dev/null || true)"
if [ -n "$mb" ]; then rows_at "$mb" >"$TMP/mb_rows"; else cp "$TMP/base_rows" "$TMP/mb_rows"; fi

# Signatures are append-only: a pull request may not edit or drop one.
comm -23 "$TMP/mb_rows" "$TMP/head_rows" >"$TMP/removed"
while IFS="$TAB" read -r l d e; do
  [ -n "$l" ] || continue
  add_error "the signature row for $l was changed or removed; signatures are append-only"
done <"$TMP/removed"

comm -13 "$TMP/base_rows" "$TMP/head_rows" >"$TMP/added"
: >"$TMP/valid_rows"
: >"$TMP/reasons"
: >"$TMP/people"
: >"$TMP/author_keys"
: >"$TMP/base_keys"
while IFS="$TAB" read -r l d e; do
  [ -n "$l" ] || continue
  keys "$l" "$e" >>"$TMP/base_keys"
done <"$TMP/base_rows"

statement_n="$(printf '%s' "$statement" | norm_space | tr '[:upper:]' '[:lower:]')"

for sha in $(commits_in_range); do
  short="$(printf '%.7s' "$sha")"
  IFS="$US" read -r an ae < <(git log -1 --format="%an$US%ae" "$sha")
  al="$(from_map "$sha" 2)"; at="$(from_map "$sha" 3)"
  if is_bot "$al" "$at" "$an" "$ae"; then
    continue
  fi
  keys "$al" "$ae" | sort -u >"$TMP/c_keys"
  printf '%s\t%s\t%s\n' "$an" "$ae" "$al" >>"$TMP/people"
  cat "$TMP/c_keys" >>"$TMP/author_keys"

  msg_ok=0
  if git log -1 --format=%B "$sha" | norm_space | tr '[:upper:]' '[:lower:]' | grep -Fq -- "$statement_n"; then
    msg_ok=1
  fi
  git diff-tree --root --no-commit-id -r -p -U0 "$sha" -- "$file" \
    | sed -n 's/^+\(|.*\)$/\1/p' | parse_rows 0 | sort -u >"$TMP/c_rows" || true
  while IFS="$TAB" read -r l d e; do
    [ -n "$l" ] || continue
    row="$l$TAB$d$TAB$e"
    grep -Fxq -- "$row" "$TMP/added" || continue
    keys "$l" "$e" | sort -u >"$TMP/r_keys"
    if ! grep -Fxq -f "$TMP/r_keys" "$TMP/c_keys"; then
      printf '%s\037%s\n' "$row" "$l was added in commit $short, which $an <$ae> authored; only the person signing may add their row" >>"$TMP/reasons"
    elif [ "$msg_ok" -ne 1 ]; then
      printf '%s\037%s\n' "$row" "commit $short adds $l but its message lacks the sentence: $statement" >>"$TMP/reasons"
    else
      printf '%s\n' "$row" >>"$TMP/valid_rows"
    fi
  done <"$TMP/c_rows"
done

sort -u "$TMP/valid_rows" -o "$TMP/valid_rows"
while IFS="$TAB" read -r l d e; do
  [ -n "$l" ] || continue
  row="$l$TAB$d$TAB$e"
  grep -Fxq -- "$row" "$TMP/valid_rows" && continue
  why="$(awk -F'\037' -v r="$row" '$1 == r { print $2 }' "$TMP/reasons" 2>/dev/null | head -n 1 || true)"
  add_error "${why:-the row for $l was not added in a commit that signs for it}"
done <"$TMP/added"

while IFS="$TAB" read -r l d e; do
  [ -n "$l" ] || continue
  keys "$l" "$e" >>"$TMP/signed_keys"
done <"$TMP/valid_rows"
cat "$TMP/base_keys" >>"$TMP/signed_keys" 2>/dev/null || true
touch "$TMP/signed_keys"

# signed NAME EMAIL LOGIN: true when the person is on the base list or signs in
# a valid row of this pull request.
signed() {
  keys "$3" "$2" | sort -u >"$TMP/p_keys"
  [ -s "$TMP/p_keys" ] || return 1
  grep -Fxq -f "$TMP/p_keys" "$TMP/signed_keys"
}

if [ -n "$pr_author" ] && ! is_bot "$pr_author" '' '' ''; then
  if ! signed '' '' "$pr_author"; then
    add_error "@$(lower "${pr_author#@}") (the pull request author) has not signed the CLA"
  fi
fi
sort -u "$TMP/people" | while IFS="$TAB" read -r an ae al; do
  if ! signed "$an" "$ae" "$al"; then
    who="$an <$ae>"
    [ -z "$al" ] || who="$who (@$al)"
    add_error "$who authored a commit but has not signed the CLA"
  fi
done >>"$TMP/errors.more" || true
[ ! -s "$TMP/errors.more" ] || cat "$TMP/errors.more" >>"$TMP/errors"

# is_ignored_coauthor NAME EMAIL: true for an AI tool or bot on the ignore list.
is_ignored_coauthor() {
  local n e words rest p
  n="$(lower "$1")"
  e="$(lower "$2")"
  words=" $(printf '%s %s' "$n" "$e" | tr -c 'a-z0-9' ' ' | tr -s ' ') "
  rest="$(lower "$ignored_co")"
  while [ -n "$rest" ]; do
    p="${rest%%,*}"
    if [ "$rest" = "$p" ]; then rest=''; else rest="${rest#*,}"; fi
    p="$(printf '%s' "$p" | tr -d ' ')"
    [ -n "$p" ] || continue
    case "$p" in
      *@* | *'['*)
        case "$e" in *"$p"*) return 0 ;; esac
        case "$n" in *"$p"*) return 0 ;; esac
        ;;
      *)
        case "$words" in *" $p "*) return 0 ;; esac
        ;;
    esac
  done
  return 1
}

# Co-authors: each Co-authored-by trailer names a person who must be signed.
: >"$TMP/co_errors"
for sha in $(commits_in_range); do
  short="$(printf '%.7s' "$sha")"
  git log -1 --format=%B "$sha" | grep -iE '^co-authored-by[[:space:]]*:' >"$TMP/trailers" || true
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    val="$(printf '%s' "$line" | sed -E 's/^[^:]*:[[:space:]]*//; s/[[:space:]]+$//')"
    cemail="$(printf '%s' "$val" | sed -nE 's/^.*<([^<>[:space:]@]+@[^<>[:space:]@]+)>$/\1/p')"
    cname="$(printf '%s' "$val" | sed -E 's/[[:space:]]*<[^<>]*>$//')"
    if [ -z "$cemail" ]; then
      if is_ignored_coauthor "$val" ''; then continue; fi
      printf '%s\n' "commit $short has a Co-authored-by trailer that is not in the form \`Co-authored-by: Name <email>\`: \"$val\". Fix the trailer in the commit message, or remove it." >>"$TMP/co_errors"
      continue
    fi
    if is_ignored_coauthor "$cname" "$cemail"; then continue; fi
    keys '' "$cemail" | sort -u >"$TMP/co_keys"
    # The same person as an author is checked as an author.
    if grep -Fxq -f "$TMP/co_keys" "$TMP/author_keys"; then continue; fi
    if ! grep -Fxq -f "$TMP/co_keys" "$TMP/signed_keys"; then
      printf '%s\n' "${cname:-$cemail} <$cemail> is a co-author of commit $short and has not signed the CLA. ${cname:-$cemail} must add their own row to $file in a commit that they author (their own pull request is fine), with the sentence in the message: $statement" >>"$TMP/co_errors"
    fi
  done <"$TMP/trailers"
done
cat "$TMP/co_errors" >>"$TMP/errors"

report_and_exit 'CLA check failed' 'CLA: every author is on the signature list or signs in this pull request'
