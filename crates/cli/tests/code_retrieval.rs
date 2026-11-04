// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Correctness bars for `playbook code` on a generated 100 file repo, and the
//! table that turns measured numbers into kill, partial or pass.

mod code_fixture;

use code_fixture::{correctness, generate, stale_without_flag, verdict, Harness, Numbers, Verdict};

fn passing() -> Numbers {
    Numbers {
        files: 5_000,
        r_a: 80.0,
        r_b: 50.0,
        symbol_top3: 100.0,
        refs_recall: 100.0,
        stale_without_flag: 0,
        latency_bars_met: true,
        db_size_bar_met: true,
        large_bars_met: true,
    }
}

#[test]
fn verdict_follows_the_bars() {
    type Change = fn(&mut Numbers);
    let table: [(&str, Change, Verdict); 18] = [
        ("all bars met", |_| {}, Verdict::Pass),
        (
            "one stale answer",
            |n| n.stale_without_flag = 1,
            Verdict::Kill,
        ),
        ("r_a 49.9", |n| n.r_a = 49.9, Verdict::Kill),
        ("r_a 50", |n| n.r_a = 50.0, Verdict::Partial),
        ("r_a 74.9", |n| n.r_a = 74.9, Verdict::Partial),
        ("r_a 75", |n| n.r_a = 75.0, Verdict::Pass),
        (
            "r_a 80 and a correctness bar missed",
            |n| n.symbol_top3 = 90.0,
            Verdict::Kill,
        ),
        (
            "latency bar missed",
            |n| n.latency_bars_met = false,
            Verdict::Partial,
        ),
        ("r_b 39.9", |n| n.r_b = 39.9, Verdict::Partial),
        ("r_b 40", |n| n.r_b = 40.0, Verdict::Pass),
        (
            "db size missed",
            |n| n.db_size_bar_met = false,
            Verdict::Partial,
        ),
        (
            "50,000 file bar missed",
            |n| n.large_bars_met = false,
            Verdict::Partial,
        ),
        ("top 3 at 94.9", |n| n.symbol_top3 = 94.9, Verdict::Kill),
        ("top 3 at 95", |n| n.symbol_top3 = 95.0, Verdict::Pass),
        ("recall at 89.9", |n| n.refs_recall = 89.9, Verdict::Kill),
        ("recall at 90", |n| n.refs_recall = 90.0, Verdict::Pass),
        (
            "no saving at 100 files",
            |n| {
                n.files = 100;
                n.r_a = 0.0;
            },
            Verdict::Partial,
        ),
        (
            "a low saving at 100 files",
            |n| {
                n.files = 100;
                n.r_a = -10.0;
            },
            Verdict::Partial,
        ),
    ];
    for (name, change, expected) in table {
        // Arrange
        let mut numbers = passing();
        change(&mut numbers);

        // Act
        let got = verdict(&numbers);

        // Assert
        assert_eq!(got, expected, "{name}");
    }
}

#[test]
fn the_generated_repo_has_thirty_tasks_with_known_answers() {
    // Arrange
    let h = Harness::new("shape", false);

    // Act
    let repo = generate(&h.repo, 100, 7);

    // Assert
    assert_eq!(repo.tasks.len(), 30);
    assert!(repo.tasks.iter().all(|t| !t.expected.is_empty()));
    assert_eq!(repo.declarations.len(), 100);
}

#[test]
fn symbol_and_refs_meet_their_bars_on_one_hundred_files() {
    // Arrange
    let h = Harness::new("correct", false);
    let repo = generate(&h.repo, 100, 7);
    h.init_and_commit();

    // Act
    let (top3, recall) = correctness(&h, &repo);

    // Assert
    assert!(top3 >= 95.0, "symbol top-3 was {top3}%");
    assert!(recall >= 90.0, "refs recall was {recall}%");
}

#[test]
fn no_answer_points_at_changed_files_without_the_stale_flag() {
    // Arrange
    let h = Harness::new("stale", false);
    let repo = generate(&h.repo, 100, 7);
    h.init_and_commit();

    // Act
    let unflagged = stale_without_flag(&h, &repo);

    // Assert
    assert_eq!(unflagged, 0);
}
