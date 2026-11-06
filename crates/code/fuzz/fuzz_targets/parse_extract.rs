// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

#![no_main]

use libfuzzer_sys::fuzz_target;
use playbook_code::lang;
use playbook_code_fuzz::split_input;

fuzz_target!(|data: &[u8]| {
    let Some((language, bytes)) = split_input(data) else {
        return;
    };
    if let Ok(parsed) = lang::parse(language, bytes) {
        let extracted = lang::extract(language, &parsed, bytes);
        for symbol in &extracted.symbols {
            assert!(symbol.start_byte <= symbol.end_byte);
            assert!(symbol.end_byte <= bytes.len());
        }
    }
});
