// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Watch mode for GitHub events (ADR 0055): rules, the autonomy ceiling, and
//! the pieces a polling engine is built from. Each submodule is exercised
//! through its public API; the `playbook watch` subcommand that calls them
//! lands in a later change.

pub mod audit;
pub mod clock;
pub mod event;
pub mod github;
pub mod lock;
pub mod rule;
pub mod settings;
pub mod source;
pub mod store;
