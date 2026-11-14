#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# release-summary.test.sh: offline tests for release-summary.sh. Fake curl and
# gh on PATH answer from canned files and record what they were given. Every
# case runs in its own temp dir, with the variables the script reads cleared.
#
# Usage: release-summary.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."

ROOT="$PWD"
SCRIPT="$ROOT/scripts/release-summary.sh"
PROMPT="$ROOT/scripts/release-summary.prompt.md"
NOTES_SCRIPT="$ROOT/scripts/release-notes.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir "$TMP/fakes"

KEY=sk-test-not-real
REPO_NAME=AmplifiedEngineer/playbook
EXIT_SKIP=9
EXIT_FAIL=1
HTTP_OK=200
HTTP_SERVER_ERROR=500
HTTP_BAD_REQUEST=400
CURL_NETWORK_RC=6
WANT_MODEL=claude-opus-5-5
WANT_MAX_TOKENS=16000
WANT_EFFORT=high
WANT_VERSION_HEADER='anthropic-version: 2026-10-21'
WANT_BETA_HEADER='anthropic-beta: server-side-fallback-2026-07-01'

fail() { echo "FAIL $*" >&2; exit 1; }

# The fake curl records one line per call, the header lines (the script passes
# them as an @file) and the request body. It answers from FAKE_CURL_* values.
cat > "$TMP/fakes/curl" <<'FAKE'
#!/usr/bin/env bash
echo "$*" >> "$FAKE_DIR/calls"
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    -H)
      case "$2" in
        @*) cat "${2#@}" >> "$FAKE_DIR/headers" ;;
        *) echo "$2" >> "$FAKE_DIR/headers" ;;
      esac
      shift ;;
    --data-binary|-d) cp "${2#@}" "$FAKE_DIR/request.json"; shift ;;
  esac
  shift
done
rc="${FAKE_CURL_RC:-0}"
n="$(wc -l < "$FAKE_DIR/calls" | tr -d ' ')"
cp "$FAKE_DIR/request.json" "$FAKE_DIR/request.$n.json"
resp="$FAKE_CURL_RESPONSE"
if [ -f "$FAKE_DIR/response.$n.json" ]; then resp="$FAKE_DIR/response.$n.json"; fi
if [ "$rc" -eq 0 ] && [ -n "$out" ]; then
  cp "$resp" "$out"
fi
printf '%s' "${FAKE_CURL_STATUS:-200}"
exit "$rc"
FAKE

