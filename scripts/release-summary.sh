#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# release-summary.sh: a plain-English summary on top of the release notes,
# written by a model.
#
#   release-summary.sh body <release-notes.json>
#     Prints the full release body: the summary, then "### All changes" and
#     the commit list from release-notes.sh. Exits 0 with the body on stdout.
#     On any failure it prints nothing on stdout and one ::warning:: line on
#     stderr, and exits 1. It exits 9 when ANTHROPIC_API_KEY is empty, since
#     there is nothing to do.
#
# The model, the request settings and the prompt are the constants below and
# scripts/release-summary.prompt.md. The pull requests are read with gh, which
# needs GH_TOKEN. GITHUB_REPOSITORY names the repository for the links. A
# relative file argument is read from the directory it was run in.
set -euo pipefail

CALLER="$PWD"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

MODEL=claude-opus-5-5
ANTHROPIC_VERSION=2026-10-21
FALLBACK_BETA=server-side-fallback-2026-07-01
FALLBACKS=default
EFFORT=high
MAX_TOKENS=16000
REQUEST_TIMEOUT_SECONDS=300
CURL_RETRIES=2
API_URL=https://api.anthropic.com/v1/messages
PROMPT_FILE=scripts/release-summary.prompt.md
NOTES_SCRIPT=scripts/release-notes.sh
REPO="${GITHUB_REPOSITORY:-amplifiedengineer/playbook}"

EXIT_FAIL=1
EXIT_SKIP=9
HTTP_OK=200
STOP_END_TURN=end_turn
LIST_HEADING="### All changes"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# $1 as an absolute path, read against the directory the script was run in.
from_caller() {
  case "$1" in
    /*) echo "$1" ;;
    *) echo "$CALLER/$1" ;;
  esac
}

# One warning line on stderr, then a failed run with nothing on stdout.
die() {
  echo "::warning::release summary: $*" >&2
  exit "$EXIT_FAIL"
}

# Writes $WORK/pulls.ndjson: one {number, title, body} line per pull request
# in the notes. A pull request gh cannot read is left out with a warning.
gather_pulls() {
  local notes="$1" n
  : > "$WORK/pulls.ndjson"
  while IFS= read -r n; do
    if gh api "repos/$REPO/pulls/$n" --jq '{number, title, body: (.body // "")}' \
      >> "$WORK/pulls.ndjson" 2>/dev/null; then
      continue
    fi
    echo "::warning::release summary: could not read pull request #$n, so it is left out." >&2
  done < <(jq -r '[.sections[].items[].pr | select(. != null)] | unique | .[]' "$notes")
}

# Writes $WORK/request.json: the system prompt, the release data in
# <release> tags as the one user message, and the settings.
build_request() {
  local notes="$1"
  {
    echo "<release>"
    jq -n --arg repo "$REPO" --slurpfile notes "$notes" --slurpfile pulls "$WORK/pulls.ndjson" \
      '{repo: $repo, version: $notes[0].version, previous_tag: $notes[0].previous_tag,
        notes: $notes[0], pulls: $pulls}'
    printf '</release>'
  } > "$WORK/user.txt"
  jq -n --rawfile system "$PROMPT_FILE" --rawfile content "$WORK/user.txt" \
    --arg model "$MODEL" --arg effort "$EFFORT" --arg fallbacks "$FALLBACKS" \
    --argjson max_tokens "$MAX_TOKENS" \
    '{model: $model, max_tokens: $max_tokens, system: $system,
      messages: [{role: "user", content: $content}],
      output_config: {effort: $effort}, fallbacks: $fallbacks}' > "$WORK/request.json"
}

# Calls the API once, with no retry of a failed answer. On success it writes
# the answer text to $WORK/summary.md. curl --retry covers only transport
# errors. The key goes in through a header file, never on the command line.
call_api() {
  local status rc=0 stop
  printf 'x-api-key: %s\nanthropic-version: %s\nanthropic-beta: %s\ncontent-type: application/json\n' \
    "$ANTHROPIC_API_KEY" "$ANTHROPIC_VERSION" "$FALLBACK_BETA" > "$WORK/headers"
  status="$(curl -sS --max-time "$REQUEST_TIMEOUT_SECONDS" --retry "$CURL_RETRIES" \
    -H "@$WORK/headers" --data-binary "@$WORK/request.json" \
    -o "$WORK/response.json" -w '%{http_code}' "$API_URL")" || rc=$?
  [ "$rc" -eq 0 ] || die "the API call failed (curl exit $rc)."
  [ "$status" = "$HTTP_OK" ] || die "the API answered HTTP $status."
  jq -e . "$WORK/response.json" > /dev/null 2>&1 || die "the API answer is not JSON."
  stop="$(jq -r 'if type == "object" then .stop_reason // "" else "" end' "$WORK/response.json")"
  [ "$stop" = "$STOP_END_TURN" ] || die "the model stopped with '$stop', not '$STOP_END_TURN'."
  jq -r '[.content | arrays | .[] | objects | select(.type == "text") | .text | strings]
    | join("") | gsub("^\\s+|\\s+$"; "")' "$WORK/response.json" > "$WORK/summary.md"
}

# Prints one finding per line for $1 and returns non-zero when it fails.
check_summary() {
  if ! grep -q '[^[:space:]]' "$1"; then
    echo "the answer is blank"
    return 1
  fi
}

body() {
  local notes findings
  notes="$(from_caller "${1:?usage: release-summary.sh body <release-notes.json>}")"
  if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "::notice::release summary skipped: ANTHROPIC_API_KEY is empty." >&2
    exit "$EXIT_SKIP"
  fi
  [ -f "$notes" ] || die "$notes is missing."

  gather_pulls "$notes"
  build_request "$notes"
  call_api
  findings="$(check_summary "$WORK/summary.md")" || die "$findings"

  GITHUB_REPOSITORY="$REPO" bash "$NOTES_SCRIPT" markdown "$notes" > "$WORK/list.md" \
    || die "could not build the commit list."
  {
    # The command substitution drops the newline jq adds after the text.
    printf '%s\n\n%s\n\n' "$(cat "$WORK/summary.md")" "$LIST_HEADING"
    # The list sits one level below the summary headings.
    sed 's/^## /#### /' "$WORK/list.md"
  } > "$WORK/body.md"
  cat "$WORK/body.md"
}

case "${1:-}" in
  body) body "${2:-}" ;;
  *) echo "usage: release-summary.sh body <release-notes.json>" >&2; exit 2 ;;
esac
