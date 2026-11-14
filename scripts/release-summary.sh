#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# release-summary.sh: a plain-English summary on top of the release notes,
# written by a model.
#
#   release-summary.sh apply <tag> <version>
#     The release workflow step. Downloads release-notes.json and the linux
#     binary from the release, checks both attestations, runs "body" with that
#     binary and puts the result on the release as its notes. A missing key, or
#     a "body" failure, leaves the notes as they are and exits 0. A failed
#     download, attestation check or release edit exits 1. It reads GH_TOKEN,
#     GH_REPO, SOURCE_REF and GITHUB_STEP_SUMMARY, and writes one line there.
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
SELF="$0"
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
PLAYBOOK_BIN="${PLAYBOOK_BIN:-playbook}"
MAX_ATTEMPTS=2

EXIT_FAIL=1
EXIT_SKIP=9
EXIT_USAGE=2
EXIT_OK=0
HTTP_OK=200
STOP_END_TURN=end_turn
LIST_HEADING="### All changes"
FIRST_HEADING="### Summary"
ALLOWED_HEADINGS=("### Summary" "### Breaking changes" "### New features" "### Fixes")

NOTES_ASSET=release-notes.json
BINARY_ASSET_PREFIX=playbook-
BINARY_ASSET_TARGET=x86_64-unknown-linux-musl
SIGNER_WORKFLOW=.github/workflows/release.yml
STEP_DOWNLOAD="download"
STEP_ATTESTATION="attestation check"
STEP_EDIT="release edit"
SUMMARY_WRITTEN="Release summary written."
SUMMARY_SKIPPED="Release summary skipped: the release-notes environment has no ANTHROPIC_API_KEY secret."
SUMMARY_FAILED="Release summary failed; the body keeps the commit list."

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
  local file="$1" first heading allowed ok bare url
  local found=0
  if ! grep -q '[^[:space:]]' "$file"; then
    echo "the answer is blank"
    return 1
  fi

  first="$(grep -m1 '[^[:space:]]' "$file")"
  if [ "$first" != "$FIRST_HEADING" ]; then
    echo "the first line must be '$FIRST_HEADING'"
    found=1
  fi

  while IFS= read -r heading; do
    ok=0
    for allowed in "${ALLOWED_HEADINGS[@]}"; do
      [ "$heading" = "$allowed" ] && ok=1
    done
    if [ "$ok" -eq 0 ]; then
      echo "the heading '$heading' is not allowed; use only: ${ALLOWED_HEADINGS[*]}"
      found=1
    fi
  done < <(grep -E '^#{1,6}( |$)' "$file")

  # Inline code spans may hold <, ![ and @, so they are cut out first.
  bare="$(sed 's/`[^`]*`//g' "$file")"
  if printf '%s\n' "$bare" | grep -qE '<[A-Za-z/!]'; then
    echo "raw HTML is not allowed outside code spans"
    found=1
  fi
  if printf '%s\n' "$bare" | grep -qF '!['; then
    echo "images are not allowed"
    found=1
  fi
  if printf '%s\n' "$bare" | grep -qF '@'; then
    echo "an @ mention is not allowed outside code spans"
    found=1
  fi

  while IFS= read -r url; do
    case "$(printf '%s' "$url" | tr '[:upper:]' '[:lower:]')" in
      "https://github.com/$(printf '%s' "$REPO" | tr '[:upper:]' '[:lower:]')/"*) ;;
      *) echo "the link '$url' does not point into https://github.com/$REPO/"; found=1 ;;
    esac
  done < <(grep -oE 'https?://[^][:space:])>"]+' "$file")

  if ! env -u ANTHROPIC_API_KEY "$PLAYBOOK_BIN" prose check --strict "$file"; then
    found=1
  fi
  return "$found"
}

# Adds the first answer and the findings to $WORK/request.json, so the model
# writes the whole section again.
build_retry_request() {
  local findings="$1" answer
  answer="$(cat "$WORK/summary.md")"
  jq --arg answer "$answer" --arg findings "$findings" \
    '.messages += [{role: "assistant", content: $answer},
      {role: "user", content: ("The section broke these rules:\n" + $findings
        + "\nWrite the whole section again and fix every one.")}]' \
    "$WORK/request.json" > "$WORK/request.next.json"
  mv "$WORK/request.next.json" "$WORK/request.json"
}

body() {
  local notes findings attempt
  notes="$(from_caller "${1:?usage: release-summary.sh body <release-notes.json>}")"
  if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "::notice::release summary skipped: ANTHROPIC_API_KEY is empty." >&2
    exit "$EXIT_SKIP"
  fi
  [ -f "$notes" ] || die "$notes is missing."

  gather_pulls "$notes"
  build_request "$notes"
  attempt=1
  while :; do
    call_api
    findings="$(check_summary "$WORK/summary.md")" && break
    [ "$attempt" -lt "$MAX_ATTEMPTS" ] || die "the summary still breaks the rules after $attempt attempts: $(echo "$findings" | tr '\n' ' ')"
    build_retry_request "$findings"
    attempt=$((attempt + 1))
  done

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

# Appends $1 to the job's step summary when there is one.
summary_line() {
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "$1" >> "$GITHUB_STEP_SUMMARY"
  fi
}

# A failed step of apply: the error on stderr and in the step summary, exit 1.
step_failed() {
  local line="::error::release summary: $1 failed, so the release notes are unchanged."
  echo "$line" >&2
  summary_line "$line"
  exit "$EXIT_FAIL"
}

apply() {
  local tag version dir binary asset rc=0
  tag="${1:?usage: release-summary.sh apply <tag> <version>}"
  version="${2:?usage: release-summary.sh apply <tag> <version>}"
  if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    summary_line "$SUMMARY_SKIPPED"
    exit "$EXIT_OK"
  fi

  dir="$WORK/release"
  mkdir -p "$dir"
  asset="${BINARY_ASSET_PREFIX}${version}-${BINARY_ASSET_TARGET}"
  binary="$dir/$asset"
  gh release download "$tag" --dir "$dir" --pattern "$NOTES_ASSET" --pattern "$asset" \
    || step_failed "$STEP_DOWNLOAD"
  for file in "$dir/$NOTES_ASSET" "$binary"; do
    gh attestation verify "$file" --repo "$GH_REPO" --source-ref "$SOURCE_REF" \
      --signer-workflow "$GH_REPO/$SIGNER_WORKFLOW" > /dev/null || step_failed "$STEP_ATTESTATION"
  done
  chmod +x "$binary"

  PLAYBOOK_BIN="$binary" bash "$(from_caller "$SELF")" body "$dir/$NOTES_ASSET" > "$dir/body.md" || rc=$?
  case "$rc" in
    "$EXIT_OK")
      gh release edit "$tag" --notes-file "$dir/body.md" || step_failed "$STEP_EDIT"
      summary_line "$SUMMARY_WRITTEN" ;;
    "$EXIT_SKIP")
      summary_line "$SUMMARY_SKIPPED" ;;
    *)
      echo "::warning::release summary: the summary step failed (exit $rc), so the release notes are unchanged." >&2
      summary_line "$SUMMARY_FAILED" ;;
  esac
}

case "${1:-}" in
  apply) apply "${2:-}" "${3:-}" ;;
  body) body "${2:-}" ;;
  *) echo "usage: release-summary.sh apply <tag> <version> | body <release-notes.json>" >&2; exit "$EXIT_USAGE" ;;
esac