# The fake gh records every call. It answers `api repos/<repo>/pulls/<n> --jq
# <filter>` by running the filter on $FAKE_DIR/pulls/<n>.json, and fails when
# that file is missing. `release download` copies the notes fixture and the
# fake playbook as the binary into --dir; `attestation verify` fails for the
# file named in FAKE_GH_ATTEST_FAIL; `release edit` keeps a copy of its notes
# file. FAKE_GH_DOWNLOAD_RC and FAKE_GH_EDIT_RC force a failure.
cat > "$TMP/fakes/gh" <<'FAKE'
#!/usr/bin/env bash
echo "$*" >> "$FAKE_DIR/gh-calls"
case "${1:-} ${2:-}" in
  "api repos/"*)
    f="$FAKE_DIR/pulls/${2##*/}.json"
    if [ -f "$f" ]; then jq -c "$4" "$f"; exit 0; fi
    echo "gh: Not Found (HTTP 404)" >&2
    exit 1 ;;
  "release download")
    [ "${FAKE_GH_DOWNLOAD_RC:-0}" -eq 0 ] || exit "$FAKE_GH_DOWNLOAD_RC"
    dir=""; patterns=()
    shift 3
    while [ $# -gt 0 ]; do
      case "$1" in
        --dir) dir="$2"; shift ;;
        --pattern) patterns+=("$2"); shift ;;
      esac
      shift
    done
    for p in "${patterns[@]}"; do
      case "$p" in
        release-notes.json) cp "$FAKE_DIR/release-notes.json" "$dir/$p" ;;
        *) cp "$FAKE_DIR/bin/playbook" "$dir/$p" ;;
      esac
    done
    exit 0 ;;
  "attestation verify")
    [ "$(basename "$3")" != "${FAKE_GH_ATTEST_FAIL:-}" ] || exit 1
    exit 0 ;;
  "release edit")
    [ "${FAKE_GH_EDIT_RC:-0}" -eq 0 ] || exit "$FAKE_GH_EDIT_RC"
    while [ $# -gt 0 ]; do
      if [ "$1" = --notes-file ]; then cp "$2" "$FAKE_DIR/edited-notes"; fi
      shift
    done
    exit 0 ;;
esac
exit 1
FAKE
# The fake playbook prints a finding and exits 1 when its input file holds the
# line PROSE-MARKER. It records whether the API key reached its environment.
cat > "$TMP/fakes/playbook" <<'FAKE'
#!/usr/bin/env bash
echo "$*" >> "$FAKE_DIR/playbook-calls"
env | grep '^ANTHROPIC_API_KEY=' >> "$FAKE_DIR/playbook-env" || true
file="${!#}"
if grep -qx 'PROSE-MARKER' "$file"; then
  echo "$file:1: em or en dash"
  exit 1
fi
exit 0
FAKE
chmod +x "$TMP/fakes/curl" "$TMP/fakes/gh" "$TMP/fakes/playbook"

CASE_N=0
CASE=""
RC=0

# new_case: a fresh dir with the fakes, the fixture notes and two readable PRs.
new_case() {
  CASE_N=$((CASE_N + 1))
  CASE="$TMP/case-$CASE_N"
  mkdir -p "$CASE/bin" "$CASE/pulls"
  cp "$TMP/fakes/curl" "$TMP/fakes/gh" "$TMP/fakes/playbook" "$CASE/bin/"
  cat > "$CASE/release-notes.json" <<'JSON'
{"format":1,"component":"playbook","version":"9.9.9","tag":"v9.9.9","previous":"9.9.8","previous_tag":"v9.9.8","date":"2026-01-02","sections":[
{"name":"breaking","title":"Breaking changes","items":[{"type":"feat","scope":null,"text":"Drop the old flag","pr":11,"breaking":true}]},
{"name":"fixes","title":"Fixes","items":[{"type":"fix","scope":"cli","text":"Fix a crash on start","pr":12,"breaking":false},{"type":"fix","scope":null,"text":"Tidy the docs","pr":null,"breaking":false}]}]}
JSON
  echo '{"number":11,"state":"closed","title":"Drop the old flag","body":"The --old flag is gone. Use --new."}' > "$CASE/pulls/11.json"
  echo '{"number":12,"state":"closed","title":"Fix a crash on start","body":"Start no longer crashes."}' > "$CASE/pulls/12.json"
}

# ok_response <text>: a good answer, with a thinking block before the text.
ok_response() {
  jq -n --arg t "$1" '{type: "message", role: "assistant", stop_reason: "end_turn",
    content: [{type: "thinking", thinking: "hmm"}, {type: "text", text: $t}]}' > "$CASE/response.json"
}

# answer_n <n> <text>: the answer the fake curl serves on call n.
answer_n() {
  jq -n --arg t "$2" '{type: "message", role: "assistant", stop_reason: "end_turn",
    content: [{type: "text", text: $t}]}' > "$CASE/response.$1.json"
}

# run_body [VAR=value...]: runs `body` with the variables the script reads
# cleared, then the defaults, then the case's own values (the last one wins).
run_body() {
  RC=0
  env -u ANTHROPIC_API_KEY -u GITHUB_REPOSITORY -u GITHUB_STEP_SUMMARY -u PLAYBOOK_BIN \
    PATH="$CASE/bin:$PATH" FAKE_DIR="$CASE" FAKE_CURL_RESPONSE="$CASE/response.json" \
    GITHUB_REPOSITORY="$REPO_NAME" PLAYBOOK_BIN="$CASE/bin/playbook" ANTHROPIC_API_KEY="$KEY" \
    "$@" \
    bash "$SCRIPT" body "$CASE/release-notes.json" >"$CASE/stdout" 2>"$CASE/stderr" || RC=$?
  if grep -qF "$KEY" "$CASE/stdout" "$CASE/stderr"; then fail "case $CASE_N: the API key was printed"; fi
}

