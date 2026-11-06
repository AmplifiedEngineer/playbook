// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The prose lint: the banned phrases, structures and dashes of the
//! writing-style skill, as one table of patterns. It runs on commit messages,
//! PR and issue text (the `commit-message-sanitizer` hook) and on files
//! (`playbook prose check`).
//!
//! The phrase and structure lists follow the stop-slop skill of
//! `wunki/amplify` (https://github.com/wunki/amplify/tree/main/skills/stop-slop),
//! which the writing-style skill adopts.
//!
//! Text that is not prose is masked before matching: fenced code, inline code,
//! URLs, file paths, quoted lines (`> `), shell prompt lines (`$ `), indented
//! code and `Signed-off-by` and `Co-authored-by` trailers. A finding is
//! reported on the line where its text starts.

use super::prose::BANNED;
use super::re::static_regex;
use regex::Regex;
use std::sync::LazyLock;

/// The kind of rule a pattern belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Dash,
    ThroatClearing,
    EmphasisCrutch,
    Jargon,
    FillerAdverb,
    MetaCommentary,
    Performative,
    TellingNotShowing,
    BinaryContrast,
    DramaticFragmentation,
    RhetoricalSetup,
    Formulaic,
    SoOpener,
    LookOpener,
    BannedWord,
    AbsoluteWord,
    Intensifier,
    ThreeItemList,
    PassiveVoice,
    UnknownToken,
    OverLimit,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Dash => "em or en dash",
            Class::ThroatClearing => "throat-clearing opener",
            Class::EmphasisCrutch => "emphasis crutch",
            Class::Jargon => "business jargon",
            Class::FillerAdverb => "filler phrase",
            Class::MetaCommentary => "meta commentary",
            Class::Performative => "performative emphasis",
            Class::TellingNotShowing => "telling instead of showing",
            Class::BinaryContrast => "binary contrast",
            Class::DramaticFragmentation => "dramatic fragmentation",
            Class::RhetoricalSetup => "rhetorical setup",
            Class::Formulaic => "formulaic construction",
            Class::SoOpener => "paragraph starting with So",
            Class::LookOpener => "sentence starting with Look",
            Class::BannedWord => "banned word",
            Class::AbsoluteWord => "absolute word",
            Class::Intensifier => "intensifier",
            Class::ThreeItemList => "three-item list",
            Class::PassiveVoice => "passive voice",
            Class::UnknownToken => "token not in the facts",
            Class::OverLimit => "over the length limit",
        }
    }

    /// Hard rules name a phrase or a structure the skill bans outright. Soft
    /// rules are word-level heuristics that fit technical text too, so they
    /// only ever warn, even when the mode is `block`.
    pub fn hard(self) -> bool {
        !matches!(
            self,
            Class::AbsoluteWord | Class::Intensifier | Class::ThreeItemList | Class::PassiveVoice
        )
    }

    fn advice(self) -> &'static str {
        match self {
            Class::Dash => "Use a period, a comma or a colon.",
            Class::ThroatClearing | Class::EmphasisCrutch | Class::MetaCommentary => {
                "Cut it and state the point."
            }
            Class::Jargon => "Use a plain word.",
            Class::FillerAdverb | Class::Performative | Class::TellingNotShowing => {
                "Cut it or say the fact."
            }
            Class::BinaryContrast => "State the point directly.",
            Class::DramaticFragmentation => "Write a complete sentence.",
            Class::RhetoricalSetup => "Make the point without the setup.",
            Class::Formulaic => "Say it directly.",
            Class::SoOpener | Class::LookOpener => "Start with the content.",
            Class::BannedWord => "Say it plainly.",
            Class::AbsoluteWord => "Name the real scope, or keep it only in a technical rule.",
            Class::Intensifier => "Cut the intensifier.",
            Class::ThreeItemList => "Use two items or one, unless all three are required.",
            Class::PassiveVoice => "Name who acts and use an active verb.",
            Class::UnknownToken => "Cut it, or use the value from the facts.",
            Class::OverLimit => "Cut the text to the limit.",
        }
    }
}

/// One pattern of the table. `pattern` carries its own flags.
pub struct Pattern {
    pub class: Class,
    pub pattern: &'static str,
}

