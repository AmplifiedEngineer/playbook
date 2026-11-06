#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# release-gh.test.sh: offline tests for the wait step of release-gh.sh. A fake
# gh on PATH answers `pr view` from a script of lines and records every call.
#
# Usage: release-gh.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."

SCRIPT="$PWD/scripts/release-gh.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir "$TMP/bin"

SHA=0123456789abcdef0123456789abcdef01234567

# The fake gh. `pr view` prints the next line of $TMP/views (the last line
# repeats). Other calls are only recorded.
cat > "$TMP/bin/gh" <<'FAKE'
#!/usr/bin/env bash
echo "$*" >> "$FAKE_DIR/calls"
if [ "$1 $2" = "pr view" ]; then
  n="$(cat "$FAKE_DIR/n" 2>/dev/null || echo 0)"
  n=$((n + 1)); echo "$n" > "$FAKE_DIR/n"
  line="$(sed -n "${n}p" "$FAKE_DIR/views")"
  [ -n "$line" ] || line="$(tail -n 1 "$FAKE_DIR/views")"
  echo "$line"
fi
FAKE
chmod +x "$TMP/bin/gh"

fail() { echo "FAIL $*" >&2; exit 1; }

RC=0
OUT=""
# run <env assignments...> -- <seconds>: lines of $TMP/views are the polls.
run() {
  rm -f "$TMP/calls" "$TMP/n"
  RC=0
  env PATH="$TMP/bin:$PATH" FAKE_DIR="$TMP" RELEASE_GH_POLL=0 "$@" \
    bash "$SCRIPT" wait o/r 7 "${SECS:-5}" >"$TMP/out" 2>&1 || RC=$?
  OUT="$(cat "$TMP/out")"
}
updates() { grep -c '^pr update-branch 7 --repo o/r' "$TMP/calls" || true; }

# 1. Behind, switch on: updates once, then merges.
printf 'OPEN BEHIND \nOPEN BEHIND \nOPEN BLOCKED \nMERGED CLEAN %s\n' "$SHA" > "$TMP/views"
run RELEASE_GH_UPDATE_BRANCH=1 RELEASE_GH_UPDATE_EVERY=1000
[ "$RC" -eq 0 ] || fail "behind: exit $RC: $OUT"
case "$OUT" in *"sha=$SHA"*) ;; *) fail "behind: no sha: $OUT" ;; esac
[ "$(updates)" -eq 1 ] || fail "behind: want 1 update, got $(updates)"

# 2. Behind, interval 0: updates on every behind poll.
printf 'OPEN BEHIND \nOPEN BEHIND \nMERGED CLEAN %s\n' "$SHA" > "$TMP/views"
run RELEASE_GH_UPDATE_BRANCH=1 RELEASE_GH_UPDATE_EVERY=0
[ "$(updates)" -eq 2 ] || fail "interval 0: want 2 updates, got $(updates)"

# 3. Behind, switch off: never updates.
printf 'OPEN BEHIND \nMERGED CLEAN %s\n' "$SHA" > "$TMP/views"
run RELEASE_GH_UPDATE_BRANCH=
[ "$RC" -eq 0 ] || fail "off: exit $RC: $OUT"
[ "$(updates)" -eq 0 ] || fail "off: updated"

# 4. Not behind: never updates.
printf 'OPEN CLEAN \nMERGED CLEAN %s\n' "$SHA" > "$TMP/views"
run RELEASE_GH_UPDATE_BRANCH=1
[ "$(updates)" -eq 0 ] || fail "clean: updated"

# 5. A failed update does not stop the wait.
printf 'OPEN BEHIND \nMERGED CLEAN %s\n' "$SHA" > "$TMP/views"
cat >> "$TMP/bin/gh" <<'FAKE'
[ "$1 $2" != "pr update-branch" ] || exit 1
FAKE
run RELEASE_GH_UPDATE_BRANCH=1
[ "$RC" -eq 0 ] || fail "failed update: exit $RC: $OUT"

# 6. Closed without a merge fails.
printf 'CLOSED DIRTY \n' > "$TMP/views"
run RELEASE_GH_UPDATE_BRANCH=1
[ "$RC" -ne 0 ] || fail "closed: exit 0"
case "$OUT" in *"closed without a merge"*) ;; *) fail "closed: $OUT" ;; esac

# 7. Time runs out and the limit message stays.
printf 'OPEN BEHIND \n' > "$TMP/views"
SECS=0 run RELEASE_GH_UPDATE_BRANCH=1
[ "$RC" -ne 0 ] || fail "timeout: exit 0"
case "$OUT" in *"did not merge within 0 seconds"*) ;; *) fail "timeout: $OUT" ;; esac

echo "release-gh wait: 7 cases passed"
