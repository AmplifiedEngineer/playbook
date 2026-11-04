#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# package-repo.sh: builds the signed apt and dnf repository (ADR 0052).
#
# Usage:
#   package-repo.sh build <site-dir> <assets-root> <yanked.txt>
#
# <assets-root> holds one folder per release, named X.Y.Z, with that release's
# SHA256SUMS and packages. Attestations are checked by the caller, before this
# script runs. The script checks every package against SHA256SUMS and its magic
# bytes, keeps the newest KEEP releases that are not yanked (default 5), builds
# both indexes in a scratch folder, signs them, and moves the result to
# <site-dir> only when everything worked.
#
# The signing key comes from PACKAGE_SIGNING_KEY (armored secret key) and
# PACKAGE_SIGNING_PASSPHRASE. Both stay out of arguments and logs. A key with
# under 30 days left fails the build and one with under 90 days warns.
set -euo pipefail

KEEP="${KEEP:-5}"
WARN_DAYS=90
FAIL_DAYS=30

fail() { echo "FAIL $*" >&2; exit 1; }
log() { echo "package-repo: $*" >&2; }

semver() { printf '%s' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; }

# yanked_versions <file>: prints one version per line, fails on any other text.
yanked_versions() {
  local line n=0
  [ -f "$1" ] || fail "yank list $1 does not exist"
  while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    line="${line%%#*}"
    line="$(printf '%s' "$line" | tr -d '[:space:]')"
    [ -n "$line" ] || continue
    semver "$line" || fail "$1 line $n is not a MAJOR.MINOR.PATCH version"
    echo "$line"
  done < "$1"
}

# check_magic <file>: a .deb is an ar archive, an .rpm starts with 0xedabeedb.
check_magic() {
  case "$1" in
    *.deb) [ "$(head -c 7 "$1")" = '!<arch>' ] || fail "$1 is not a Debian package" ;;
    *.rpm) [ "$(head -c 4 "$1" | od -An -tx1 | tr -d ' \n')" = edabeedb ] || fail "$1 is not an rpm package" ;;
  esac
}

# release_packages <dir> <version>: verifies the folder and prints the package
# file names it lists. Prints nothing for a release without packages.
release_packages() {
  local dir="$1" v="$2" sums="$1/SHA256SUMS" hash name f
  [ -f "$sums" ] || fail "release $v has no SHA256SUMS"
  while read -r hash name; do
    name="${name#\*}"
    case "$name" in
      "playbook_${v}_amd64.deb" | "playbook_${v}_arm64.deb" | "playbook-${v}.x86_64.rpm" | "playbook-${v}.aarch64.rpm") ;;
      *) continue ;;
    esac
    [ -f "$dir/$name" ] || fail "release $v lists $name but the file is missing"
    [ "$(sha256sum "$dir/$name" | cut -d' ' -f1)" = "$hash" ] || fail "$name does not match its SHA256SUMS entry"
    check_magic "$dir/$name"
    echo "$name"
  done < "$sums"
  for f in "$dir"/playbook_*.deb "$dir"/playbook-*.rpm; do
    [ -e "$f" ] || continue
    awk -v n="${f##*/}" '{ sub(/^\*/, "", $2); if ($2 == n) found = 1 } END { exit !found }' "$sums" || fail "${f##*/} is not listed in the SHA256SUMS of release $v"
  done
}

# key_check <fingerprint>: the earliest expiry among the keys decides.
key_check() {
  local expiry now left
  expiry="$(gpg --list-keys --with-colons "$1" | awk -F: '/^(pub|sub):/ && $7 != "" {print $7}' | sort -n | head -n 1)"
  [ -n "$expiry" ] || { log "warning: the signing key never expires"; return 0; }
  now="$(date +%s)"
  left=$(((expiry - now) / 86400))
  [ "$left" -ge "$FAIL_DAYS" ] || fail "the signing key expires in $left days, under the $FAIL_DAYS day limit"
  [ "$left" -ge "$WARN_DAYS" ] || log "warning: the signing key expires in $left days"
}

# sign <in> <clear-out> <detached-out>: pass an empty string to skip an output.
sign() {
  local in="$1" clear="$2" detached="$3"
  [ -z "$clear" ] || gpg --batch --yes --pinentry-mode loopback --passphrase-fd 0 \
    --default-key "$FPR" --faked-system-time "$STAMP!" --clearsign -o "$clear" "$in" <<< "$PACKAGE_SIGNING_PASSPHRASE"
  [ -z "$detached" ] || gpg --batch --yes --pinentry-mode loopback --passphrase-fd 0 \
    --default-key "$FPR" --faked-system-time "$STAMP!" --armor --detach-sign -o "$detached" "$in" <<< "$PACKAGE_SIGNING_PASSPHRASE"
}

