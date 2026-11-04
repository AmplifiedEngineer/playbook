// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The language server client and what it rests on: process handling, message
//! framing and the byte transport.

pub mod client;
#[cfg(test)]
pub(crate) mod fake;
pub mod framing;
pub mod process;
pub mod transport;
