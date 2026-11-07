// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `pbsync` pushes opt-in usage data to a Turso database over SQL-over-HTTP.
//! The transport is the only layer that touches the network.

pub mod privacy;
pub mod push;
pub mod remote;
pub mod tables;
pub mod token;
pub mod transport;
