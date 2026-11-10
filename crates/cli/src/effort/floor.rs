// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The quality floor table (ADR-0022). For each agent it records the lowest
//! effort the maintainer accepts, whether a missed finding is costly, whether
//! the agent is dispatched many times per task on purpose, and where the
//! number came from.
//!
//! This table is the only thing that grants permission to lower effort.
//! Usage evidence can trigger or veto a proposal and never grants one.
//!
//! A floor is the lowest level that held quality in the benchmark campaign of
//! 2026-10-10 (rule: pass rate at least 95% and within 3 points of the best
//! setting, and seeded finding recall within 3 points for review agents). When
//! the bench is thin (single turn with no tools, a pilot of a few cases, or a
//! result that is not monotonic across levels) the floor is pinned to the
//! shipped effort, so nothing is proposed. Every agent is pinned to its shipped
//! effort today. A row moves below shipped only when a larger bench says so,
//! and `tests/effort_floor.rs` checks that every agent has a row and that no
//! floor is above the effort in the agent's file.

/// One agent's floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Floor {
    pub agent: &'static str,
    /// The lowest effort the tuner may propose.
    pub floor: &'static str,
    /// A missed finding is the costly error, so the tuner never proposes lowering.
    pub costly_to_miss: bool,
    /// Dispatched several times per task on purpose (one per lens, one per Work
    /// Unit), so repeats are not reruns.
    pub fan_out: bool,
    /// The benchmark run and the document that records it.
    pub source: &'static str,
}

const DOC: &str = "docs/internals/02-model-routing-and-memory.md";

/// Every agent, sorted by name.
pub const FLOORS: [Floor; 11] = [
    Floor {
        agent: "analyst",
        floor: "medium",
        costly_to_miss: false,
        fan_out: false,
        source: "bench campaign 2026-10-10 (#714): Haiku low 45/45 and medium 45/45 on 3 cases, pinned at shipped for a thin bench; see the policy table in docs/internals/02-model-routing-and-memory.md",
    },
    Floor {
        agent: "auditor",
        floor: "high",
        costly_to_miss: true,
        fan_out: false,
        source: "no headless bench exists; pinned at shipped (bench campaign report, kept list)",
    },
    Floor {
        agent: "cheap-checker",
        floor: "low",
        costly_to_miss: false,
        fan_out: true,
        source: "already the lowest effort; kept by the bench campaign (#714); see the policy table in docs/internals/02-model-routing-and-memory.md",
    },
    Floor {
        agent: "collector",
        floor: "low",
        costly_to_miss: false,
        fan_out: true,
        source: "already the lowest effort; kept by the bench campaign (#714); see the policy table in docs/internals/02-model-routing-and-memory.md",
    },
    Floor {
        agent: "critic",
        floor: "medium",
        costly_to_miss: false,
        fan_out: false,
        source: "bench campaign 2026-10-10 (#714): Sonnet low 89/90 and medium 89/90 on 6 plans, high 71/90; thin bench, pinned at shipped medium",
    },
    Floor {
        agent: "fact-checker",
        floor: "medium",
        costly_to_miss: false,
        fan_out: false,
        source: "bench campaign 2026-10-10 (#714): Haiku medium 134/138 and high 137/138 (low 86/90, over 3 points under high); see the policy table in docs/internals/02-model-routing-and-memory.md",
    },
    Floor {
        agent: "implementer",
        floor: "medium",
        costly_to_miss: false,
        fan_out: true,
        source: "bench campaign 2026-10-10 (#712, #714): Sonnet low 99.7% and medium 98.7% on 20 repo tasks; single turn bench with no tools, so pinned at shipped medium",
    },
    Floor {
        agent: "review-triage",
        floor: "low",
        costly_to_miss: false,
        fan_out: false,
        source: "already the lowest effort; a bad answer falls back to a full lens (bench campaign #714)",
    },
    Floor {
        agent: "reviewer",
        floor: "medium",
        costly_to_miss: true,
        fan_out: true,
        source: "ADR-0021 and the reviewer bench (#716): Opus medium 199/200 against high 200/200, low 49/50 on a 5 run round; only 7 distinct seeded bugs, so no lower floor",
    },
    Floor {
        agent: "test-reviewer",
        floor: "low",
        costly_to_miss: false,
        fan_out: false,
        source: "already the lowest effort; Haiku low 86/90, misses were false alarms on clean controls (bench campaign #714)",
    },
    Floor {
        agent: "writer",
        floor: "medium",
        costly_to_miss: false,
        fan_out: true,
        source: "writer pilot 2026-11-06: Haiku medium, 18 cases, 0 invented facts and 0 hard prose findings; a thin pilot, so pinned at the shipped medium",
    },
];

/// The row for `agent`, if the table has one.
pub fn floor_of(agent: &str) -> Option<&'static Floor> {
    FLOORS.iter().find(|f| f.agent == agent)
}

/// Where the numbers are explained for a human.
pub const DOC_PATH: &str = DOC;
