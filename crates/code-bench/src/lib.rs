// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Dev-only benchmark for the code index. Nothing in the shipped binary
//! depends on this crate; `tests/dependency_tree.rs` keeps it that way.

pub mod corpus;
pub mod metrics;
pub mod tokenizer;
