// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The language server client: message framing and the byte transport under it.

#[cfg(test)]
pub(crate) mod fake;
pub mod framing;
pub mod transport;
