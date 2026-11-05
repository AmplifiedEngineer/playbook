#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# package-repo-fetch.test.sh: offline tests for package-repo-fetch.sh. No network.
#
# Usage: package-repo-fetch.test.sh
#
# A fake `gh` serves releases from a fixture folder. Each release folder holds
# its assets and a `.ref` file with the source ref its attestation names, and a
# `.badattest` file listing assets whose attestation does not verify. The suite
# needs GNU date and sha256sum, so it skips on a machine without them (macOS).
set -euo pipefail

cd "$(dirname "$0")/.."

date -u -d "2026-01-02T03:04:05Z" +%s > /dev/null 2>&1 || { echo "SKIP package-repo-fetch.test.sh: GNU date is not installed"; exit 0; }
command -v sha256sum > /dev/null || { echo "SKIP package-repo-fetch.test.sh: sha256sum is not installed"; exit 0; }

SCRIPT="$PWD/scripts/package-repo-fetch.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail() { echo "FAIL $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); }

mkdir -p "$TMP/bin"
cat > "$TMP/bin/gh" << 'FAKE'
#!/usr/bin/env bash
# Fake gh: serves $FIXTURES. `release list` prints the fixture list as the real
# --jq would. `release download` copies assets and the .ref marker into the cwd.
case "$1 $2" in
  "release list") cat "$FIXTURES/list.tsv" ;;
  "release download")
    tag="$3" pat=""
    while [ $# -gt 0 ]; do [ "$1" = --pattern ] && pat="$2"; shift; done
    [ -f "$FIXTURES/$tag/$pat" ] || { echo "no asset $pat in $tag" >&2; exit 1; }
    cp "$FIXTURES/$tag/$pat" "$FIXTURES/$tag/.ref" . 2> /dev/null || true
    cp "$FIXTURES/$tag/.badattest" . 2> /dev/null || true ;;
  "attestation verify")
    file="$3" want=""
    while [ $# -gt 0 ]; do [ "$1" = --source-ref ] && want="$2"; shift; done
    [ "$(cat .ref)" = "$want" ] || { echo "Error: expected SourceRepositoryRef to be $want, got $(cat .ref)" >&2; exit 1; }
    if [ -f .badattest ] && grep -qx "$file" .badattest; then echo "Error: no attestation for $file" >&2; exit 1; fi ;;
  *) echo "fake gh: unexpected $*" >&2; exit 9 ;;
esac
FAKE
chmod +x "$TMP/bin/gh"

# new_case <name>: makes an empty fixture folder in $FIX and a release list.
new_case() { FIX="$TMP/$1"; mkdir -p "$FIX"; : > "$FIX/list.tsv"; OUT="$TMP/$1-out"; }

# add_release <tag> <ref> <with-packages>: adds a release, newest first.
add_release() {
  local tag="$1" ref="$2" pk="$3" v dir
  v="${tag#v}"
  dir="$FIX/$tag"
  mkdir -p "$dir"
  printf '%s' "$ref" > "$dir/.ref"
  : > "$dir/SHA256SUMS"
  if [ "$pk" = yes ]; then
    for name in "playbook_${v}_amd64.deb" "playbook-${v}.x86_64.rpm"; do
      echo "package $name" > "$dir/$name"
      (cd "$dir" && sha256sum "$name" >> SHA256SUMS)
    done
  fi
  printf '%s\t2026-10-21T22:02:04Z\n' "$tag" >> "$FIX/list.tsv"
}

# run_fetch: runs the script; sets RC and LOG.
run_fetch() {
  RC=0
  LOG="$(PATH="$TMP/bin:$PATH" FIXTURES="$FIX" GH_REPO=o/r SOURCE_REF=refs/heads/main "$SCRIPT" "$OUT" 5 2>&1)" || RC=$?
}

# 1: releases that list no packages are skipped even when their attestation
# names a tag. Plugin tags are ignored. The newest packaged release is kept.
new_case first
add_release v0.40.0 refs/heads/main yes
add_release playbook-plugin-v0.33.0 refs/heads/main no
add_release v0.34.0 refs/tags/v0.34.0 no
run_fetch
[ "$RC" -eq 0 ] || fail "case 1 exited $RC: $LOG"
[ "$(printf '%s\n' "$LOG" | tail -n 1)" = 1792620124 ] || fail "case 1 epoch: $LOG"
[ -f "$OUT/0.40.0/playbook_0.40.0_amd64.deb" ] || fail "case 1 lost the package"
[ ! -d "$OUT/0.34.0" ] || fail "case 1 kept an unpackaged release"
ok

# 2: a release that lists packages but was attested from a tag is refused.
new_case tagref
add_release v0.40.0 refs/heads/main yes
add_release v0.39.0 refs/tags/v0.39.0 yes
run_fetch
[ "$RC" -ne 0 ] || fail "case 2 accepted a tag attested release: $LOG"
printf '%s\n' "$LOG" | grep -q "release v0.39.0 did not verify" || fail "case 2 message: $LOG"
ok

# 3: a package whose own attestation fails is refused, SHA256SUMS being fine.
new_case badpkg
add_release v0.40.0 refs/heads/main yes
echo "playbook_0.40.0_amd64.deb" > "$FIX/v0.40.0/.badattest"
run_fetch
[ "$RC" -ne 0 ] || fail "case 3 accepted an unattested package: $LOG"
printf '%s\n' "$LOG" | grep -q "release v0.40.0 did not verify" || fail "case 3 message: $LOG"
ok

# 4: a hash mismatch is refused.
new_case badhash
add_release v0.40.0 refs/heads/main yes
echo tampered > "$FIX/v0.40.0/playbook_0.40.0_amd64.deb"
run_fetch
[ "$RC" -ne 0 ] || fail "case 4 accepted a tampered package: $LOG"
ok

# 5: no release with packages at all is an error.
new_case none
add_release v0.34.0 refs/tags/v0.34.0 no
run_fetch
[ "$RC" -ne 0 ] || fail "case 5 passed with no packages: $LOG"
printf '%s\n' "$LOG" | grep -q "no release with packages" || fail "case 5 message: $LOG"
ok

echo "package-repo-fetch.test.sh: $pass passed"
