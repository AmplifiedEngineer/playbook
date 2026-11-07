#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# release-notes.sh: release notes built from Conventional Commits (ADR 0037).
#
#   release-notes.sh json <component> <version>
#     Prints release-notes.json for <component> at <version>. It lists the
#     commits since the previous tag of <component> up to HEAD that touch the
#     component's paths, merges left out, sorted into sections.
#
#   release-notes.sh markdown <file>
#     Prints the markdown for a release-notes.json file.
#
#   release-notes.sh body <component> <version> <json-out>
#     Writes release-notes.json to <json-out> and prints the release body,
#     the markdown.
#
# The sections come in this order, and an empty one is left out:
#   breaking     `!` after the type or scope, or a BREAKING CHANGE footer
#   features     feat
#   fixes        fix
#   performance  perf
#   other        every other type, and subjects that are not conventional
# A breaking item shows the footer text when there is one, else the subject.
# A trailing "(#123)" becomes the pull request number.
# The release commit, "chore(release): playbook X.Y.Z", is left out, and so
# is the flake commit that follows a release, "chore(release): nix flake X.Y.Z".
#
# GITHUB_REPOSITORY names the repository for the links. A shallow clone is
# deepened, with its tags. Run from anywhere inside the repository; a
# relative file argument is read from the directory it was run in.
set -euo pipefail

CALLER="$PWD"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

REPO="${GITHUB_REPOSITORY:-amplified-engineer/playbook}"

fail() { echo "FAIL $*" >&2; exit 1; }

