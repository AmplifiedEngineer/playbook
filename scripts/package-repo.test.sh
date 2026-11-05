#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# package-repo.test.sh: offline tests for package-repo.sh. No network.
#
# Usage: package-repo.test.sh
#
# Each case builds fake packages and a throwaway signing key in its own temp
# folder. The suite needs dpkg-deb, rpmbuild, apt-ftparchive, createrepo_c and
# gpg, so it skips on a machine without them (macOS).
set -euo pipefail

cd "$(dirname "$0")/.."

for tool in dpkg-deb rpmbuild apt-ftparchive createrepo_c gpg; do
  command -v "$tool" > /dev/null || { echo "SKIP package-repo.test.sh: $tool is not installed"; exit 0; }
done

SCRIPT="$PWD/scripts/package-repo.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
PASS="sentinel-passphrase-7f3a9c"

pass=0
fail() { echo "FAIL $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); }

# new_case <name>: makes $TMP/<name> with an empty assets folder, sets CASE.
new_case() { CASE="$TMP/$1"; mkdir -p "$CASE/assets"; }

# make_key <days>: a signing key that expires in <days> days. Sets KEY.
make_key() {
  local home="$CASE/keyhome"
  mkdir -p "$home"
  chmod 700 "$home"
  GNUPGHOME="$home" gpg --batch --quiet --pinentry-mode loopback --passphrase "$PASS" \
    --quick-generate-key "playbook test <test@example.invalid>" rsa2048 sign "${1}d" 2> /dev/null
  KEY="$(GNUPGHOME="$home" gpg --batch --pinentry-mode loopback --passphrase "$PASS" --armor --export-secret-keys)"
}

# make_deb <dir> <version> <arch>
make_deb() {
  local root="$CASE/deb-$2-$3"
  mkdir -p "$root/DEBIAN"
  printf 'Package: playbook\nVersion: %s\nArchitecture: %s\nMaintainer: test <test@example.invalid>\nDescription: test\n' "$2" "$3" > "$root/DEBIAN/control"
  dpkg-deb --root-owner-group -Zgzip --build "$root" "$1/playbook_$2_$3.deb" > /dev/null
}

# make_rpm <dir> <version> <arch>
make_rpm() {
  local top="$CASE/rpm-$2-$3"
  mkdir -p "$top"/{SPECS,BUILD,RPMS,SRPMS,SOURCES,BUILDROOT}
  cat > "$top/SPECS/playbook.spec" << EOF
Name: playbook
Version: $2
Release: 1
Summary: test
License: Apache-2.0
%description
test
%install
mkdir -p %{buildroot}/usr/bin
echo hi > %{buildroot}/usr/bin/playbook
%files
/usr/bin/playbook
EOF
  rpmbuild --quiet --target "$3" --define "_topdir $top" -bb "$top/SPECS/playbook.spec" > /dev/null 2>&1
  cp "$top/RPMS/$3/playbook-$2-1.$3.rpm" "$1/playbook-$2.$3.rpm"
}

# release <version>: a release folder with the four packages and a SHA256SUMS.
release() {
  local d="$CASE/assets/$1"
  mkdir -p "$d"
  make_deb "$d" "$1" amd64
  make_deb "$d" "$1" arm64
  make_rpm "$d" "$1" x86_64
  make_rpm "$d" "$1" aarch64
  (cd "$d" && sha256sum playbook_* playbook-* > SHA256SUMS)
}

# run_build: builds into $CASE/site with $CASE/yanked.txt; sets RC and
# leaves the output in $CASE/out and $CASE/err.
run_build() {
  : > "$CASE/yanked.txt"
  [ ! -f "$CASE/yanked.in" ] || cp "$CASE/yanked.in" "$CASE/yanked.txt"
  RC=0
  (
    SOURCE_DATE_EPOCH="$(date +%s)"
    export PACKAGE_SIGNING_KEY="$KEY" PACKAGE_SIGNING_PASSPHRASE="$PASS" SOURCE_DATE_EPOCH
    bash "$SCRIPT" build "${SITE:-$CASE/site}" "$CASE/assets" "$CASE/yanked.txt" > "$CASE/out" 2> "$CASE/err"
  ) || RC=$?
}

versions_in_packages() { grep -h '^Version:' "$CASE/site/apt/dists/stable/main/binary-amd64/Packages" | sort -V; }
rpm_count() { find "$CASE/site/rpm/x86_64" -name '*.rpm' | wc -l | tr -d ' '; }

# Scenario: package altered after release
new_case altered; make_key 1000; release 1.0.0
echo tamper >> "$CASE/assets/1.0.0/playbook_1.0.0_amd64.deb"
run_build
[ "$RC" -ne 0 ] || fail "an altered package built"
[ ! -e "$CASE/site" ] || fail "an altered package left a site"
grep -q 'does not match' "$CASE/err" || fail "no hash message: $(cat "$CASE/err")"
ok

# Scenario: SHA256SUMS missing
new_case nosums; make_key 1000; release 1.0.0
rm "$CASE/assets/1.0.0/SHA256SUMS"
run_build
[ "$RC" -ne 0 ] || fail "a release without SHA256SUMS built"
ok

# Scenario: package not listed in SHA256SUMS
new_case unlisted; make_key 1000; release 1.0.0
grep -v 'arm64.deb' "$CASE/assets/1.0.0/SHA256SUMS" > "$CASE/s" && mv "$CASE/s" "$CASE/assets/1.0.0/SHA256SUMS"
run_build
[ "$RC" -ne 0 ] || fail "an unlisted package built"
grep -q 'not listed' "$CASE/err" || fail "no unlisted message: $(cat "$CASE/err")"
ok

# Scenario: empty assets folder
new_case empty; make_key 1000
run_build
[ "$RC" -ne 0 ] || fail "an empty assets folder built"
grep -q 'no packaged release' "$CASE/err" || fail "no empty message: $(cat "$CASE/err")"
ok

# Scenario: wrong magic bytes
new_case magic; make_key 1000; release 1.0.0
printf 'not a package' > "$CASE/assets/1.0.0/playbook_1.0.0_amd64.deb"
(cd "$CASE/assets/1.0.0" && sha256sum playbook_* playbook-* > SHA256SUMS)
run_build
[ "$RC" -ne 0 ] || fail "a fake .deb built"
grep -q 'not a Debian package' "$CASE/err" || fail "no magic message: $(cat "$CASE/err")"
ok

# Scenario: one packaged release, with a release without packages beside it
new_case single; make_key 1000; release 1.0.0
mkdir "$CASE/assets/0.9.0"; echo "$(printf 'a%.0s' {1..64})  playbook-0.9.0-x86_64-unknown-linux-musl" > "$CASE/assets/0.9.0/SHA256SUMS"
run_build
[ "$RC" -eq 0 ] || fail "a single release did not build: $(cat "$CASE/err")"
[ "$(versions_in_packages)" = "Version: 1.0.0" ] || fail "unexpected versions: $(versions_in_packages)"
grep -q '0.9.0 lists no packages' "$CASE/err" || fail "the empty release was not skipped"
ok

# Scenario Outline: retention (n releases, kept)
for pair in 4:4 5:5 6:5; do
  n="${pair%:*}" kept="${pair#*:}"
  new_case "keep$n"; make_key 1000
  for i in $(seq 1 "$n"); do release "1.$i.0"; done
  run_build
  [ "$RC" -eq 0 ] || fail "retention $n did not build: $(cat "$CASE/err")"
  [ "$(versions_in_packages | wc -l | tr -d ' ')" = "$kept" ] || fail "retention $n kept $(versions_in_packages | wc -l)"
  [ "$(rpm_count)" = "$kept" ] || fail "retention $n kept $(rpm_count) rpms"
  [ "$(versions_in_packages | tail -n 1)" = "Version: 1.$n.0" ] || fail "retention $n lost the newest"
done
ok

# Scenario: yanked.txt names 0.38.0
new_case yank; make_key 1000; release 0.38.0; release 0.39.0
echo "0.38.0" > "$CASE/yanked.in"
run_build
[ "$RC" -eq 0 ] || fail "a yank did not build: $(cat "$CASE/err")"
[ "$(versions_in_packages)" = "Version: 0.39.0" ] || fail "the yanked version is still listed"
[ "$(rpm_count)" = 1 ] || fail "the yanked rpm is still listed"
ok

# Scenario: malformed yanked.txt
new_case badyank; make_key 1000; release 1.0.0
echo "v1.0" > "$CASE/yanked.in"
run_build
[ "$RC" -ne 0 ] || fail "a malformed yank list built"
ok

# Scenario: valid run, signatures verify, a second run gives the same indexes
new_case valid; make_key 1000; release 1.0.0
run_build
[ "$RC" -eq 0 ] || fail "a valid run failed: $(cat "$CASE/err")"
VERIFY="$CASE/verify"; mkdir -m 700 "$VERIFY"
GNUPGHOME="$VERIFY" gpg --batch --quiet --import "$CASE/site/playbook.asc" 2> /dev/null
GNUPGHOME="$VERIFY" gpg --batch --verify "$CASE/site/apt/dists/stable/InRelease" 2> /dev/null || fail "InRelease does not verify"
GNUPGHOME="$VERIFY" gpg --batch --verify "$CASE/site/apt/dists/stable/Release.gpg" "$CASE/site/apt/dists/stable/Release" 2> /dev/null || fail "Release.gpg does not verify"
GNUPGHOME="$VERIFY" gpg --batch --verify "$CASE/site/rpm/x86_64/repodata/repomd.xml.asc" "$CASE/site/rpm/x86_64/repodata/repomd.xml" 2> /dev/null || fail "repomd.xml.asc does not verify"
SITE="$CASE/site2" run_build
[ "$RC" -eq 0 ] || fail "the second run failed: $(cat "$CASE/err")"
cmp "$CASE/site/apt/dists/stable/main/binary-amd64/Packages" "$CASE/site2/apt/dists/stable/main/binary-amd64/Packages" || fail "Packages differs between runs"
cmp "$CASE/site/playbook.asc" "$CASE/site2/playbook.asc" || fail "the public key differs between runs"
ok

# Scenario: tampered InRelease or repomd.xml
sed 's/^Date:/Dat3:/' "$CASE/site/apt/dists/stable/InRelease" > "$CASE/InRelease.bad"
mv "$CASE/InRelease.bad" "$CASE/site/apt/dists/stable/InRelease"
if GNUPGHOME="$VERIFY" gpg --batch --verify "$CASE/site/apt/dists/stable/InRelease" 2> /dev/null; then fail "a tampered InRelease verified"; fi
echo tamper >> "$CASE/site/rpm/x86_64/repodata/repomd.xml"
if GNUPGHOME="$VERIFY" gpg --batch --verify "$CASE/site/rpm/x86_64/repodata/repomd.xml.asc" "$CASE/site/rpm/x86_64/repodata/repomd.xml" 2> /dev/null; then fail "a tampered repomd.xml verified"; fi
ok

# Scenario: the passphrase appears nowhere
grep -rqF "$PASS" "$CASE/out" "$CASE/err" "$CASE/site" "$CASE/site2" && fail "the passphrase leaked"
ok

# Scenario Outline: key expiry (days left, result)
for pair in 29:fail 89:warn 91:pass; do
  days="${pair%:*}" want="${pair#*:}"
  new_case "expiry$days"; make_key "$days"; release 1.0.0
  run_build
  case "$want" in
    fail) [ "$RC" -ne 0 ] || fail "a key with $days days built"; grep -q 'under the 30 day limit' "$CASE/err" || fail "no expiry message" ;;
    warn) [ "$RC" -eq 0 ] || fail "a key with $days days failed"; grep -q 'warning: the signing key expires' "$CASE/err" || fail "no expiry warning" ;;
    pass) [ "$RC" -eq 0 ] || fail "a key with $days days failed"; ! grep -q 'warning' "$CASE/err" || fail "an unexpected warning" ;;
  esac
done
ok

# Scenario: the newest and previous version of a built site
# One release has no previous version, and that must not trip set -u or a
# negative array subscript.
new_case outputs
mkdir -p "$CASE/rpm"
touch "$CASE/rpm/playbook-0.40.0-1.x86_64.rpm"
out="$(bash "$SCRIPT" versions "$CASE/rpm" 2>&1)" || fail "one release: versions failed: $out"
[ "$out" = "$(printf 'version=0.40.0-1\nprevious=')" ] || fail "one release: unexpected output: $out"
ok
touch "$CASE/rpm/playbook-0.39.0-1.x86_64.rpm" "$CASE/rpm/playbook-0.9.0-1.x86_64.rpm"
out="$(bash "$SCRIPT" versions "$CASE/rpm" 2>&1)" || fail "three releases: versions failed: $out"
[ "$out" = "$(printf 'version=0.40.0-1\nprevious=0.39.0-1')" ] || fail "three releases: unexpected output: $out"
ok
if bash "$SCRIPT" versions "$CASE/none" > /dev/null 2>&1; then fail "an empty folder printed versions"; fi
ok

echo "OK package-repo.test.sh: $pass scenarios passed"
