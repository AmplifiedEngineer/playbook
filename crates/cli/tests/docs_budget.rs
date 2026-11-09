// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Size budgets for the pages a new reader sees first. The README must stay
//! short: it advertises the project, gives confidence, and gets the reader from
//! install to a first use. Detail belongs in `docs/`. Older guides, reference,
//! internals and ADR pages have no budget.
//!
//! Prose words are counted outside fenced code, HTML comments, the badge line
//! and image lines.

use std::fs;
use std::path::PathBuf;

/// One row of the budget table.
struct Limits {
    /// A file path, or `dir/*.md` for every Markdown file directly in `dir`.
    path: &'static str,
    max_lines: Option<usize>,
    max_words: usize,
    max_h2: Option<usize>,
    max_pictures: Option<usize>,
    max_badges: Option<usize>,
}

const LIMITS: &[Limits] = &[
    Limits {
        path: "README.md",
        max_lines: Some(70),
        max_words: 450,
        max_h2: Some(6),
        max_pictures: Some(1),
        max_badges: Some(5),
    },
    Limits {
        path: "docs/README.md",
        max_lines: Some(90),
        max_words: 450,
        max_h2: None,
        max_pictures: None,
        max_badges: None,
    },
    Limits {
        path: "docs/get-started.md",
        max_lines: None,
        max_words: 700,
        max_h2: None,
        max_pictures: None,
        max_badges: None,
    },
    Limits {
        path: "docs/concepts.md",
        max_lines: None,
        max_words: 800,
        max_h2: None,
        max_pictures: None,
        max_badges: None,
    },
];

fn root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

/// Does this line open or close a fenced code block?
fn is_fence(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// The lines outside fenced code blocks, with their 0-based index.
fn outside_fences(text: &str) -> Vec<(usize, &str)> {
    let mut open = false;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if is_fence(line) {
            open = !open;
        } else if !open {
            out.push((i, line));
        }
    }
    out
}