# $1 as an absolute path, read against the directory the script was run in.
from_caller() {
  case "$1" in
    /*) echo "$1" ;;
    *) echo "$CALLER/$1" ;;
  esac
}

# The components. Adding one means a line here, in prefix_of and paths_of,
# and a tag in release.yml. The telemetry line ended at telemetry-v0.2.0
# (ADR 0041).
COMPONENTS="playbook"

check_component() {
  case " $COMPONENTS " in
    *" $1 "*) ;;
    *) fail "unknown component '$1'. Expected one of: $COMPONENTS." ;;
  esac
}

prefix_of() {
  case "$1" in
    playbook) echo "v" ;;
  esac
}

# The paths each component's release is built from, one pathspec per line.
# A playbook release ships the plugin too (ADR 0038), and the binary builds
# every crate but the plugin checks. Cargo.lock counts for none, since every
# dependency bump rewrites it. A bump also changes a pin in a crate's
# Cargo.toml, so a Dependabot security PR is listed under Other changes.
paths_of() {
  case "$1" in
    playbook) printf '%s\n' crates ':(exclude)crates/plugin-checks' \
      Cargo.toml install.sh seed settings.shared.json prompts Brewfile plugins/playbook ;;
  esac
}

check_semver() {
  printf '%s' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
    || fail "version '$1' is not MAJOR.MINOR.PATCH"
}

# True when $1 is strictly greater than $2 (both MAJOR.MINOR.PATCH).
version_gt() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)" = "$1" ]
}

ensure_history() {
  if [ "$(git rev-parse --is-shallow-repository)" = "true" ]; then
    git fetch --quiet --unshallow --tags origin
  fi
}

# The highest tag version of component $1 below version $2, or nothing.
previous_version() {
  local prefix best="" tag v
  prefix="$(prefix_of "$1")"
  while IFS= read -r tag; do
    v="${tag#"$prefix"}"
    [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
    version_gt "$2" "$v" || continue
    if [ -z "$best" ] || version_gt "$v" "$best"; then best="$v"; fi
  done < <(git tag -l "${prefix}[0-9]*")
  echo "$best"
}

json() {
  local component="${1:?usage: release-notes.sh json <component> <version>}"
  local version="${2:?usage: release-notes.sh json <component> <version>}"
  check_component "$component"
  check_semver "$version"
  ensure_history

  local prefix prev range
  prefix="$(prefix_of "$component")"
  prev="$(previous_version "$component" "$version")"
  if [ -n "$prev" ]; then range="$prefix$prev..HEAD"; else range="HEAD"; fi
  local -a paths=()
  local p
  while IFS= read -r p; do paths+=("$p"); done < <(paths_of "$component")

  # The day of the commit the notes are built at, in UTC, so a rerun on the
  # same tag writes the same bytes.
  local date
  date="$(TZ=UTC git log -1 --format=%cd --date=format-local:%Y-%m-%d HEAD)"

  # One full message per commit, oldest first, each ended by a NUL, which a
  # commit message cannot hold. The first line is the subject.
  git log -z --no-merges --reverse --format='%B' "$range" -- "${paths[@]}" \
    | jq -R -s \
      --arg component "$component" \
      --arg version "$version" \
      --arg previous "$prev" \
      --arg prefix "$prefix" \
      --arg date "$date" '
      def clean: gsub("[[:cntrl:]]"; "") | gsub("^\\s+|\\s+$"; "");
      # The text of the first BREAKING CHANGE footer that has some, or null.
      # The footer line must start at column 0. The text runs on over the
      # lines below it, joined with single spaces, up to a blank line or a
      # run of trailers ("Token: " or "Token #" lines up to the next blank
      # line or the end), so a prose line such as "Note: x" does not end it.
      def footer:
        split("\n")[1:] as $raw
        | ($raw | map(clean)) as $l
        | ($l | length) as $n
        | def trailer: test("^(BREAKING[ -]CHANGE|[A-Za-z0-9-]+)(: | #)");
          def para_end($j): [range($j; $n) | select($l[.] == "")] | first // $n;
          [range(0; $n) | select($raw[.] | test("^BREAKING[ -]CHANGE: "))
           | . as $s
           | (first(range($s + 1; $n)
                    | select($l[.] == "" or (. as $j | all($l[$j:para_end($j)][]; trailer))))
              // $n) as $e
           | $l[$s:$e] | .[0] |= sub("^BREAKING[ -]CHANGE: "; "") | join(" ") | clean
           | select(length > 0)]
          | first;
      def item:
        . as $body
        | (split("\n")[0] | clean) as $subject
        | ($subject
           | capture("^(?<type>[a-z]+)(\\((?<scope>[^)]*)\\))?(?<bang>!)?: (?<text>.+)$")
          ) // {type: "other", scope: null, bang: null, text: $subject}
        | . as $c
        | ($body | footer) as $footer
        | ($c.text | capture("^(?<t>.*?) *\\(#(?<pr>[0-9]+)\\)$") // {t: $c.text, pr: null}) as $p
        | ($footer != null or $c.bang == "!") as $breaking
        | {
            type: $c.type,
            scope: (if ($c.scope // "") == "" then null else $c.scope end),
            text: (if $breaking and $footer != null then $footer else $p.t end),
            pr: (if $p.pr == null then null else ($p.pr | tonumber) end),
            breaking: $breaking
          };
      # The section of an item. A breaking commit shows only in the first.
      def section_name:
        if .breaking then "breaking"
        else {feat: "features", fix: "fixes", perf: "performance"}[.type] // "other" end;
      # The release commit of the version being built is left out: it only
      # sets the version and the changelog (scripts/release-prepare.sh).
      [split("\u0000")[] | ltrimstr("\n") | select(test("\\S")) | item
        | select((.type == "chore" and .scope == "release"
                  and (.text | test("^(playbook|nix flake) [0-9]+\\.[0-9]+\\.[0-9]+$"))) | not)] as $items
      | [
          {name: "breaking", title: "Breaking changes"},
          {name: "features", title: "Features"},
          {name: "fixes", title: "Fixes"},
          {name: "performance", title: "Performance"},
          {name: "other", title: "Other changes"}
        ]
      | map(. as $s | $s + {items: [$items[] | select(section_name == $s.name)]})
      | {
          format: 1,
          component: $component,
          version: $version,
          tag: ($prefix + $version),
          previous: (if $previous == "" then null else $previous end),
          previous_tag: (if $previous == "" then null else $prefix + $previous end),
          date: $date,
          sections: map(select(.items | length > 0))
        }
    '
}

markdown() {
  local file
  file="$(from_caller "${1:?usage: release-notes.sh markdown <file>}")"
  [ -f "$file" ] || fail "$file is missing"
  jq -r --arg repo "$REPO" '
    # Markup characters show as they are, so a subject cannot add link or
    # image markup, HTML, emphasis or a mention to the release page. GitHub
    # still links a bare URL. When every backtick stands alone and they
    # pair up, each pair is a code span, which shows its text as it is, so
    # only the text outside them is escaped. A run of two or more backticks
    # or an odd one escapes the whole text, since a code span closes only on
    # a run of the same length.
    def esc_all: gsub("(?<c>[\\\\`*_\\[\\]<>|~&#@])"; "\\\(.c)");
    def esc:
      split("`") as $parts
      | if ($parts | length) % 2 == 1 and (test("``") | not) then
          [range(0; $parts | length) as $i
           | if $i % 2 == 0 then $parts[$i] | esc_all else $parts[$i] end]
          | join("`")
        else esc_all end;
    def link: if .pr == null then "" else " ([#\(.pr)](https://github.com/\($repo)/pull/\(.pr)))" end;
    def line($name):
      if $name == "other" then
        "- \(.type)\(if .scope then "(\(.scope | esc))" else "" end): \(.text | esc)\(link)"
      else
        "- \(if .scope then "**\(.scope | esc)**: " else "" end)\(.text | esc)\(link)"
      end;
    (if (.sections | length) == 0 then
       "## Changes\n\nNo commits since \(.previous_tag // "the first commit") touch this component.\n"
     else
       (.sections[] | . as $s | "## \($s.title)\n\n" + ([$s.items[] | line($s.name)] | join("\n")) + "\n")
     end),
    (if .previous_tag then
       "**Full changelog**: https://github.com/\($repo)/compare/\(.previous_tag)...\(.tag)"
     else empty end)
  ' "$file"
}

body() {
  local component="${1:?usage: release-notes.sh body <component> <version> <json-out>}"
  local version="${2:?usage: release-notes.sh body <component> <version> <json-out>}"
  local out
  out="$(from_caller "${3:?usage: release-notes.sh body <component> <version> <json-out>}")"
  json "$component" "$version" > "$out"
  markdown "$out"
}

case "${1:-}" in
  json) json "${2:-}" "${3:-}" ;;
  markdown) markdown "${2:-}" ;;
  body) body "${2:-}" "${3:-}" "${4:-}" ;;
  *) echo "usage: release-notes.sh json <component> <version> | markdown <file> | body <component> <version> <json-out>" >&2; exit 2 ;;
esac
