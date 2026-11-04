// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The language server client and what it rests on: process handling, message
//! framing and the byte transport.

pub mod archive;
pub mod client;
pub mod discover;
#[cfg(test)]
pub(crate) mod fake;
pub mod framing;
pub mod manifest;
pub mod process;
pub mod pyenv;
pub mod registry;
pub mod session;
pub mod status;
#[cfg(test)]
pub(crate) mod testkit;
pub mod transport;
pub mod uri;
