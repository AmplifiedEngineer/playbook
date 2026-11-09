// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Guards the pictures under `docs/images`: each file and the folder stay
//! small, every image in `README.md` and `docs/` has alt text, every picture
//! is used by a page, and no SVG holds a path or an address that names a
//! person or a machine. The pbtop captures come from fake sample data and a
//! fixed clock (`crates/telemetry/src/tui/screenshot.rs`), so a hit here means
//! real data leaked in.

use std::fs;
use std::path::{Path, PathBuf};

/// The most bytes one image may take.
const MAX_FILE_BYTES: u64 = 153_600;
/// The most bytes `docs/images` may take in all.
const MAX_TOTAL_BYTES: u64 = 2_621_440;
/// Text an SVG must never hold: user home folders and addresses.
const DENIED: [&str; 3] = ["/Users/", "/home/", "@"];
fn repo_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

/// Every file under `dir`, as paths relative to `base`, sorted.
fn files_under(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(base, &path, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    out.sort();
}

/// The pages whose images are checked: `README.md` and the Markdown under
/// `docs/`, except the ADRs.
fn pages(root: &Path) -> Vec<String> {
    let mut all = Vec::new();
    files_under(root, &root.join("docs"), &mut all);
    let mut pages: Vec<String> = all
        .into_iter()
        .filter(|p| p.ends_with(".md") && !p.starts_with("docs/adr/"))
        .collect();
    if root.join("README.md").is_file() {
        pages.push("README.md".to_string());
    }
    pages
}

/// The text of a page with every fenced code block blanked, keeping its lines.
fn without_fences(text: &str) -> String {
    let mut fenced = false;
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            out.push('\n');
            continue;
        }
        if !fenced {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// The line number, one based, of byte `at` in `text`.
fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// Problems with the alt text of the images in one page.
fn alt_problems(page: &str, text: &str) -> Vec<String> {
    let text = without_fences(text);
    let mut problems = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("![") {
        let at = from + i;
        let after = at + 2;
        let close = text[after..].find(']').map(|c| after + c);
        match close {
            Some(c) if text[c + 1..].starts_with('(') => {
                if text[after..c].trim().is_empty() {
                    problems.push(format!(
                        "{page}:{} has an image with empty alt text",
                        line_of(&text, at)
                    ));
                }
                from = c + 1;
            }
            _ => from = after,
        }
    }
    from = 0;
    while let Some(i) = text[from..].find("<img") {
        let at = from + i;
        let end = text[at..].find('>').map_or(text.len(), |e| at + e);
        let tag = &text[at..end];
        if !has_alt(tag) {
            problems.push(format!(
                "{page}:{} has an <img> with no alt text",
                line_of(&text, at)
            ));
        }
        from = end;
    }
    problems
}

/// Whether an `<img ...` tag, line breaks included, has a non-empty `alt`.
fn has_alt(tag: &str) -> bool {
    let Some(i) = tag.find("alt=") else {
        return false;
    };
    let rest = tag[i + 4..].trim_start();
    let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
        return false;
    };
    rest[1..]
        .split(quote)
        .next()
        .is_some_and(|v| !v.trim().is_empty())
}

/// Every problem under `root`. `unused_ok` names pictures a page need not use.
fn problems(root: &Path, unused_ok: &[&str]) -> Vec<String> {
    let images = root.join("docs/images");
    let mut found = Vec::new();
    let mut files = Vec::new();
    files_under(&images, &images, &mut files);
    let mut total = 0;
    for rel in &files {
        let path = images.join(rel);
        let size = fs::metadata(&path).map_or(0, |m| m.len());
        total += size;
        if size > MAX_FILE_BYTES {
            found.push(format!(
                "docs/images/{rel} is {size} bytes, the limit is {MAX_FILE_BYTES}"
            ));
        }
        if rel.ends_with(".svg") {
            let text = fs::read_to_string(&path).unwrap_or_default();
            for bad in DENIED {
                if text.contains(bad) {
                    found.push(format!(
                        "docs/images/{rel} holds {bad:?}, which can name a person or a machine"
                    ));
                }
            }
        }
    }
    if total > MAX_TOTAL_BYTES {
        found.push(format!(
            "docs/images is {total} bytes, the limit is {MAX_TOTAL_BYTES}"
        ));
    }
    let mut all_text = String::new();
    for page in pages(root) {
        let text = fs::read_to_string(root.join(&page)).unwrap_or_default();
        found.extend(alt_problems(&page, &text));
        all_text.push_str(&without_fences(&text));
    }
    for rel in &files {
        let used = all_text.contains(&format!("images/{rel}"));
        if !used && !unused_ok.contains(&rel.as_str()) {
            found.push(format!("docs/images/{rel} is not used by any page"));
        }
    }
    found
}

/// Writes `files` under a fresh folder and returns its path.
fn tree(name: &str, files: &[(&str, Vec<u8>)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("docs-images-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    for (rel, bytes) in files {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, bytes).expect("write");
    }
    dir
}

fn text(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// Runs `problems` on a synthetic tree and removes it.
fn check(name: &str, files: &[(&str, Vec<u8>)]) -> Vec<String> {
    let dir = tree(name, files);
    let found = problems(&dir, &[]);
    let _ = fs::remove_dir_all(&dir);
    found
}

const PAGE: &str = "![a chart, sample data](docs/images/a.svg)\n";

#[test]
fn a_clean_tree_has_no_problems() {
    let found = check(
        "clean",
        &[
            ("README.md", text(PAGE)),
            ("docs/images/a.svg", text("<svg/>")),
        ],
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_file_of_exactly_the_limit_passes_and_one_byte_more_fails() {
    let at = check(
        "limit-at",
        &[
            ("README.md", text(PAGE)),
            ("docs/images/a.svg", vec![b'x'; MAX_FILE_BYTES as usize]),
        ],
    );
    let over = check(
        "limit-over",
        &[
            ("README.md", text(PAGE)),
            ("docs/images/a.svg", vec![b'x'; MAX_FILE_BYTES as usize + 1]),
        ],
    );
    assert!(at.is_empty(), "{at:?}");
    assert_eq!(over.len(), 1, "{over:?}");
    assert!(
        over[0].contains("docs/images/a.svg") && over[0].contains("153600"),
        "{over:?}"
    );
}

#[test]
fn files_that_total_exactly_the_folder_limit_pass_and_one_byte_more_fails() {
    // Arrange: 17 files of 153,600 bytes and one that fills the rest.
    let page = (0..18)
        .map(|i| format!("![x, sample data](docs/images/f{i}.svg)\n"))
        .collect::<String>();
    let mut files = vec![("README.md", text(&page))];
    let names: Vec<String> = (0..18).map(|i| format!("docs/images/f{i}.svg")).collect();
    let rest = MAX_TOTAL_BYTES as usize - 17 * MAX_FILE_BYTES as usize;
    for (i, name) in names.iter().enumerate() {
        let size = if i < 17 {
            MAX_FILE_BYTES as usize
        } else {
            rest
        };
        files.push((name.as_str(), vec![b'x'; size]));
    }

    // Act
    let at = check("total-at", &files);
    files[18].1.push(b'x');
    let over = check("total-over", &files);

    // Assert
    assert!(at.is_empty(), "{at:?}");
    assert_eq!(over.len(), 1, "{over:?}");
    assert!(over[0].contains("2621440"), "{over:?}");
}

#[test]
fn empty_alt_text_fails_and_names_the_page_and_line() {
    let found = check(
        "alt-empty",
        &[
            ("README.md", text("Intro\n![](docs/images/a.svg)\n")),
            ("docs/images/a.svg", text("<svg/>")),
        ],
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("README.md:2 "), "{found:?}");
}

#[test]
fn an_img_tag_needs_alt_even_when_it_spans_lines() {
    let ok = check(
        "img-ok",
        &[
            (
                "docs/a.md",
                text("<img\n  src=\"images/a.svg\"\n  alt=\"a chart\">\n"),
            ),
            ("docs/images/a.svg", text("<svg/>")),
        ],
    );
    let missing = check(
        "img-missing",
        &[
            ("docs/a.md", text("<img\n  src=\"images/a.svg\">\n")),
            ("docs/images/a.svg", text("<svg/>")),
        ],
    );
    let empty = check(
        "img-empty",
        &[
            ("docs/a.md", text("<img src=\"images/a.svg\" alt=\"\">\n")),
            ("docs/images/a.svg", text("<svg/>")),
        ],
    );
    assert!(ok.is_empty(), "{ok:?}");
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!(empty.len(), 1, "{empty:?}");
}

#[test]
fn an_image_inside_a_fence_is_ignored() {
    let found = check(
        "fence",
        &[
            (
                "README.md",
                text("```md\n![](docs/images/a.svg)\n```\n![ok, sample data](docs/images/a.svg)\n"),
            ),
            ("docs/images/a.svg", text("<svg/>")),
        ],
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn adr_pages_are_not_checked_for_alt_text() {
    let found = check(
        "adr",
        &[
            ("README.md", text(PAGE)),
            ("docs/adr/0001-x.md", text("![](x.png)\n")),
            ("docs/images/a.svg", text("<svg/>")),
        ],
    );
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_picture_no_page_uses_fails_unless_it_is_allowed() {
    let files = [
        ("README.md", text(PAGE)),
        ("docs/images/a.svg", text("<svg/>")),
        ("docs/images/orphan.svg", text("<svg/>")),
    ];
    let dir = tree("orphan", &files);
    let strict = problems(&dir, &[]);
    let allowed = problems(&dir, &["orphan.svg"]);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(strict.len(), 1, "{strict:?}");
    assert!(strict[0].contains("orphan.svg"), "{strict:?}");
    assert!(allowed.is_empty(), "{allowed:?}");
}

#[test]
fn an_svg_with_a_home_folder_or_an_address_fails() {
    for (name, bad) in [
        ("users", "/Users/x"),
        ("home", "/home/x"),
        ("at", "me@example.com"),
    ] {
        let found = check(
            name,
            &[
                ("README.md", text(PAGE)),
                ("docs/images/a.svg", text(&format!("<svg>{bad}</svg>"))),
            ],
        );
        assert_eq!(found.len(), 1, "{name}: {found:?}");
        assert!(found[0].contains("docs/images/a.svg"), "{found:?}");
    }
}

#[test]
fn the_pictures_in_this_repository_pass() {
    let found = problems(&repo_root(), &[]);
    assert!(found.is_empty(), "{}", found.join("\n"));
}
