#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# render-pkgbuild.test.sh: offline tests for render-pkgbuild.sh. No network,
# no pacman. Every test makes its own directory and shares no state.
#
# Usage: render-pkgbuild.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."

SCRIPT="$PWD/scripts/render-pkgbuild.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail() { echo "FAIL $*" >&2; exit 1; }
ok() { pass=$((pass + 1)); }

hex_of() { printf '%s' "$1" | shasum -a 256 | cut -d' ' -f1; }

T_LINUX_X=x86_64-unknown-linux-musl
T_LINUX_A=aarch64-unknown-linux-musl

# new_case <name>: makes $TMP/<name> and sets CASE.
new_case() { CASE="$TMP/$1"; mkdir -p "$CASE"; }

# sums_full <version> <file>: a SHA256SUMS as the release makes it, with all
# five targets, the pbtop and pbdoc archives and release-notes.json.
sums_full() {
  local v="$1" f="$2" t
  {
    printf '%s  playbook-%s-aarch64-apple-darwin\n' "$(hex_of dar)" "$v"
    printf '%s  playbook-%s-%s\n' "$(hex_of linux-arm)" "$v" "$T_LINUX_A"
    printf '%s  playbook-%s-x86_64-apple-darwin\n' "$(hex_of dax)" "$v"
    printf '%s  playbook-%s-x86_64-pc-windows-msvc.exe\n' "$(hex_of win)" "$v"
    printf '%s *playbook-%s-%s\n' "$(hex_of linux-x86)" "$v" "$T_LINUX_X"
    for t in aarch64-apple-darwin "$T_LINUX_A" x86_64-apple-darwin "$T_LINUX_X"; do
      printf '%s  pbtop-%s-%s.tar.gz\n' "$(hex_of "pbtop$t")" "$v" "$t"
      printf '%s  pbdoc-%s-%s.tar.gz\n' "$(hex_of "pbdoc$t")" "$v" "$t"
    done
    printf '%s  release-notes.json\n' "$(hex_of notes)"
  } > "$f"
}

# expect_fail <code> <message> <args...>: exits with <code>, prints <message>,
# and leaves no output file.
expect_fail() {
  local code="$1" msg="$2" out rc=0
  shift 2
  out="$("$SCRIPT" "$@" 2>&1)" || rc=$?
  [ "$rc" -eq "$code" ] || fail "expected exit $code, got $rc for: $* ($out)"
  grep -qF -- "$msg" <<<"$out" || fail "expected message '$msg', got: $out"
  [ "$1" = keep ] || [ ! -e "${3:-/nonexistent}" ] || fail "a refused run wrote $3"
  ok
}

# A release renders both Linux sums, one assertion per architecture.
new_case x86
sums_full 1.2.3 "$CASE/SHA256SUMS"
"$SCRIPT" 1.2.3 "$CASE/SHA256SUMS" "$CASE/PKGBUILD"
grep -qxF "sha256sums_x86_64=('$(hex_of linux-x86)')" "$CASE/PKGBUILD" || fail "x86_64 sum"
ok
grep -qxF "sha256sums_aarch64=('$(hex_of linux-arm)')" "$CASE/PKGBUILD" || fail "aarch64 sum"
ok

# The package is the shape the plan sets.
for want in 'pkgname=playbook-bin' 'pkgver=1.2.3' 'pkgrel=1' "arch=('x86_64' 'aarch64')" \
  "license=('Apache-2.0')" "provides=('playbook')" "conflicts=('playbook')"; do
  grep -qxF "$want" "$CASE/PKGBUILD" || fail "missing line: $want"