const fn p(class: Class, pattern: &'static str) -> Pattern {
    Pattern { class, pattern }
}

/// Every pattern, grouped by class. A dot stands for an apostrophe so the
/// curly form matches too. Pattern strings are lowercase and case-insensitive
/// unless a pattern needs capitals.
pub const PATTERNS: &[Pattern] = &[
    p(Class::Dash, r"[\x{2013}\x{2014}]"),
    p(
        Class::ThroatClearing,
        r"(?i)here.s the thing|the uncomfortable truth is|\bit turns out\b|\bthe real \w+ is\b|let me be clear|\bthe truth is,|i.ll say it again|i.m going to be honest|can we talk about|here.s what i find interesting|here.s the problem though",
    ),
    p(
        Class::EmphasisCrutch,
        r"(?i)\bfull stop\.|(?:^|[.!?]\s)period\.|let that sink in|\bthis matters because\b|make no mistake|here.s why that matters",
    ),
    p(
        Class::Jargon,
        r"(?i)\bnavigat\w* (?:the )?(?:challenges?|uncertaint\w+|complexit\w+|landscape)\b|\bunpack (?:this|that|what)\b|\blean(?:s|ed|ing)? into\b|game-changer|\bdouble down\b|\bdeep dive\b|\btake a step back\b|\bmoving forward\b|\bcircle back\b|\bon the same page\b",
    ),
    p(
        Class::FillerAdverb,
        r"(?i)\bat its core\b|\bin today.s\b|it.s worth noting|(?:^|[.!?]\s)(?:interestingly|importantly|crucially),|at the end of the day|\bwhen it comes to\b|in a world where|\bthe reality is\b",
    ),
    p(
        Class::MetaCommentary,
        r"(?i)plot twist|(?:^|[.!?]\s)(?:spoiler|hint):|you already know this, but|but that.s another post|is a feature, not a bug|\bdressed up as\b",
    ),
    p(Class::Performative, r"(?i)\bcreeps in\b|\bi promise\b"),
    p(
        Class::TellingNotShowing,
        r"(?i)this is genuinely hard|what leadership actually looks like|this is what \w+ actually looks like",
    ),
    p(
        Class::BinaryContrast,
        r"(?i)\bnot because\b[^\n]{1,80}?[.;]\s+because\b|\b(?:answer|question|problem) (?:isn.t|is not) [^.\n]{1,60}\.\s+(?:it.s|the \w+ is)|\bit feels like [^.\n]{1,60}\.\s+it.s actually\b|(?:^|[.!?]\s)not [^.\n]{1,40}\.\s+but\b|\bit.s not [^.\n]{1,60}[.;]\s+it.s\b|\bstops? being\b[^.\n]{1,50}\band starts? being\b|\bnot just\b[^.\n]{0,80}\bbut (?:also )?\b|(?:^|[.!?]\s)(?:it|this|that)(?:.s| is| was) not (?:about )?[^.\n;,]{1,60}[,;]\s+(?:it|this|that)(?:.s| is| was)\b|(?:^|[.!?]\s)(?:it|this|that) (?:isn.t|wasn.t) (?:about )?[^.\n;,]{1,60}[,;]\s+(?:it|this|that)(?:.s| is| was)\b",
    ),
    p(
        Class::DramaticFragmentation,
        r"(?i)\bthat.s it\.\s+that.s (?:the|all)\b",
    ),
    p(
        Class::DramaticFragmentation,
        r"(?:^|[.!?][ \t])[A-Z][\w']*\.[ \t]+[A-Z][\w']*\.[ \t]+[A-Z][\w']*\.",
    ),
    p(
        Class::RhetoricalSetup,
        r"(?i)what if i told you|(?:^|[.!?]\s)what if [^.\n]{1,80}\?|here.s what i mean|\bthink about it\b|\band that.s okay\b",
    ),
    p(Class::Formulaic, r"(?i)\bby the time [^,\n]{1,40}, i was\b"),
    p(Class::SoOpener, r"(?i)(?:\A|\n[ \t]*\n)[ \t]*so[ ,]"),
    p(Class::LookOpener, r"(?i)(?:^|[.!?]\s)look,"),
    p(
        Class::AbsoluteWord,
        r"(?i)\b(?:always|never|everyone|everybody|nobody)\b",
    ),
    p(
        Class::Intensifier,
        r"(?i)\b(?:deeply|truly|fundamentally|inherently|simply|literally|inevitably)\b",
    ),
    p(
        Class::ThreeItemList,
        r"(?i)\b[\w-]+, [\w-]+,? (?:and|or) [\w-]+\b",
    ),
];

