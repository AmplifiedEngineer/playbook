// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pinned token counter. The usual BPE crates ship large vocabulary data
//! and many dependencies, and the gate only needs a count that never changes
//! between runs. This counter is integer arithmetic over the text, so it is
//! identical on every machine. Changing the rule means a new `TOKENIZER_ID`
//! and a new baseline.

/// The id pinned in `corpus.toml`.
pub const TOKENIZER_ID: &str = "playbook-lexical-v1";

/// A run of word characters costs one token per this many characters.
const WORD_CHARS_PER_TOKEN: u64 = 4;

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Counts tokens. A run of word characters costs one token per
/// `WORD_CHARS_PER_TOKEN` characters, rounded up. Every other character that
/// is not whitespace costs one token. Whitespace is free.
pub fn count_tokens(text: &str) -> u64 {
    let mut tokens = 0;
    let mut run: u64 = 0;
    for c in text.chars() {
        if is_word_char(c) {
            run += 1;
            continue;
        }
        tokens += run.div_ceil(WORD_CHARS_PER_TOKEN);
        run = 0;
        if !c.is_whitespace() {
            tokens += 1;
        }
    }
    tokens + run.div_ceil(WORD_CHARS_PER_TOKEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_has_no_tokens() {
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn word_run_rounds_up_per_four_characters() {
        assert_eq!(count_tokens("abcd"), 1);
        assert_eq!(count_tokens("abcde"), 2);
        assert_eq!(count_tokens("abcdefgh"), 2);
    }

    #[test]
    fn punctuation_costs_one_and_whitespace_is_free() {
        assert_eq!(count_tokens("fn a() {}"), 6);
        assert_eq!(count_tokens("  \n\t"), 0);
    }

    #[test]
    fn count_is_stable_for_the_same_text() {
        let text = "let snake_case = 42;";
        assert_eq!(count_tokens(text), count_tokens(text));
    }
}
