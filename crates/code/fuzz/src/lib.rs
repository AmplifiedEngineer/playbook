// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shared knobs of the fuzz targets and their callers.

/// Seconds one target runs in a pull request smoke job (`-max_total_time`).
pub const SMOKE_SECONDS: u64 = 60;
/// Seconds one target runs in the nightly job (`-max_total_time`).
pub const NIGHTLY_SECONDS: u64 = 600;
/// Seconds one input may take before libFuzzer reports a hang (`-timeout`).
pub const INPUT_TIMEOUT_SECONDS: u64 = 10;
/// Longest input a target takes, in bytes (`-max_len`).
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Splits an input into a language pick and the file bytes. The first byte
/// chooses the language, so one corpus serves every grammar.
pub fn split_input(data: &[u8]) -> Option<(playbook_code::lang::Lang, &[u8])> {
    let (pick, rest) = data.split_first()?;
    let langs = playbook_code::lang::ALL;
    Some((langs[usize::from(*pick) % langs.len()], rest))
}