struct Compiled {
    class: Class,
    regex: Regex,
}

static TABLE: LazyLock<Vec<Compiled>> = LazyLock::new(|| {
    let mut table: Vec<Compiled> = PATTERNS
        .iter()
        .map(|pat| Compiled {
            class: pat.class,
            regex: static_regex(&format!("(?m){}", pat.pattern)),
        })
        .collect();
    table.push(Compiled {
        class: Class::BannedWord,
        regex: static_regex(&format!(r"(?i)\b(?:{})\b", BANNED.join("|"))),
    });
    table
});

static CODE_SPAN: LazyLock<Regex> = LazyLock::new(|| static_regex(r"`[^`\n]*`"));
static URL: LazyLock<Regex> = LazyLock::new(|| static_regex(r"https?://\S+"));
static PATH: LazyLock<Regex> = LazyLock::new(|| static_regex(r"[\w.~*-]*(?:/[\w.*~-]+)+"));

/// A form of "be", up to two light adverbs, then the next word. Only the
/// next word decides whether the phrase is a passive (`is_participle`).
static PASSIVE: LazyLock<Regex> = LazyLock::new(|| {
    static_regex(
        r"(?i)\b(is|are|am|was|were|be|been|being)\s+(?:(?:not|also|now|then|already|only|still|never|always|just|later|first|again|often)\s+)?([a-z]+)\b",
    )
});

/// Past participles that do not end in "ed".
const IRREGULAR_PARTICIPLES: &[&str] = &[
    "done",
    "made",
    "given",
    "taken",
    "shown",
    "known",
    "written",
    "built",
    "found",
    "kept",
    "sent",
    "seen",
    "chosen",
    "driven",
    "held",
    "lost",
    "thrown",
    "told",
    "understood",
    "begun",
    "drawn",
    "hidden",
    "spoken",
    "stolen",
    "torn",
    "worn",
    "broken",
    "left",
];

/// Words that end in "ed" and are not participles, or name a feeling. "Is
/// tired" and "were interested" describe a state of a person, not an action
/// done to a thing.
const NOT_PARTICIPLES: &[&str] = &[
    "hundred",
    "naked",
    "wicked",
    "sacred",
    "hatred",
    "kindred",
    "tired",
    "excited",
    "interested",
    "concerned",
    "surprised",
    "pleased",
    "worried",
    "annoyed",
    "bored",
    "scared",
];

/// Participles that read as a status when they follow a present-tense "is",
/// "are" or "am": "the flag is deprecated", "the PR is merged". The same
/// word after "was", "were", "be", "been" or "being" is an event and is
/// still flagged ("the flag was deprecated", "will be merged").
const STATUS_WORDS: &[&str] = &[
    "deprecated",
    "required",
    "supported",
    "unsupported",
    "enabled",
    "disabled",
    "merged",
    "unmerged",
    "closed",
    "expired",
    "named",
    "called",
    "based",
    "located",
    "installed",
    "configured",
    "signed",
    "pinned",
    "locked",
    "stuck",
    "unused",
    "unchanged",
    "archived",
    "released",
    "unreleased",
    "ignored",
    "done",
    "broken",
    "known",
];

fn is_participle(word: &str) -> bool {
    if NOT_PARTICIPLES.contains(&word) {
        return false;
    }
    IRREGULAR_PARTICIPLES.contains(&word)
        || (word.len() >= 4 && word.ends_with("ed") && !word.ends_with("eed"))
}

/// The passive-voice findings of the masked text. A heuristic: a form of
/// "be" followed by a past participle. It skips what it cannot tell apart
/// from an adjective, so some passives pass.
fn passive_findings(masked: &str, newlines: &[usize]) -> Vec<Finding> {
    let mut found = Vec::new();
    for caps in PASSIVE.captures_iter(masked) {
        let be = caps[1].to_lowercase();
        let word = caps[2].to_lowercase();
        if !is_participle(&word) {
            continue;
        }
        let present = matches!(be.as_str(), "is" | "are" | "am");
        if present && STATUS_WORDS.contains(&word.as_str()) {
            continue;
        }
        let Some(whole) = caps.get(0) else {
            continue;
        };
        let line = newlines.partition_point(|&n| n < whole.start()) + 1;
        found.push(Finding {
            line,
            class: Class::PassiveVoice,
            text: whole.as_str().chars().take(48).collect(),
        });
    }
    found
}

