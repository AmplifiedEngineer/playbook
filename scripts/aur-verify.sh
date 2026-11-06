#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# aur-verify.sh: builds and checks the AUR package playbook-bin. The release
# runs it inside the archlinux:base-devel container, as root, because makepkg
# refuses root and pacman -U needs it. The script makes a user for makepkg.
#
#   aur-verify.sh srcinfo <dir>
#     Writes <dir>/.SRCINFO with `makepkg --printsrcinfo` of <dir>/PKGBUILD.
#   aur-verify.sh build <dir> <version>
#     Builds <dir>/PKGBUILD with `makepkg -f` (no -s: the package has no
#     dependencies, so no pacman mirror is used), installs it with pacman -U,
#     checks that `playbook --version` says <version>, that pacman owns
#     /usr/bin/playbook, and that <dir>/.SRCINFO, when there, is what
#     makepkg prints now. It then downloads the aarch64 asset and checks it
#     against the sum in the PKGBUILD. CARCH=aarch64 makepkg --verifysource is
#     not used, since makepkg.conf may override CARCH.
#   aur-verify.sh aarch64-asset <PKGBUILD>
#     Prints "<url> <sha256>" of the aarch64 source. Needs no container.
set -euo pipefail

fail() { echo "FAIL $*" >&2; exit 1; }
usage() { fail "usage: aur-verify.sh srcinfo <dir> | build <dir> <version> | aarch64-asset <PKGBUILD>"; }

aarch64_asset() {
  [ -f "$1" ] || fail "$1 is not a file"
  # The PKGBUILD is the one this repo rendered; sourcing it defines variables
  # and the package function and runs nothing else.
  (
    # shellcheck disable=SC1090
    . "$1"
    [ -n "${source_aarch64[0]:-}" ] || fail "no aarch64 source in $1"
    [ -n "${sha256sums_aarch64[0]:-}" ] || fail "no aarch64 sum in $1"
    echo "${source_aarch64[0]#*::} ${sha256sums_aarch64[0]}"
  )
}

# as_builder <dir> <command...>: runs a command as the build user in <dir>.
as_builder() {
  local dir="$1"
  shift
  # shellcheck disable=SC2016
  runuser -u builder -- bash -c 'cd "$1" && shift && "$@"' _ "$dir" "$@"
}

# prepare <dir>: makes the build user and a copy of <dir> it owns; prints it.
prepare() {
  id builder >/dev/null 2>&1 || useradd --create-home builder
  local work
  work="$(mktemp -d /home/builder/pkg.XXXXXX)"
  cp -R "$1/." "$work/"
  chown -R builder:builder "$work"
  echo "$work"
}

[ $# -ge 1 ] || usage
case "$1" in
  aarch64-asset)
    [ $# -eq 2 ] || usage
    aarch64_asset "$2"
    ;;
  srcinfo)
    [ $# -eq 2 ] || usage
    [ -d "$2" ] || fail "$2 is not a directory"
    work="$(prepare "$2")"
    as_builder "$work" makepkg --printsrcinfo > "$2/.SRCINFO"
    ;;
  build)
    [ $# -eq 3 ] || usage
    [ -d "$2" ] || fail "$2 is not a directory"
    version="$3"
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "bad version: '$version' is not MAJOR.MINOR.PATCH"
    work="$(prepare "$2")"
    as_builder "$work" makepkg -f --noconfirm
    pacman -U --noconfirm "$work"/playbook-bin-*.pkg.tar.*
    out="$(playbook --version)"
    [ "${out##* }" = "$version" ] || fail "playbook --version says '$out', not $version"
    echo "installed: $out"
    pacman -Qo /usr/bin/playbook | grep -q 'playbook-bin' || fail "pacman does not own /usr/bin/playbook as playbook-bin"
    top="$(pbtop --version)"
    [ "${top##* }" = "$version" ] || fail "pbtop --version says '$top', not $version"
    pacman -Qo /usr/bin/pbtop | grep -q 'playbook-bin' || fail "pacman does not own /usr/bin/pbtop as playbook-bin"
    if [ -f "$2/.SRCINFO" ]; then
      as_builder "$work" makepkg --printsrcinfo | cmp -s - "$2/.SRCINFO" || fail ".SRCINFO does not match the PKGBUILD"
    fi
    asset="$(aarch64_asset "$2/PKGBUILD")"
    url="${asset% *}" sum="${asset##* }"
    curl -fsSL --retry 3 -o "$work/aarch64-asset" "$url"
    echo "$sum  $work/aarch64-asset" | sha256sum -c -
    echo "built, installed and checked playbook-bin $version"
    ;;
  *) usage ;;
esac
