// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr`: the mechanical half of `/playbook:create-pull-request`.

pub mod body_rules;
pub mod comments;
pub mod create;
pub mod guard;
pub mod prepare;
pub mod reply;
pub mod rules;
pub mod shared;
pub mod stack;
pub mod stack_context;
pub mod stack_digest;
pub mod stack_map;
pub mod triage;
pub mod wait;