done
ok
# shellcheck disable=SC2016
grep -qF 'releases/download/v${pkgver}/playbook-${pkgver}-x86_64-unknown-linux-musl' "$CASE/PKGBUILD" || fail "x86_64 url"
# shellcheck disable=SC2016
grep -qF 'releases/download/v${pkgver}/playbook-${pkgver}-aarch64-unknown-linux-musl' "$CASE/PKGBUILD" || fail "aarch64 url"
grep -qF -- '-Dm755' "$CASE/PKGBUILD" || fail "binary is not installed with mode 755"
grep -qF '/usr/bin/playbook' "$CASE/PKGBUILD" || fail "binary is not installed to /usr/bin/playbook"
ok
# The playbook binary and a pbtop link to it: no pb link, no pbtop archive, no install hook.
# shellcheck disable=SC2016
grep -qF 'ln -s playbook "${pkgdir}/usr/bin/pbtop"' "$CASE/PKGBUILD" || fail "pbtop is not linked to playbook"
! grep -v '^ *#' "$CASE/PKGBUILD" | grep -qE 'pbtop-|pbdoc|\binstall=|/usr/bin/pb\b' || fail "the package ships more than playbook and its pbtop link"
! grep -q '@[A-Z_0-9]*@' "$CASE/PKGBUILD" || fail "a placeholder is left"
ok
bash -n "$CASE/PKGBUILD" || fail "the rendered PKGBUILD is not valid bash"
ok

# Boundary versions.
for v in 0.0.0 999.999.999; do
  new_case "v$v"
  sums_full "$v" "$CASE/SHA256SUMS"
  "$SCRIPT" "$v" "$CASE/SHA256SUMS" "$CASE/PKGBUILD"
  grep -qxF "pkgver=$v" "$CASE/PKGBUILD" || fail "version $v"
  ok
done

# The output is stable: a second run is byte-identical.
new_case stable
sums_full 1.2.3 "$CASE/SHA256SUMS"
"$SCRIPT" 1.2.3 "$CASE/SHA256SUMS" "$CASE/a"
"$SCRIPT" 1.2.3 "$CASE/SHA256SUMS" "$CASE/b"
cmp -s "$CASE/a" "$CASE/b" || fail "two runs differ"
ok

# pkgrel and epoch are kept when given, and epoch is absent otherwise.
! grep -q '^epoch=' "$CASE/a" || fail "epoch rendered by default"
"$SCRIPT" 1.2.3 "$CASE/SHA256SUMS" "$CASE/c" 4 2
grep -qxF 'pkgrel=4' "$CASE/c" || fail "pkgrel not kept"
grep -qxF 'epoch=2' "$CASE/c" || fail "epoch not kept"
ok
new_case badrel
sums_full 1.2.3 "$CASE/SHA256SUMS"
expect_fail 1 'bad pkgrel' 1.2.3 "$CASE/SHA256SUMS" "$CASE/out" x
expect_fail 1 'bad epoch' 1.2.3 "$CASE/SHA256SUMS" "$CASE/out" 1 y

# Look-alike names of another version or with a suffix are ignored.
new_case lookalike
sums_full 1.2.3 "$CASE/SHA256SUMS"
printf '%s  playbook-1.2.30-%s\n%s  playbook-1.2.3-%s.sig\n' "$(hex_of x)" "$T_LINUX_X" "$(hex_of y)" "$T_LINUX_X" >> "$CASE/SHA256SUMS"
"$SCRIPT" 1.2.3 "$CASE/SHA256SUMS" "$CASE/PKGBUILD"
grep -qxF "sha256sums_x86_64=('$(hex_of linux-x86)')" "$CASE/PKGBUILD" || fail "a look-alike name was used"
ok

# Sum failures exit 2.
new_case empty
: > "$CASE/SHA256SUMS"
expect_fail 2 'is empty' 1.2.3 "$CASE/SHA256SUMS" "$CASE/out"

new_case onlyone
sums_full 1.2.3 "$CASE/full"
grep -v "playbook-1.2.3-$T_LINUX_A" "$CASE/full" > "$CASE/SHA256SUMS"
expect_fail 2 "missing target: $T_LINUX_A" 1.2.3 "$CASE/SHA256SUMS" "$CASE/out"

new_case missingx
sums_full 1.2.3 "$CASE/full"
grep -v "playbook-1.2.3-$T_LINUX_X" "$CASE/full" > "$CASE/SHA256SUMS"
expect_fail 2 "missing target: $T_LINUX_X" 1.2.3 "$CASE/SHA256SUMS" "$CASE/out"

new_case malformed
sums_full 1.2.3 "$CASE/full"
sed "s/^[0-9a-f]\{64\}  playbook-1.2.3-$T_LINUX_A\$/abcd  playbook-1.2.3-$T_LINUX_A/" "$CASE/full" > "$CASE/SHA256SUMS"
expect_fail 2 'bad hash' 1.2.3 "$CASE/SHA256SUMS" "$CASE/out"

