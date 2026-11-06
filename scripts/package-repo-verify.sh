#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# package-repo-verify.sh: installs from a built repository folder in clean
# Debian, Ubuntu and Fedora containers (ADR 0052).
#
# Usage: package-repo-verify.sh <site-dir> <version> [<previous-version>]
#
# Each container has no network and reads the repository through file:, with
# the distro sources switched off. It checks that a tampered index is refused,
# installs the newest package and checks its version. With a previous version
# it installs that first, upgrades, and checks that a seeded file under
# ~/.config/playbook is unchanged. The images are the ones package-smoke.sh pins.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fail() { echo "FAIL $*" >&2; exit 1; }

site="${1:-}" version="${2:-}" prev="${3:-}"
[ -d "$site" ] && [ -n "$version" ] || { echo "usage: package-repo-verify.sh <site-dir> <version> [<previous-version>]" >&2; exit 2; }
site="$(cd "$site" && pwd)"

# The image pins live in package-smoke.sh so there is one place to bump them.
pins="$(grep -E '^(DEBIAN|UBUNTU|FEDORA)=' "$ROOT/scripts/package-smoke.sh" | sed 's/[[:space:]]*#.*//')"
eval "$pins"

# The script each container runs. $1 is the kind (apt or dnf), $2 the version
# and $3 the previous version, which may be empty.
inner='
set -eu
kind="$1"; want="$2"; prev="${3:-}"
tamper() { rm -rf /tmp/bad; cp -r /site /tmp/bad; echo tampered >> "$1"; }
seed() { mkdir -p "$HOME/.config/playbook"; echo keep > "$HOME/.config/playbook/user.conf"; }
if [ "$kind" = apt ]; then
  mkdir -p /etc/apt/keyrings
  cp /site/playbook.gpg /etc/apt/keyrings/playbook.gpg
  # POSIX sh: Debian and Ubuntu run this under dash, which has no ${@:2}.
  apt_with() {
    dir="$1"; shift
    echo "deb [signed-by=/etc/apt/keyrings/playbook.gpg] file://$dir/apt stable main" > /tmp/playbook.list
    apt-get -o Dir::Etc::sourcelist=/tmp/playbook.list -o Dir::Etc::sourceparts=- "$@"
  }
  tamper /tmp/bad/apt/dists/stable/InRelease
  if apt_with /tmp/bad update >/dev/null 2>&1; then echo "apt accepted a tampered InRelease" >&2; exit 1; fi
  apt_with /site update >/dev/null
  if [ -n "$prev" ]; then apt_with /site install -y "playbook=$prev" >/dev/null; seed; fi
  apt_with /site install -y "playbook=$want" >/dev/null
else
  repo() {
    printf "[playbook]\nname=playbook\nbaseurl=file://%s/rpm/\$basearch\nenabled=1\nrepo_gpgcheck=1\ngpgcheck=0\ngpgkey=file:///site/playbook.asc\n" "$1" > /etc/yum.repos.d/playbook.repo
  }
  tamper /tmp/bad/rpm/x86_64/repodata/repomd.xml
  repo /tmp/bad
  if dnf -y --disablerepo="*" --enablerepo=playbook makecache >/dev/null 2>&1; then echo "dnf accepted a tampered repomd.xml" >&2; exit 1; fi
  repo /site
  if [ -n "$prev" ]; then dnf -y --disablerepo="*" --enablerepo=playbook install "playbook-$prev" >/dev/null; seed; fi
  dnf -y --disablerepo="*" --enablerepo=playbook install "playbook-$want" >/dev/null 2>&1 \
    || dnf -y --disablerepo="*" --enablerepo=playbook upgrade "playbook-$want" >/dev/null
fi
out="$(playbook --version)"
[ "${out##* }" = "$want" ] || { echo "playbook --version says $out, not $want" >&2; exit 1; }
if [ -n "$prev" ]; then
  [ "$(cat "$HOME/.config/playbook/user.conf")" = keep ] || { echo "user config changed by the upgrade" >&2; exit 1; }
fi
'

run() {
  echo "== $1"
  docker run --rm --network none -v "$site:/site:ro" "$2" sh -c "$inner" sh "$3" "$version" "$prev" \
    || fail "$1 failed (container exit $?)"
  echo "ok $1"
}

run debian "$DEBIAN" apt
run ubuntu "$UBUNTU" apt
run fedora "$FEDORA" dnf
