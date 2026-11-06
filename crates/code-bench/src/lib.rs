// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Dev-only benchmark for the code index. Nothing in the shipped binary
//! depends on this crate; `tests/dependency_tree.rs` keeps it that way.

pub mod corpus;
pub mod ctags;
pub mod gate;
pub mod merge_base;
pub mod metrics;
pub mod oracle;
pub mod process;
pub mod runner;
pub mod scope;
pub mod tasks;
pub mod tokenizer;
