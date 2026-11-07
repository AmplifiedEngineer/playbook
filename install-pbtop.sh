#!/bin/sh
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# Installer for pbtop alone, the standalone Agent Budget Dashboard, on a machine
# without playbook. The README shows the curl command that pipes it to sh.
#
# It runs install.sh in its pbtop mode. That fetches
# pbtop-<version>-<target>.tar.gz from the latest release, or from the
# release named by PLAYBOOK_REF, checks its line in SHA256SUMS, its build
# attestation when gh can check it, and `pbtop --version`, then installs pbtop
# into PLAYBOOK_BIN_DIR (default $HOME/.local/bin) and puts that folder on
# PATH. Nothing else runs. Run from a checkout, it uses the install.sh next
# to it when that is playbook's and knows the pbtop mode, else the one on
# main. Homebrew has the same binary as the `pbtop`
# formula of amplifiedengineer/tap.
set -eu

command -v bash >/dev/null 2>&1 || { echo "error: bash is required" >&2; exit 1; }

case "$0" in
    install-pbtop.sh | */install-pbtop.sh)
        here="$(cd "$(dirname "$0")" && pwd)"
        local_sh="$here/install.sh"
        if [ -f "$local_sh" ] &&
            grep -Eq '^RELEASE_REPO="(amplifiedengineer|amplified-engineer|pragmatic-engineer)/playbook"$' "$local_sh" &&
            grep -q PLAYBOOK_INSTALL_ONLY "$local_sh"; then
            PLAYBOOK_INSTALL_ONLY=pbtop exec bash "$local_sh" "$@"
        fi
        ;;
esac

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }
url="https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh"
script="$(curl -fsSL "$url")" || { echo "error: could not download $url" >&2; exit 1; }
PLAYBOOK_INSTALL_ONLY=pbtop exec bash -c "$script" install.sh "$@"