/// One problem, with where it is and what to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub line: usize,
    pub class: Class,
    pub text: String,
}

impl Finding {
    /// `line N: <class> "<text>". <advice>`
    pub fn render(&self) -> String {
        if self.class == Class::Dash {
            return format!(
                "line {}: {}. {}",
                self.line,
                self.class.name(),
                self.class.advice()
            );
        }
        format!(
            "line {}: {} \"{}\". {}",
            self.line,
            self.class.name(),
            self.text,
            self.class.advice()
        )
    }
}

fn blank_out(line: &str) -> String {
    " ".repeat(line.len())
}

fn is_trailer(line: &str) -> bool {
    let l = line.trim_start();
    l.starts_with("Signed-off-by:") || l.starts_with("Co-authored-by:")
}

/// The text with everything that is not prose replaced by spaces, so byte
/// offsets and line numbers stay the same.
pub(super) fn mask(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut fenced = false;
    // An indented block is code only after a blank line. An indented line
    // that follows text is a wrapped list item and stays prose.
    let mut prev_blank = true;
    let mut in_indented = false;
    for raw in text.lines() {
        let t = raw.trim_start();
        let indented = raw.starts_with("    ") || raw.starts_with('\t');
        in_indented = indented && (prev_blank || in_indented);
        prev_blank = raw.trim().is_empty();
        if t.starts_with("```") {
            fenced = !fenced;
            out.push(blank_out(raw));
        } else if fenced
            || is_trailer(raw)
            || t.starts_with("> ")
            || t.starts_with("$ ")
            || in_indented
        {
            out.push(blank_out(raw));
        } else {
            let mut line = raw.to_string();
            for re in [&*CODE_SPAN, &*URL, &*PATH] {
                line = re
                    .replace_all(&line, |c: &regex::Captures| blank_out(&c[0]))
                    .into_owned();
            }
            out.push(line);
        }
    }
    out.join("\n")
}

/// Every problem in `text`, in line order. Empty when the text is clean.
pub fn lint(text: &str) -> Vec<Finding> {
    let masked = mask(text);
    let newlines: Vec<usize> = masked.match_indices('\n').map(|(i, _)| i).collect();
    let mut found: Vec<Finding> = Vec::new();
    for entry in TABLE.iter() {
        for m in entry.regex.find_iter(&masked) {
            // A pattern can start on the separator before its words, so the
            // line is where the first word starts.
            let at = m.start() + m.as_str().find(char::is_alphanumeric).unwrap_or(0);
            let line = newlines.partition_point(|&n| n < at) + 1;
            let snippet: String = m.as_str().trim().chars().take(48).collect();
            found.push(Finding {
                line,
                class: entry.class,
                text: snippet,
            });
        }
    }
    found.extend(passive_findings(&masked, &newlines));
    found.sort_by(|a, b| (a.line, a.text.as_str()).cmp(&(b.line, b.text.as_str())));
    found.dedup();
    found
}

