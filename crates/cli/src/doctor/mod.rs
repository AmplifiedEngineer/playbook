// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Backs `playbook doctor`. `check` runs the layer checks that
//! `commands/doctor.md` used to run as bash blocks, and `field` holds the small
//! JSON field reads they need and used to get from `jq`.

pub mod build;
pub mod check;
pub mod field;
pub mod launchers;
pub mod pbtop;
pub mod style;
pub mod sync;