want_failure() {
  [ "$RC" -eq "$EXIT_FAIL" ] || fail "$1: want exit $EXIT_FAIL, got $RC: $(cat "$CASE/stderr")"
  [ ! -s "$CASE/stdout" ] || fail "$1: stdout is not empty"
}
curl_calls() { if [ -f "$CASE/calls" ]; then wc -l < "$CASE/calls" | tr -d ' '; else echo 0; fi; }

# --- skip and failure cases ---

# 1. No key: exit 9, nothing on stdout, curl never called.
new_case; ok_response "### Summary

Text."
run_body ANTHROPIC_API_KEY=
[ "$RC" -eq "$EXIT_SKIP" ] || fail "no key: want exit $EXIT_SKIP, got $RC"
[ ! -s "$CASE/stdout" ] || fail "no key: stdout is not empty"
[ "$(curl_calls)" -eq 0 ] || fail "no key: curl was called"

# 2. curl fails on the network: exit 1, one call.
new_case; ok_response "### Summary"
run_body FAKE_CURL_RC="$CURL_NETWORK_RC" FAKE_CURL_STATUS=000
want_failure "curl exit 6"
[ "$(curl_calls)" -eq 1 ] || fail "curl exit 6: want 1 call, got $(curl_calls)"

# 3. Status 500 with curl exit 0: exit 1, one call.
new_case; echo '{"type":"error"}' > "$CASE/response.json"
run_body FAKE_CURL_STATUS="$HTTP_SERVER_ERROR"
want_failure "status 500"
[ "$(curl_calls)" -eq 1 ] || fail "status 500: want 1 call, got $(curl_calls)"

# 4. Status 400 with an error body: exit 1.
new_case; echo '{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}' > "$CASE/response.json"
run_body FAKE_CURL_STATUS="$HTTP_BAD_REQUEST"
want_failure "status 400"

# 5. Status 200 with a body that is not JSON: exit 1.
new_case; echo 'this is not json' > "$CASE/response.json"
run_body FAKE_CURL_STATUS="$HTTP_OK"
want_failure "not json"

# 6. Stop reasons other than end_turn, and a whitespace-only text: exit 1.
for stop in refusal max_tokens; do
  new_case
  jq -n --arg s "$stop" '{stop_reason: $s, content: [{type: "text", text: "### Summary\n\nText."}]}' > "$CASE/response.json"
  run_body
  want_failure "stop_reason $stop"
done
new_case; ok_response "   
 "
run_body
want_failure "blank text"

# --- success ---

# 7. A good answer: the answer, a blank line, ### All changes, the list with
# headings one level down.
ANSWER='### Summary

This release drops a flag and fixes a crash.

### Breaking changes