/// The text with every HTML comment removed, line breaks kept.
fn strip_comments(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => {
                // Keep the line breaks so line numbers stay put.
                let skipped = &rest[start..start + end + 3];
                out.extend(skipped.chars().filter(|c| *c == '\n'));
                rest = &rest[start + end + 3..];
            }
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The URLs of the Markdown images and `<img>` tags on a line.
fn image_urls(line: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find("![") {
        let after = &rest[i + 2..];
        if let Some(close) = after.find("](") {
            let tail = &after[close + 2..];
            if let Some(end) = tail.find(')') {
                urls.push(tail[..end].to_string());
            }
        }
        rest = after;
    }
    urls
}

fn is_badge_url(url: &str) -> bool {
    url.contains("badge") || url.contains("img.shields.io")
}

/// The index of the badge line: the first non-empty line after the H1, when it
/// starts with `[![` and every image on it has a badge URL.
fn badge_line(text: &str) -> Option<usize> {
    let lines: Vec<(usize, &str)> = outside_fences(text);
    let h1 = lines.iter().position(|(_, l)| l.starts_with("# "))?;
    let (i, line) = lines[h1 + 1..].iter().find(|(_, l)| !l.trim().is_empty())?;
    let urls = image_urls(line);
    if line.starts_with("[![") && !urls.is_empty() && urls.iter().all(|u| is_badge_url(u)) {
        Some(*i)
    } else {
        None
    }
}

fn count_lines(text: &str) -> usize {
    text.lines().count()
}

/// Prose words: outside fences and comments, skipping the badge line, image
/// lines and `<picture>` blocks. A token with no letter or digit is not a word.
fn count_words(text: &str) -> usize {
    let clean = strip_comments(text);
    let badge = badge_line(&clean);
    let mut in_picture = false;
    let mut words = 0;
    for (i, line) in outside_fences(&clean) {
        if Some(i) == badge {
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("<picture") {
            in_picture = true;
        }
        if in_picture {
            if line.contains("</picture>") {
                in_picture = false;
            }
            continue;
        }
        if t.starts_with("![") {
            continue;
        }
        words += line
            .split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .count();
    }
    words
}

fn count_h2(text: &str) -> usize {
    outside_fences(text)
        .iter()
        .filter(|(_, l)| l.starts_with("## "))
        .count()
}

/// Pictures: each `<picture>` element is one, and so is each Markdown image or
/// bare `<img>` outside the badge line and outside a `<picture>`.
fn count_pictures(text: &str) -> usize {
    let clean = strip_comments(text);
    let badge = badge_line(&clean);
    let mut in_picture = false;
    let mut pictures = 0;
    for (i, line) in outside_fences(&clean) {
        if Some(i) == badge {
            continue;
        }
        if line.contains("<picture") {
            in_picture = true;
            pictures += 1;
        }
        if in_picture {
            if line.contains("</picture>") {
                in_picture = false;
            }
            continue;
        }
        pictures += image_urls(line).len() + line.matches("<img").count();
    }
    pictures
}

fn count_badges(text: &str) -> usize {
    let clean = strip_comments(text);
    badge_line(&clean).map_or(0, |i| {
        clean.lines().nth(i).map_or(0, |l| image_urls(l).len())
    })
}

/// Every failed limit for one page, each naming the file, the measure, the
/// value and the limit.
fn check(name: &str, text: &str, limits: &Limits) -> Vec<String> {
    let mut bad = Vec::new();
    let mut over = |measure: &str, value: usize, limit: usize, hint: &str| {
        if value > limit {
            bad.push(format!(
                "{name} has {value} {measure}, the limit is {limit}. {hint}"
            ));
        }
    };
    if let Some(max) = limits.max_lines {
        over("lines", count_lines(text), max, "Move detail to docs/.");
    }
    over(
        "prose words",
        count_words(text),
        limits.max_words,
        "Move detail to docs/.",
    );
    if let Some(max) = limits.max_h2 {
        over("H2 sections", count_h2(text), max, "Merge or cut sections.");
    }
    if let Some(max) = limits.max_pictures {
        over("pictures", count_pictures(text), max, "Keep one picture.");
    }
    if let Some(max) = limits.max_badges {
        over(
            "badges",
            count_badges(text),
            max,
            "Keep the badge line short.",
        );
    }
    bad
}

/// The files a table row covers.
fn files_for(path: &str) -> Vec<PathBuf> {
    match path.strip_suffix("/*.md") {
        Some(dir) => {
            let mut files: Vec<PathBuf> = fs::read_dir(root().join(dir))
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .map(|e| e.path())
                        .filter(|p| p.extension().is_some_and(|e| e == "md"))
                        .collect()
                })
                .unwrap_or_default();
            files.sort();
            files
        }
        None => vec![root().join(path)],
    }
}

#[test]
fn the_pages_stay_within_their_budget() {
    let mut bad = Vec::new();
    for limits in LIMITS {
        for file in files_for(limits.path) {
            let text = fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
            let name = file
                .strip_prefix(root())
                .unwrap_or(&file)
                .display()
                .to_string();
            bad.extend(check(&name, &text, limits));
        }
    }
    assert!(bad.is_empty(), "page budget exceeded:\n{}", bad.join("\n"));
}

#[cfg(test)]
mod synthetic {
    use super::*;

    const ANY: Limits = Limits {
        path: "x.md",
        max_lines: None,
        max_words: usize::MAX,
        max_h2: None,
        max_pictures: None,
        max_badges: None,
    };

    fn lines_limit(n: usize) -> Limits {
        Limits {
            max_lines: Some(n),
            ..ANY
        }
    }

    fn words_limit(n: usize) -> Limits {
        Limits {
            max_words: n,
            ..ANY
        }
    }

    #[test]
    fn seventy_lines_pass_and_seventy_one_fail() {
        let l = lines_limit(70);
        assert!(check("X.md", &"a\n".repeat(70), &l).is_empty());
        let bad = check("X.md", &"a\n".repeat(71), &l);
        assert_eq!(
            bad,
            ["X.md has 71 lines, the limit is 70. Move detail to docs/."]
        );
    }

