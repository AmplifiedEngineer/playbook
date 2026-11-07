#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# asset-names.test.sh: the release asset names that cargo binstall and mise
# depend on must not change by accident.
#
# Usage: asset-names.test.sh
#
# `cargo binstall` builds the download URL from `[package.metadata.binstall]`
# in crates/cli/Cargo.toml, and `mise use github:amplified-engineer/playbook`
# picks an asset by the operating system and architecture words in its name.
# Both need the raw binary of each target at playbook-<version>-<target>[.exe]
# (ADR 0039). This test expands the binstall template for every target in the
# build matrix of release.yml and compares it with the name the workflow gives
# the asset. It also pins the two metadata lines that keep binstall on those
# assets and off the foreign crates.io crate.
set -euo pipefail

cd "$(dirname "$0")/.."

fail() { echo "FAIL $*" >&2; exit 1; }

wf=.github/workflows/release.yml
cargo=crates/cli/Cargo.toml
version=1.2.3

# The targets of the build matrix, and the extension each one has.
targets="$(sed -n '/^  build:/,/^    runs-on:/p' "$wf" | sed -n 's/^ *- target: *//p')"
[ "$(wc -l <<<"$targets" | tr -d ' ')" -eq 5 ] || fail "expected 5 build targets in $wf, got: $targets"
ext_of() {
  sed -n '/^  build:/,/^    runs-on:/p' "$wf" | awk -v t="$1" '
    $1 == "-" && $2 == "target:" { cur = $3; next }
    cur == t && $1 == "ext:" { print $2 }'
}

# The name the workflow gives the raw binary.
line="$(grep -F 'cp "$dir/playbook$EXT" "dist/' "$wf" || true)"
[ -n "$line" ] || fail "$wf no longer copies the playbook binary to dist/"
pattern="$(sed -n 's|.*"dist/\(.*\)"$|\1|p' <<<"$line")"
[ "$pattern" = 'playbook-$VERSION-$TARGET$EXT' ] \
  || fail "the workflow names the binary '$pattern'. Change [package.metadata.binstall] in $cargo, the installers and the docs with it."

# The binstall template.
pkg_url="$(sed -n 's/^pkg-url *= *"\(.*\)"$/\1/p' "$cargo")"
[ -n "$pkg_url" ] || fail "$cargo has no [package.metadata.binstall] pkg-url"
[ "$(sed -n 's/^pkg-fmt *= *"\(.*\)"$/\1/p' "$cargo")" = bin ] || fail "$cargo pkg-fmt must be bin, the asset is a raw binary"
repo="$(sed -n 's/^repository *= *"\(.*\)"$/\1/p' "$cargo")"
[ -n "$repo" ] || fail "$cargo has no repository, which { repo } needs"

grep -qxF 'disabled-strategies = ["quick-install"]' "$cargo" \
  || fail "$cargo must turn the quick-install strategy off, the crates.io name playbook is another project's"

while IFS= read -r target; do
  ext="$(ext_of "$target")"
  want="playbook-$version-$target$ext"
  url="$pkg_url"
  url="${url//\{ repo \}/$repo}"
  url="${url//\{ version \}/$version}"
  url="${url//\{ target \}/$target}"
  url="${url//\{ binary-ext \}/$ext}"
  case "$url" in *"{"*) fail "pkg-url keeps a placeholder binstall may not fill: $url" ;; esac
  [ "$url" = "$repo/releases/download/v$version/$want" ] \
    || fail "binstall asks for $url, the workflow uploads $want"
  # mise picks by these words, so every target must carry them.
  case "$target" in
    *linux* | *apple-darwin | *windows*) ;;
    *) fail "$target has no operating system word mise can match" ;;
  esac
  case "$target" in x86_64-* | aarch64-*) ;; *) fail "$target has no architecture word mise can match" ;; esac
done <<<"$targets"

echo "ok: binstall and mise find playbook-<version>-<target> for all $(wc -l <<<"$targets" | tr -d ' ') targets"