- The `--old` flag is gone. What to do: use `--new`.'
new_case; ok_response "$ANSWER"
run_body
[ "$RC" -eq 0 ] || fail "good answer: exit $RC: $(cat "$CASE/stderr")"
{
  printf '%s\n' "$ANSWER"
  cat <<'EXPECTED'

### All changes

#### Breaking changes

- Drop the old flag ([#11](https://github.com/AmplifiedEngineer/playbook/pull/11))

#### Fixes

- **cli**: Fix a crash on start ([#12](https://github.com/AmplifiedEngineer/playbook/pull/12))
- Tidy the docs

**Full changelog**: https://github.com/AmplifiedEngineer/playbook/compare/v9.9.8...v9.9.9
EXPECTED
} > "$CASE/expected"
cmp -s "$CASE/stdout" "$CASE/expected" || fail "good answer: stdout differs: $(diff "$CASE/expected" "$CASE/stdout")"
# With the headings put back, the list is release-notes.sh markdown byte for byte.
GITHUB_REPOSITORY="$REPO_NAME" bash "$NOTES_SCRIPT" markdown "$CASE/release-notes.json" > "$CASE/list"
sed -e '1,/^### All changes$/d' "$CASE/stdout" | sed -e '1d' -e 's/^#### /## /' > "$CASE/list.back"
cmp -s "$CASE/list" "$CASE/list.back" || fail "good answer: the list changed: $(diff "$CASE/list" "$CASE/list.back")"

# 8. The request: settings, headers, system prompt and the PRs.
req="$CASE/request.json"
jq -e --arg model "$WANT_MODEL" --arg effort "$WANT_EFFORT" --argjson max "$WANT_MAX_TOKENS" '
  .model == $model and .fallbacks == "default" and .output_config.effort == $effort
  and .max_tokens == $max
  and (has("temperature") or has("top_p") or has("top_k") or has("thinking") | not)
  and (.messages | length) == 1 and .messages[0].role == "user"' "$req" >/dev/null \
  || fail "request: wrong settings: $(cat "$req")"
jq -e --rawfile p "$PROMPT" '.system == $p' "$req" >/dev/null || fail "request: system is not the prompt file"
jq -r '.messages[0].content' "$req" > "$CASE/user.txt"
head -n 1 "$CASE/user.txt" | grep -qx '<release>' || fail "request: user message does not open with <release>"
tail -n 1 "$CASE/user.txt" | grep -qx '</release>' || fail "request: user message does not end with </release>"
sed -e '1d' -e '$d' "$CASE/user.txt" > "$CASE/release.json"
jq -e --arg repo "$REPO_NAME" '
  .repo == $repo and .version == "9.9.9" and .previous_tag == "v9.9.8"
  and .notes.format == 1
  and (.pulls | map(.number) == [11, 12])
  and .pulls[0].title == "Drop the old flag" and .pulls[0].body == "The --old flag is gone. Use --new."
  and .pulls[1].title == "Fix a crash on start"' "$CASE/release.json" >/dev/null \
  || fail "request: wrong release data: $(cat "$CASE/release.json")"
grep -qxF "$WANT_VERSION_HEADER" "$CASE/headers" || fail "request: no $WANT_VERSION_HEADER"
grep -qxF "$WANT_BETA_HEADER" "$CASE/headers" || fail "request: no $WANT_BETA_HEADER"
grep -qxF "x-api-key: $KEY" "$CASE/headers" || fail "request: no x-api-key header"
case "$(cat "$CASE/calls")" in *"--max-time"*"--retry"*|*"--retry"*"--max-time"*) ;; *) fail "request: curl has no timeout and retry" ;; esac

# 9. release-notes.json is read only.
new_case; ok_response "$ANSWER"
before="$(shasum -a 256 "$CASE/release-notes.json")"
run_body
[ "$before" = "$(shasum -a 256 "$CASE/release-notes.json")" ] || fail "release-notes.json changed"

# --- pull request edge cases ---

# 10. One PR gh cannot read: warns naming it, still succeeds, sends the other.
new_case; ok_response "$ANSWER"; rm "$CASE/pulls/12.json"
run_body
[ "$RC" -eq 0 ] || fail "unreadable PR: exit $RC"
grep -q '#12' "$CASE/stderr" || fail "unreadable PR: stderr does not name #12: $(cat "$CASE/stderr")"
sed -e '1d' -e '$d' <(jq -r '.messages[0].content' "$CASE/request.json") \
  | jq -e '.pulls | map(.number) == [11]' >/dev/null || fail "unreadable PR: pulls is wrong"

# 11. No PR readable: still succeeds, pulls is [] and notes has the commit list.
new_case; ok_response "$ANSWER"; rm "$CASE/pulls/11.json" "$CASE/pulls/12.json"
run_body
[ "$RC" -eq 0 ] || fail "no PRs: exit $RC"
sed -e '1d' -e '$d' <(jq -r '.messages[0].content' "$CASE/request.json") \
  | jq -e '.pulls == [] and (.notes.sections | length) == 2' >/dev/null || fail "no PRs: wrong release data"

# 12. A PR with a null body: kept, with an empty body.
new_case; ok_response "$ANSWER"
echo '{"number":11,"state":"closed","title":"Drop the old flag","body":null}' > "$CASE/pulls/11.json"
run_body
[ "$RC" -eq 0 ] || fail "null body: exit $RC"
sed -e '1d' -e '$d' <(jq -r '.messages[0].content' "$CASE/request.json") \
  | jq -e '.pulls[0].number == 11 and .pulls[0].body == ""' >/dev/null || fail "null body: wrong pulls"

# --- the checks and the one retry ---

MAX_ATTEMPTS=2
GOOD='### Summary

This release drops a flag and fixes a crash.'
FINDING_PROSE='em or en dash'
EXAMPLE_PREFIX='https://github.com/AmplifiedEngineer/playbook'

# retry_row <name> <first answer>: the first answer breaks one rule, the second
# is clean. The script must call curl twice and print the second answer.
retry_row() {
  new_case; answer_n 1 "$2"; answer_n 2 "$GOOD"; ok_response "$GOOD"
  run_body
  [ "$RC" -eq 0 ] || fail "$1: want exit 0, got $RC: $(cat "$CASE/stderr")"
  [ "$(curl_calls)" -eq "$MAX_ATTEMPTS" ] || fail "$1: want $MAX_ATTEMPTS calls, got $(curl_calls)"
  printf '%s\n' "$GOOD" > "$CASE/want-head"
  head -n "$(wc -l < "$CASE/want-head" | tr -d ' ')" "$CASE/stdout" | cmp -s - "$CASE/want-head" \
    || fail "$1: the body does not start with the second answer"
}

retry_row "first line" "Intro.

### Summary

Text."
retry_row "other heading" "### Summary

Text.

## Notes

More."
retry_row "html" "### Summary

A raw <img src=x> tag."
retry_row "image" "### Summary

![x]($EXAMPLE_PREFIX/a.png)"
retry_row "mention" "### Summary

Thanks @someone."
retry_row "foreign link" "### Summary

See https://example.com for more."
retry_row "prose" "### Summary

PROSE-MARKER"

# The second request carries the first answer as an assistant turn and the
# findings in the last user turn.
new_case; answer_n 1 "### Summary

PROSE-MARKER"; answer_n 2 "$GOOD"; ok_response "$GOOD"
run_body
jq -e '(.messages | length) == 3 and .messages[0].role == "user"
  and .messages[1].role == "assistant" and .messages[1].content == "### Summary\n\nPROSE-MARKER"
  and .messages[2].role == "user"' "$CASE/request.2.json" >/dev/null \
  || fail "retry request: wrong messages: $(jq -c .messages "$CASE/request.2.json")"
jq -e --arg f "$FINDING_PROSE" '.messages[2].content | contains($f)' "$CASE/request.2.json" >/dev/null \
  || fail "retry request: the last user turn does not list the finding"
jq -e '(.messages | length) == 1' "$CASE/request.1.json" >/dev/null || fail "first request: want one message"

# Both answers fail: exit 1 after MAX_ATTEMPTS calls.
new_case; answer_n 1 "### Summary

PROSE-MARKER"; answer_n 2 "### Summary

PROSE-MARKER"; ok_response "$GOOD"
run_body
want_failure "both answers fail"
[ "$(curl_calls)" -eq "$MAX_ATTEMPTS" ] || fail "both fail: want $MAX_ATTEMPTS calls, got $(curl_calls)"

# Boundaries that must pass on the first answer.
pass_row() {
  new_case; ok_response "$2"
  run_body
  [ "$RC" -eq 0 ] || fail "$1: want exit 0, got $RC: $(cat "$CASE/stderr")"
  [ "$(curl_calls)" -eq 1 ] || fail "$1: want 1 call, got $(curl_calls)"
}
pass_row "code span html" '### Summary

Run `--account <key>` first.'
pass_row "code span mention" '### Summary

Mail `a@b` to us.'
pass_row "link case" '### Summary

See https://GitHub.com/AmplifiedEngineer/playbook/pull/1 for it.'
new_case; answer_n 1 "### Summary

See $EXAMPLE_PREFIX-evil/x for it."; answer_n 2 "### Summary

See $EXAMPLE_PREFIX-evil/x for it."; ok_response "$GOOD"
run_body
want_failure "prefix ends at the slash"

# The fake playbook never saw the API key.
new_case; answer_n 1 "### Summary

PROSE-MARKER"; answer_n 2 "$GOOD"; ok_response "$GOOD"
run_body
[ -f "$CASE/playbook-calls" ] || fail "playbook was not called"
grep -qx 'prose check --strict .*' "$CASE/playbook-calls" || fail "playbook: wrong arguments: $(cat "$CASE/playbook-calls")"
[ ! -s "$CASE/playbook-env" ] || fail "playbook saw ANTHROPIC_API_KEY"

# Contract of the real binary, when one is installed: --strict exits 1 with a
# path:line finding for an em dash and 0 for clean text.
REAL_PLAYBOOK="$(command -v playbook || true)"
if [ -n "$REAL_PLAYBOOK" ]; then
  printf 'one \xe2\x80\x94 two\n' > "$TMP/dash.md"
  printf 'one, two.\n' > "$TMP/clean.md"
  RC=0; out="$("$REAL_PLAYBOOK" prose check --strict "$TMP/dash.md" 2>/dev/null)" || RC=$?
  [ "$RC" -eq 1 ] || fail "real playbook: em dash file: want exit 1, got $RC"
  printf '%s\n' "$out" | grep -qE "^$TMP/dash.md:[0-9]+: " || fail "real playbook: no path:line finding: $out"
  RC=0; "$REAL_PLAYBOOK" prose check --strict "$TMP/clean.md" >/dev/null 2>&1 || RC=$?
  [ "$RC" -eq 0 ] || fail "real playbook: clean file: want exit 0, got $RC"
else
  echo "skip: no real playbook on PATH, the prose check contract is not tested"
fi

# --- apply ---

TAG=v9.9.9
VERSION=9.9.9
SOURCE_REF=refs/heads/main
BINARY_ASSET="playbook-$VERSION-x86_64-unknown-linux-musl"
SUMMARY_WRITTEN="Release summary written."
SUMMARY_SKIPPED="Release summary skipped: the release-notes environment has no ANTHROPIC_API_KEY secret."
SUMMARY_FAILED="Release summary failed; the body keeps the commit list."

# run_apply [VAR=value...]: runs `apply` with a step summary file and the
# variables the workflow step sets.
run_apply() {
  RC=0
  env -u ANTHROPIC_API_KEY -u GITHUB_REPOSITORY -u GITHUB_STEP_SUMMARY -u PLAYBOOK_BIN \
    PATH="$CASE/bin:$PATH" FAKE_DIR="$CASE" FAKE_CURL_RESPONSE="$CASE/response.json" \
    GITHUB_REPOSITORY="$REPO_NAME" GH_REPO="$REPO_NAME" SOURCE_REF="$SOURCE_REF" \
    ANTHROPIC_API_KEY="$KEY" GITHUB_STEP_SUMMARY="$CASE/step-summary" \
    "$@" \
    bash "$SCRIPT" apply "$TAG" "$VERSION" >"$CASE/stdout" 2>"$CASE/stderr" || RC=$?
  if grep -qF "$KEY" "$CASE/stdout" "$CASE/stderr" "$CASE/step-summary" 2>/dev/null; then fail "apply: the API key was printed"; fi
}
edit_calls() { if [ -f "$CASE/gh-calls" ]; then grep -c '^release edit ' "$CASE/gh-calls" || true; else echo 0; fi; }
want_apply_failure() {
  [ "$RC" -eq "$EXIT_FAIL" ] || fail "$1: want exit $EXIT_FAIL, got $RC: $(cat "$CASE/stderr")"
  grep -q "^::error::release summary: $2 failed" "$CASE/stderr" || fail "$1: no ::error:: naming '$2': $(cat "$CASE/stderr")"
  grep -q "^::error::release summary: $2 failed" "$CASE/step-summary" || fail "$1: step summary has no '$2' error"
}

# A1. A good answer: the release is edited once with the body, and the step
# summary says so. The expected body is what `body` prints for the same inputs.
new_case; ok_response "$ANSWER"
run_body
cp "$CASE/stdout" "$CASE/want-body"
run_apply
[ "$RC" -eq 0 ] || fail "apply good: exit $RC: $(cat "$CASE/stderr")"
[ "$(edit_calls)" -eq 1 ] || fail "apply good: want 1 release edit, got $(edit_calls)"
grep -qx "release edit $TAG --notes-file .*" "$CASE/gh-calls" || fail "apply good: wrong release edit call: $(cat "$CASE/gh-calls")"
cmp -s "$CASE/edited-notes" "$CASE/want-body" || fail "apply good: the edited notes differ from the body"
grep -qxF "$SUMMARY_WRITTEN" "$CASE/step-summary" || fail "apply good: step summary: $(cat "$CASE/step-summary")"
grep -qF -- "--pattern release-notes.json" "$CASE/gh-calls" || fail "apply good: the notes were not downloaded"
grep -qF -- "--pattern $BINARY_ASSET" "$CASE/gh-calls" || fail "apply good: the binary was not downloaded"
for f in release-notes.json "$BINARY_ASSET"; do
  grep -qE "^attestation verify .*/$f --repo $REPO_NAME --source-ref $SOURCE_REF --signer-workflow $REPO_NAME/.github/workflows/release.yml\$" "$CASE/gh-calls" \
    || fail "apply good: no attestation check for $f: $(cat "$CASE/gh-calls")"
done

# A2. No key: no gh call at all, the skip line, exit 0.
new_case; ok_response "$ANSWER"
run_apply ANTHROPIC_API_KEY=
[ "$RC" -eq 0 ] || fail "apply no key: exit $RC"
[ ! -f "$CASE/gh-calls" ] || fail "apply no key: gh was called"
grep -qxF "$SUMMARY_SKIPPED" "$CASE/step-summary" || fail "apply no key: step summary: $(cat "$CASE/step-summary")"

# A3. body fails (status 500): exit 0, no release edit, a failure line and a warning.
new_case; echo '{"type":"error"}' > "$CASE/response.json"
run_apply FAKE_CURL_STATUS="$HTTP_SERVER_ERROR"
[ "$RC" -eq 0 ] || fail "apply body fails: exit $RC: $(cat "$CASE/stderr")"
[ "$(edit_calls)" -eq 0 ] || fail "apply body fails: release edit was called"
grep -qxF "$SUMMARY_FAILED" "$CASE/step-summary" || fail "apply body fails: step summary: $(cat "$CASE/step-summary")"
grep -q '^::warning::' "$CASE/stderr" || fail "apply body fails: no ::warning:: on stderr"

# A4. The attestation check fails for the binary: exit 1, no curl, no release edit.
new_case; ok_response "$ANSWER"
run_apply FAKE_GH_ATTEST_FAIL="$BINARY_ASSET"
want_apply_failure "apply attestation" "attestation check"
[ "$(curl_calls)" -eq 0 ] || fail "apply attestation: curl was called"
[ "$(edit_calls)" -eq 0 ] || fail "apply attestation: release edit was called"

# A5. The download fails: exit 1 naming the download, nothing else runs.
new_case; ok_response "$ANSWER"
run_apply FAKE_GH_DOWNLOAD_RC=1
want_apply_failure "apply download" "download"
[ "$(curl_calls)" -eq 0 ] || fail "apply download: curl was called"

# A6. The release edit fails: exit 1 naming the edit.
new_case; ok_response "$ANSWER"
run_apply FAKE_GH_EDIT_RC=1
want_apply_failure "apply edit" "release edit"

echo "release-summary.test.sh: all cases passed"
