// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0
//
// pick-pr-to-update.js: the decision behind .github/workflows/auto-update-pr.yml.
// Pure, no I/O, so scripts/pick-pr-to-update.test.sh can cover it offline.
//
// Input: armed PRs, oldest first, each { number, behind_by, checks_pending, draft? }.
// Output: the number of the one PR to update, or null when nothing should move.
//
// A PR that is up to date and still waiting on checks is in flight. While one
// is in flight nobody else is updated, so one CI run is live at a time and a
// merge from the queue does not make a second PR stale before the first lands.
'use strict';

function pickPrToUpdate(prs) {
  const open = prs.filter((pr) => !pr.draft);
  if (open.some((pr) => pr.behind_by === 0 && pr.checks_pending)) return null;
  const next = open.find((pr) => pr.behind_by > 0);
  return next ? next.number : null;
}

module.exports = { pickPrToUpdate };