    #[test]
    fn a_trailing_newline_adds_no_line() {
        assert_eq!(count_lines("a\nb\n"), 2);
        assert_eq!(count_lines("a\nb"), 2);
    }

    #[test]
    fn four_hundred_fifty_words_pass_and_four_hundred_fifty_one_fail() {
        let l = words_limit(450);
        assert!(check("X.md", &"word ".repeat(450), &l).is_empty());
        let bad = check("X.md", &"word ".repeat(451), &l);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("X.md has 451 prose words, the limit is 450"));
    }

    #[test]
    fn words_in_fences_and_comments_do_not_count() {
        let text = "one\n```bash\nfence words here\n```\n<!-- a comment\nspanning lines -->\ntwo <!-- inline note --> three\n";
        assert_eq!(count_words(text), 3);
    }

    #[test]
    fn symbols_alone_are_not_words() {
        assert_eq!(count_words("| --- | --- |\n| a | b |\n"), 2);
    }

    #[test]
    fn six_h2_pass_and_seven_fail() {
        let l = Limits {
            max_h2: Some(6),
            ..ANY
        };
        assert!(check("X.md", &"## a\n".repeat(6), &l).is_empty());
        let bad = check("X.md", &"## a\n".repeat(7), &l);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("X.md has 7 H2 sections, the limit is 6"));
    }

    #[test]
    fn an_h2_in_a_fence_does_not_count() {
        assert_eq!(count_h2("## a\n```text\n## not a heading\n```\n## b\n"), 2);
    }

    fn badges(n: usize) -> String {
        let one = "[![CI](https://example.com/badge.svg)](https://example.com)";
        vec![one; n].join(" ")
    }

    #[test]
    fn one_picture_and_five_badges_pass() {
        let l = Limits {
            max_pictures: Some(1),
            max_badges: Some(5),
            ..ANY
        };
        let text = format!("# T\n\n{}\n\n![alt](docs/images/a.svg)\n", badges(5));
        assert!(check("X.md", &text, &l).is_empty());
        assert_eq!(count_badges(&text), 5);
        assert_eq!(count_pictures(&text), 1);
    }

    #[test]
    fn six_badges_fail() {
        let l = Limits {
            max_badges: Some(5),
            ..ANY
        };
        let text = format!("# T\n\n{}\n", badges(6));
        let bad = check("X.md", &text, &l);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("X.md has 6 badges, the limit is 5"));
    }

    #[test]
    fn two_pictures_fail() {
        let l = Limits {
            max_pictures: Some(1),
            ..ANY
        };
        let text = "# T\n\n![a](a.svg)\n\n![b](b.svg)\n";
        let bad = check("X.md", text, &l);
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("X.md has 2 pictures, the limit is 1"));
    }

    #[test]
    fn a_picture_element_counts_once() {
        let text = "# T\n\n<picture>\n  <source media=\"(prefers-color-scheme: dark)\" srcset=\"a-dark.svg\">\n  <img src=\"a-light.svg\" alt=\"x\">\n</picture>\n";
        assert_eq!(count_pictures(text), 1);
        assert_eq!(count_words(text), 1);
    }

    #[test]
    fn a_linked_hero_image_after_the_h1_is_a_picture_not_a_badge() {
        let text = "# T\n\n[![Hero](docs/images/hero.svg)](docs/pbtop.md)\n";
        assert_eq!(count_badges(text), 0);
        assert_eq!(count_pictures(text), 1);
    }

    #[test]
    fn an_image_in_a_fence_is_not_a_picture() {
        assert_eq!(count_pictures("# T\n```md\n![a](a.svg)\n```\n"), 0);
    }

    #[test]
    fn a_glob_row_lists_markdown_files_in_a_folder() {
        let files = files_for("docs/guides/*.md");
        assert!(files.iter().any(|f| f.ends_with("00-install.md")));
        assert!(files.iter().all(|f| f.extension().unwrap() == "md"));
    }
}
