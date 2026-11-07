#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# bump-version.sh <version>: sets the version of a playbook release (ADR 0038).
# The binary and the plugin share one version line, so it writes the same
# MAJOR.MINOR.PATCH to:
#   - the workspace version in Cargo.toml, and the Cargo.lock entry of every
#     crate that takes it with `version.workspace = true`;
#   - version in the plugin's plugin.json, and metadata.minBinaryVersion as
#     the X.Y.0 of the same minor line;
#   - the PLAYBOOK_REF pins in README.md and docs/guides/00-install.md.
# The version must be above the current one, since Claude Code updates the
# plugin only when its version rises. Every file is written to a temporary
# copy first and replaced only when all of them are ready, then
# scripts/check-plugin.sh version checks the result. Run it in the release
# pull request.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MANIFEST="plugins/playbook/.claude-plugin/plugin.json"
PINS=(README.md docs/guides/00-install.md)
cd "$ROOT"

fail() { echo "FAIL $*" >&2; exit 1; }

new="${1:-}"
[ -n "$new" ] || { echo "usage: bump-version.sh <version>" >&2; exit 2; }
# shellcheck source=scripts/version-lib.sh
. scripts/version-lib.sh
printf '%s' "$new" | grep -Eq "$PLAIN_VERSION" || fail "version '$new' is not MAJOR.MINOR.PATCH"

cargo_now="$(scripts/check-plugin.sh cargo-version)" || fail "${cargo_now#FAIL }"
plugin_now="$(jq -r '.version // empty' "$MANIFEST")"
version_gt "$new" "$cargo_now" || fail "$new is not above the current version $cargo_now"
version_gt "$new" "$plugin_now" || fail "$new is not above the plugin version $plugin_now"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

awk -v v="$new" '/^\[workspace.package\]/ { on = 1 } /^\[/ && !/^\[workspace.package\]/ { on = 0 }
  on && /^version = / { print "version = \"" v "\""; on = 0; next } { print }' Cargo.toml > "$tmp/Cargo.toml"

# The lock entries to bump are the crates that take the workspace version, so a
# new crate cannot be missed and make `cargo --locked` fail.
names="$(for m in crates/*/Cargo.toml; do
  grep -q '^version.workspace = true' "$m" || continue
  awk -F'"' '/^name = / { print $2; exit }' "$m"
done | paste -sd'|' -)"
[ -n "$names" ] || fail "no crate uses version.workspace = true"

awk -v v="$new" -v names="$names" '$0 ~ "^name = \"(" names ")\"$" { hit = 1; print; next }
  hit && /^version = / { print "version = \"" v "\""; hit = 0; next } { hit = 0; print }' Cargo.lock > "$tmp/Cargo.lock"

jq --arg v "$new" --arg min "${new%.*}.0" '.version = $v | .metadata.minBinaryVersion = $min' \
  "$MANIFEST" > "$tmp/plugin.json"

for f in "${PINS[@]}"; do
  out="$tmp/$(basename "$f")"
  sed -E "s/PLAYBOOK_REF=v[0-9]+\.[0-9]+\.[0-9]+/PLAYBOOK_REF=v$new/g" "$f" > "$out"
  grep -q "PLAYBOOK_REF=v$new" "$out" || fail "$f has no PLAYBOOK_REF=vX.Y.Z pin to update"
done

# Every copy is ready, so replace the files.
cat "$tmp/Cargo.toml" > Cargo.toml
cat "$tmp/Cargo.lock" > Cargo.lock
cat "$tmp/plugin.json" > "$MANIFEST"
for f in "${PINS[@]}"; do cat "$tmp/$(basename "$f")" > "$f"; done

scripts/check-plugin.sh version >/dev/null || fail "the files disagree after the bump; run scripts/check-plugin.sh version"
grep -Eq "^version = \"$new\"$" Cargo.lock || fail "Cargo.lock has no playbook entry at $new"
echo "Set the version to $new in Cargo.toml, Cargo.lock, $MANIFEST and ${PINS[*]}."
