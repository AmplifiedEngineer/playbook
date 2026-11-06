#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# pick-pr-to-update.test.sh: offline tests for pick-pr-to-update.js.
#
# Usage: pick-pr-to-update.test.sh
set -euo pipefail

cd "$(dirname "$0")/.."

exec node -e '
const assert = require("node:assert/strict");
const { pickPrToUpdate: pick } = require("./scripts/pick-pr-to-update.js");
const pr = (number, behind_by, checks_pending = false, extra = {}) =>
  ({ number, behind_by, checks_pending, ...extra });

const cases = [
  ["none armed", [], null],
  ["one behind", [pr(1, 3)], 1],
  ["one up to date and green", [pr(1, 0)], null],
  ["in flight blocks the rest", [pr(1, 0, true), pr(2, 4)], null],
  ["in flight blocks even when it is not first", [pr(1, 2), pr(2, 0, true)], null],
  ["oldest behind first", [pr(5, 1), pr(6, 9), pr(7, 2)], 5],
  ["up to date and green is skipped", [pr(1, 0), pr(2, 3)], 2],
  ["draft skipped", [pr(1, 5, false, { draft: true }), pr(2, 1)], 2],
  ["draft in flight does not block", [pr(1, 0, true, { draft: true }), pr(2, 1)], 2],
  ["only drafts", [pr(1, 5, false, { draft: true })], null],
];
for (const [name, input, want] of cases) {
  assert.equal(pick(input), want, name);
  console.log("ok " + name);
}
'