build_apt() {
  local site="$1" arch
  mkdir -p "$site/apt/pool/main/p/playbook"
  compgen -G "$SELECTED/*.deb" > /dev/null && cp "$SELECTED"/*.deb "$site/apt/pool/main/p/playbook/"
  (
    cd "$site/apt"
    for arch in amd64 arm64; do
      mkdir -p "dists/stable/main/binary-$arch"
      apt-ftparchive --arch "$arch" packages pool > "dists/stable/main/binary-$arch/Packages"
      gzip -9n -c "dists/stable/main/binary-$arch/Packages" > "dists/stable/main/binary-$arch/Packages.gz"
    done
    apt-ftparchive \
      -o APT::FTPArchive::Release::Origin=playbook \
      -o APT::FTPArchive::Release::Label=playbook \
      -o APT::FTPArchive::Release::Suite=stable \
      -o APT::FTPArchive::Release::Codename=stable \
      -o APT::FTPArchive::Release::Components=main \
      -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
      release dists/stable > dists/stable/Release.new
    mv dists/stable/Release.new dists/stable/Release
  )
  sign "$site/apt/dists/stable/Release" "$site/apt/dists/stable/InRelease" ""
  sign "$site/apt/dists/stable/Release" "" "$site/apt/dists/stable/Release.gpg"
}

build_rpm() {
  local site="$1" arch
  for arch in x86_64 aarch64; do
    mkdir -p "$site/rpm/$arch"
    compgen -G "$SELECTED/*.$arch.rpm" > /dev/null && cp "$SELECTED"/*."$arch".rpm "$site/rpm/$arch/"
    createrepo_c --quiet --no-database --revision "$EPOCH" --set-timestamp-to-revision "$site/rpm/$arch"
    sign "$site/rpm/$arch/repodata/repomd.xml" "" "$site/rpm/$arch/repodata/repomd.xml.asc"
  done
}

build() {
  local site="${1:-}" root="${2:-}" yank="${3:-}" work v dir name n=0 f
  [ -n "$site" ] && [ -n "$root" ] && [ -n "$yank" ] || fail "usage: package-repo.sh build <site-dir> <assets-root> <yanked.txt>"
  [ -d "$root" ] || fail "assets folder $root does not exist"
  [ -n "${PACKAGE_SIGNING_KEY:-}" ] && [ -n "${PACKAGE_SIGNING_PASSPHRASE:-}" ] \
    || fail "PACKAGE_SIGNING_KEY and PACKAGE_SIGNING_PASSPHRASE must be set"
  [ ! -e "$site" ] || fail "$site already exists"
  for tool in apt-ftparchive createrepo_c gpg; do
    command -v "$tool" > /dev/null || fail "$tool is required"
  done

  work="$(mktemp -d)"
  chmod 700 "$work"
  trap 'gpgconf --kill gpg-agent 2> /dev/null || true; rm -rf "$work"' EXIT
  export GNUPGHOME="$work/gnupg"
  mkdir -m 700 "$GNUPGHOME"

  yanked_versions "$yank" > "$work/yanked"
  mkdir "$work/selected"
  SELECTED="$work/selected"
  while IFS= read -r dir; do
    v="${dir##*/}"
    semver "$v" || continue
    if grep -Fxq "$v" "$work/yanked"; then
      log "release $v is yanked"
      continue
    fi
    [ "$n" -lt "$KEEP" ] || break
    release_packages "$dir" "$v" > "$work/names"
    if [ ! -s "$work/names" ]; then
      log "release $v lists no packages, so it is skipped"
      continue
    fi
    while read -r name; do cp "$dir/$name" "$SELECTED/"; done < "$work/names"
    echo "$v" >> "$work/kept"
    [ -n "${NEWEST:-}" ] || NEWEST="$dir"
    n=$((n + 1))
  done < <(find "$root" -mindepth 1 -maxdepth 1 -type d | sort -V -r)
  [ "$n" -gt 0 ] || fail "no packaged release found in $root"

  printf '%s' "$PACKAGE_SIGNING_KEY" | gpg --batch --quiet --import 2> /dev/null
  FPR="$(gpg --list-secret-keys --with-colons | awk -F: '/^fpr:/ {print $10; exit}')"
  [ -n "$FPR" ] || fail "the signing key could not be imported"
  key_check "$FPR"

  # The caller passes the publish time of the newest release, so a rerun
  # signs at the same time. The file time is the fallback.
  EPOCH="${SOURCE_DATE_EPOCH:-$(stat -c %Y "$NEWEST/SHA256SUMS" 2> /dev/null || stat -f %m "$NEWEST/SHA256SUMS")}"
  STAMP="$(date -u -d "@$EPOCH" +%Y%m%dT%H%M%S 2> /dev/null || date -u -r "$EPOCH" +%Y%m%dT%H%M%S)"
  export SOURCE_DATE_EPOCH="$EPOCH"

  mkdir "$work/site"
  build_apt "$work/site"
  build_rpm "$work/site"
  gpg --batch --export "$FPR" > "$work/site/playbook.gpg"
  gpg --batch --armor --export "$FPR" > "$work/site/playbook.asc"
  mv "$work/site" "$site"
  for f in $(cat "$work/kept"); do log "published $f"; done
}

case "${1:-}" in
  build) shift; build "$@" ;;
  *) fail "usage: package-repo.sh build <site-dir> <assets-root> <yanked.txt>" ;;
esac