new_case nohash
sums_full 1.2.3 "$CASE/full"
grep -v "playbook-1.2.3-$T_LINUX_A" "$CASE/full" > "$CASE/SHA256SUMS"
printf 'playbook-1.2.3-%s\n' "$T_LINUX_A" >> "$CASE/SHA256SUMS"
expect_fail 2 'bad hash' 1.2.3 "$CASE/SHA256SUMS" "$CASE/out"

new_case dup
sums_full 1.2.3 "$CASE/SHA256SUMS"
printf '%s  playbook-1.2.3-%s\n' "$(hex_of z)" "$T_LINUX_X" >> "$CASE/SHA256SUMS"
expect_fail 2 "duplicate target: $T_LINUX_X" 1.2.3 "$CASE/SHA256SUMS" "$CASE/out"

new_case nofile
expect_fail 2 'is not a file' 1.2.3 "$CASE/none" "$CASE/out"

# Version failures exit 1, each its own test.
new_case ver
sums_full 1.2.3 "$CASE/SHA256SUMS"
expect_fail 1 'bad version' '' "$CASE/SHA256SUMS" "$CASE/out"
expect_fail 1 'bad version' abc "$CASE/SHA256SUMS" "$CASE/out"
expect_fail 1 'bad version' v1.2.3 "$CASE/SHA256SUMS" "$CASE/out"
expect_fail 1 'bad version' 1.2 "$CASE/SHA256SUMS" "$CASE/out"
expect_fail 1 'bad version' 1.2.3-rc1 "$CASE/SHA256SUMS" "$CASE/out"
expect_fail 1 'usage' 1.2.3 "$CASE/SHA256SUMS"

# keep <version> <PKGBUILD>: the pkgrel and epoch a new render must carry.
# The same version keeps a hand-bumped pkgrel, a new version resets it to 1,
# and the epoch is always kept. No file means pkgrel 1 and no epoch.
new_case keep
printf "epoch=2\npkgname=playbook-bin\npkgver=1.2.3\npkgrel=5\n" > "$CASE/PKGBUILD"
[ "$("$SCRIPT" keep 1.2.3 "$CASE/PKGBUILD")" = "$(printf 'pkgrel=5\nepoch=2')" ] || fail "same version did not keep pkgrel and epoch"
ok
[ "$("$SCRIPT" keep 1.2.4 "$CASE/PKGBUILD")" = "$(printf 'pkgrel=1\nepoch=2')" ] || fail "new version did not reset pkgrel and keep epoch"
ok
printf "pkgname=playbook-bin\npkgver=1.2.3\npkgrel=2\n" > "$CASE/noepoch"
[ "$("$SCRIPT" keep 1.2.3 "$CASE/noepoch")" = "$(printf 'pkgrel=2\nepoch=')" ] || fail "a file without epoch gave one"
ok
[ "$("$SCRIPT" keep 1.2.3 "$CASE/none")" = "$(printf 'pkgrel=1\nepoch=')" ] || fail "no file did not give pkgrel 1"
ok
printf "pkgver=1.2.3\npkgrel=x\n" > "$CASE/bad"
expect_fail 1 'bad pkgrel' keep 1.2.3 "$CASE/bad"
expect_fail 1 'bad version' keep v1 "$CASE/PKGBUILD"
expect_fail 1 'usage' keep 1.2.3
# The kept values feed the render.
kept="$("$SCRIPT" keep 1.2.3 "$CASE/PKGBUILD")"
k_pkgrel="$(sed -n 's/^pkgrel=//p' <<<"$kept")"
k_epoch="$(sed -n 's/^epoch=//p' <<<"$kept")"
sums_full 1.2.3 "$CASE/SHA256SUMS"
"$SCRIPT" 1.2.3 "$CASE/SHA256SUMS" "$CASE/out" "$k_pkgrel" "$k_epoch"
grep -qxF 'pkgrel=5' "$CASE/out" || fail "kept pkgrel not rendered"
grep -qxF 'epoch=2' "$CASE/out" || fail "kept epoch not rendered"
ok

echo "OK render-pkgbuild: $pass checks"
