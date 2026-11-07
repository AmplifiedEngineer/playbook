// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The model boundary. The model and the cost meter are injected, so the
//! runner and every test use stubs.

use super::{Arm, E2eTask};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("model call failed: {0}")]
pub struct ModelError(pub String);

/// One call. The snapshot and temperature travel with it so a stub can
/// assert them.
pub struct Request<'a> {
    pub snapshot: &'a str,
    pub temperature: u32,
    pub arm: Arm,
    pub task: &'a E2eTask,
}

pub struct Reply {
    pub text: String,
    pub tokens: u64,
}

pub trait Model {
    fn answer(&self, request: &Request<'_>) -> Result<Reply, ModelError>;
}

/// Total spend so far, in integer cents.
pub trait CostMeter {
    fn spent_cents(&self) -> u64;
}