/// Whether any finding is a hard one.
pub fn has_hard(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.class.hard())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(text: &str) -> Vec<Class> {
        lint(text).into_iter().map(|f| f.class).collect()
    }

    #[test]
    fn plain_text_has_no_findings() {
        let text = "fix(api): retry the call once\n\nThe client gave up too early. It now waits two seconds.\n";

        assert!(lint(text).is_empty(), "{:?}", lint(text));
    }

    #[test]
    fn an_em_or_en_dash_is_a_dash_finding() {
        let em = "The cache \u{2014} not the lock.";
        let en = "Pages 3\u{2013}5.";

        let (a, b) = (classes(em), classes(en));

        assert!(a.contains(&Class::Dash));
        assert_eq!(b, vec![Class::Dash]);
    }

    #[test]
    fn a_throat_clearing_opener_is_found() {
        let text = "Here's the thing: the cache is cold.";

        assert_eq!(classes(text), vec![Class::ThroatClearing]);
    }

    #[test]
    fn an_emphasis_crutch_is_found() {
        let text = "The lock blocks twice. Let that sink in.";

        assert_eq!(classes(text), vec![Class::EmphasisCrutch]);
    }

    #[test]
    fn business_jargon_is_found() {
        let text = "We will circle back after the deep dive.";

        let found = classes(text);

        assert_eq!(found, vec![Class::Jargon, Class::Jargon]);
    }

    #[test]
    fn a_filler_phrase_is_found() {
        let text = "At the end of the day the build is slow.";

        assert_eq!(classes(text), vec![Class::FillerAdverb]);
    }

    #[test]
    fn meta_commentary_is_found() {
        let text = "Plot twist: the test did not run.";

        assert_eq!(classes(text), vec![Class::MetaCommentary]);
    }

    #[test]
    fn performative_emphasis_is_found() {
        let text = "The bug exists, I promise.";

        assert_eq!(classes(text), vec![Class::Performative]);
    }

    #[test]
    fn telling_instead_of_showing_is_found() {
        let text = "This is genuinely hard to test.";

        assert!(classes(text).contains(&Class::TellingNotShowing));
    }

    #[test]
    fn a_binary_contrast_is_found_in_each_shape() {
        for text in [
            "Not because the code is slow. Because the query is.",
            "The problem isn't the cache. It's the lock.",
            "It feels like a bug. It's actually a feature gap.",
            "It's not a cache issue, it's a lock issue.",
            "Not just faster, but also safer.",
            "The old path stops being a fallback and starts being the default.",
        ] {
            assert!(
                classes(text).contains(&Class::BinaryContrast),
                "{text}: {:?}",
                lint(text)
            );
        }
    }

    #[test]
    fn plain_negation_is_not_a_binary_contrast() {
        for text in [
            "The cache is not cleared on exit.",
            "Skip the lock, but keep the log.",
            "It is not found. Return early.",
        ] {
            assert!(
                !classes(text).contains(&Class::BinaryContrast),
                "{text}: {:?}",
                lint(text)
            );
        }
    }

    #[test]
    fn stacked_one_word_sentences_are_dramatic_fragmentation() {
        let text = "Speed. Quality. Cost. Pick two.";
        let phrase = "The tool works. That's it. That's the fix.";

        assert!(classes(text).contains(&Class::DramaticFragmentation));
        assert!(classes(phrase).contains(&Class::DramaticFragmentation));
    }

    #[test]
    fn short_imperative_sentences_are_not_dramatic_fragmentation() {
        let text = "Add the flag. Wire it up. Update the docs.";

        assert!(!classes(text).contains(&Class::DramaticFragmentation));
    }

    #[test]
    fn a_conditional_with_a_comma_is_not_a_binary_contrast() {
        let text = "If it is not set, it is read from the environment.";

        assert!(
            !classes(text).contains(&Class::BinaryContrast),
            "{:?}",
            lint(text)
        );
    }

    #[test]
    fn a_finding_is_reported_on_the_line_where_its_words_start() {
        let so = "Done.\n\nSo we changed the order.";
        let look = "The lock blocks.\nLook, it is wrong.";

        let (a, b) = (lint(so), lint(look));

        assert_eq!(a[0].line, 3);
        assert_eq!(b[0].line, 2);
    }

    #[test]
    fn a_rhetorical_setup_is_found() {
        let text = "What if the lock were per file? Think about it.";

        let found = classes(text);

        assert_eq!(found, vec![Class::RhetoricalSetup, Class::RhetoricalSetup]);
    }

    #[test]
    fn a_formulaic_construction_is_found() {
        let text = "By the time the job finished, I was out of disk.";

        assert_eq!(classes(text), vec![Class::Formulaic]);
    }

    #[test]
    fn a_paragraph_starting_with_so_is_found_but_a_mid_paragraph_so_is_not() {
        let opener = "Done.\n\nSo we changed the order.";
        let inside = "We changed the order\nso the lock comes first.";

        assert_eq!(classes(opener), vec![Class::SoOpener]);
        assert!(classes(inside).is_empty());
    }

    #[test]
    fn a_sentence_starting_with_look_is_found() {
        let text = "Look, the lock is wrong.";

        assert_eq!(classes(text), vec![Class::LookOpener]);
    }

    #[test]
    fn a_banned_word_is_found_on_whole_words_only() {
        let hit = "We leverage the cache.";
        let miss = "The leverager is gone.";

        assert_eq!(classes(hit), vec![Class::BannedWord]);
        assert!(classes(miss).is_empty());
    }

    #[test]
    fn absolute_words_and_intensifiers_are_soft_findings() {
        let text = "It always fails and it is simply slow.";

        let found = lint(text);

        assert_eq!(classes(text), vec![Class::AbsoluteWord, Class::Intensifier]);
        assert!(!has_hard(&found));
    }

    #[test]
    fn a_three_item_list_is_a_soft_finding() {
        let text = "We changed the parser, lexer and printer.";

        let found = lint(text);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].class, Class::ThreeItemList);
        assert!(!has_hard(&found));
    }

    #[test]
    fn passive_voice_is_found_in_each_tense() {
        for text in [
            "The retry was added.",
            "The timeout is handled by the client.",
            "The bug has been fixed.",
            "The branch will be merged today.",
            "The cache is also cleared on exit.",
            "These rows were written twice.",
            "The file should be removed.",
            "The call is not retried.",
        ] {
            assert!(
                classes(text).contains(&Class::PassiveVoice),
                "{text}: {:?}",
                lint(text)
            );
        }
    }

    #[test]
    fn active_voice_and_adjectives_are_not_passive() {
        for text in [
            "I added the retry.",
            "The client handles the timeout.",
            "The speed is high and the need is clear.",
            "The list is empty.",
            "It is a hundred lines long.",
            "The result is red.",
            "Is the build green?",
            "They were happy with the result.",
        ] {
            assert!(
                !classes(text).contains(&Class::PassiveVoice),
                "{text}: {:?}",
                lint(text)
            );
        }
    }

    #[test]
    fn a_present_tense_status_word_is_allowed_but_the_same_word_as_an_event_is_not() {
        for text in [
            "The flag is deprecated.",
            "The option is required.",
            "The PR is merged.",
            "The feature is disabled in tests.",
            "The key is named id.",
        ] {
            assert!(
                !classes(text).contains(&Class::PassiveVoice),
                "{text}: {:?}",
                lint(text)
            );
        }
        for text in [
            "The flag was deprecated last week.",
            "The option will be required next year.",
            "The PR was merged.",
        ] {
            assert!(
                classes(text).contains(&Class::PassiveVoice),
                "{text}: {:?}",
                lint(text)
            );
        }
    }

    #[test]
    fn passive_voice_in_code_quotes_and_paths_is_skipped() {
        let text = "Run `was_added` now.\n\n```\nit was added\n```\n\n> it was added\n";

        assert!(!classes(text).contains(&Class::PassiveVoice));
    }

    #[test]
    fn passive_voice_is_a_soft_finding_and_names_its_line() {
        let text = "Done.\nThe retry was added.";

        let found = lint(text);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        assert!(!has_hard(&found));
    }

    #[test]
    fn fenced_code_inline_code_urls_paths_and_quotes_are_skipped() {
        let text = "Run `leverage \u{2014} x` now.\n\n```\nHere's the thing \u{2014} x\n```\n\nSee https://a.b/never-here\u{2014}x and src/always/simply.rs.\n\n> Let that sink in.\n$ echo always\n\n    indented never\n\nSigned-off-by: A \u{2014} B <a@b.c>\n";

        assert!(lint(text).is_empty(), "{:?}", lint(text));
    }

    #[test]
    fn a_wrapped_list_line_is_prose_but_an_indented_block_after_a_blank_line_is_code() {
        let wrapped = "- item text\n    Let that sink in.\n";
        let block = "Text.\n\n    Let that sink in.\n    more code\n";

        assert_eq!(classes(wrapped), vec![Class::EmphasisCrutch]);
        assert!(lint(block).is_empty(), "{:?}", lint(block));
    }

    #[test]
    fn a_finding_names_its_line_and_renders_without_the_dash_text() {
        let text = "fix: x\n\nWords \u{2014} more.\n";

        let found = lint(text);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 3);
        assert!(found[0].render().starts_with("line 3: em or en dash."));
    }
}
