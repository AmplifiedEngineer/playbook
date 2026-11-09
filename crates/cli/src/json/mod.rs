// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Hand-written Rust replacements for real `jq` call sites across this repo,
//! named per shape/operation rather than one generic query command,
//! extending `src/doctor/field.rs`'s established pattern.

pub mod claudejson;
pub mod count;
pub mod envelope;
pub mod fieldeq;
pub mod fields;
pub mod ghjson;
pub mod jsoncmp;
pub mod jsonl;
pub mod keylist;
pub mod memorycontext;
pub mod settingsjson;
pub mod statusline;
pub mod validate;
