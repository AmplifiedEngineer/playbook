#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# workflow-egress.test.sh: offline check of the harden-runner allowlists.
#
# Usage: workflow-egress.test.sh
#
# A job with `egress-policy: block` that runs `gh attestation verify` must
# allow every host that command calls. Run 38880075330 failed when the
# publish job could not reach the storage that holds the attestation
# bundles. The check reads each workflow as text, one job at a time, and
# exits 1 with one line per missing host. It also runs itself on a fixture
# that lacks a host, so a broken check fails too.
set -euo pipefail

cd "$(dirname "$0")/.."

# The hosts `gh attestation verify` calls, seen in the egress log of the
# release run.
NEEDS='api.github.com:443 tuf-repo.github.com:443 tuf-repo-cdn.sigstore.dev:443 tmaproduction.blob.core.windows.net:443'

# For one workflow file, prints "checked <file>: <job>" for each job it
# checks, then one line per missing host.
gaps() {
  awk -v needs="$NEEDS" -v file="$1" '
    function flush(   n, i, list) {
      if (job != "" && block && verify) {
        print "checked " file ": " job
        n = split(needs, list, " ")
        for (i = 1; i <= n; i++)
          if (index(hosts, " " list[i] " ") == 0)
            print file ": " job " runs gh attestation verify without " list[i]
      }
      block = 0; verify = 0; hosts = " "
    }
    /^jobs:/ { injobs = 1; next }
    injobs && /^  [A-Za-z0-9_-]+:/ { flush(); job = $1; sub(/:$/, "", job); next }
    /egress-policy: *["'\'']?block["'\'']? *$/ { block = 1 }
    /gh attestation verify/ { verify = 1 }
    /^ +[a-z0-9.-]+:[0-9]+ *$/ { hosts = hosts $1 " " }
    END { flush() }
  ' "$1"
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cat > "$tmp/fixture.yml" <<'EOF'
jobs:
  ok:
    steps:
      - uses: step-security/harden-runner
        with:
          egress-policy: block
          allowed-endpoints: >
            api.github.com:443
            tuf-repo.github.com:443
            tuf-repo-cdn.sigstore.dev:443
            tmaproduction.blob.core.windows.net:443
      - run: gh attestation verify a --repo r
  gap:
    steps:
      - uses: step-security/harden-runner
        with:
          egress-policy: block
          allowed-endpoints: >
            api.github.com:443
            tuf-repo.github.com:443
            tuf-repo-cdn.sigstore.dev:443
      - run: gh attestation verify a --repo r
  quoted:
    steps:
      - uses: step-security/harden-runner
        with:
          egress-policy: 'block'
          allowed-endpoints: >
            api.github.com:443
            tuf-repo.github.com:443
            tuf-repo-cdn.sigstore.dev:443
      - run: gh attestation verify a --repo r
EOF
want="checked $tmp/fixture.yml: ok
checked $tmp/fixture.yml: gap
$tmp/fixture.yml: gap runs gh attestation verify without tmaproduction.blob.core.windows.net:443
checked $tmp/fixture.yml: quoted
$tmp/fixture.yml: quoted runs gh attestation verify without tmaproduction.blob.core.windows.net:443"
got="$(gaps "$tmp/fixture.yml")"
if [ "$got" != "$want" ]; then
  echo "FAIL: the check missed the fixture gap. Got: '$got'" >&2
  exit 1
fi

fail=0
checked=""
for f in .github/workflows/*.yml; do
  out="$(gaps "$f")"
  checked="$checked$(grep '^checked ' <<<"$out" || true)"$'\n'
  missing="$(grep -v '^checked ' <<<"$out" || true)"
  if [ -n "$missing" ]; then
    echo "$missing" >&2
    fail=1
  fi
done
# The jobs known to run gh attestation verify. When the call moves out of
# the workflow text, the check would see nothing, so it fails instead.
for job in publish verify nix; do
  grep -qx "checked .github/workflows/release.yml: $job" <<<"$checked" \
    || { echo "FAIL: the check did not find release.yml job $job." >&2; fail=1; }
done
[ "$fail" -eq 0 ] || { echo "FAIL: add the hosts above to the job's allowed-endpoints." >&2; exit 1; }
echo "ok: every job that runs gh attestation verify allows its hosts"
