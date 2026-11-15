// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The slow, fallible lookups behind a session record: the working directory
//! a transcript recorded, what git says about that directory, and the pull
//! request of the branch. Every call is bounded by a timeout and every failure
//! reads as "unknown". The only write is the pull request cache: `gather`
//! stores a lookup's outcome in an existing `sessions.db` through the injected
//! write-back, and falls back to the cached pull request when a lookup finds
//! none.

use super::{is_trunk, store, Place, Pr, PrKind};
use crate::common::git;
use crate::common::proc::run_with_timeout;
use crate::usage::run::Paths;
use crate::usage::UsageEvent;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Lines of a transcript read while looking for its working directory.
const HEAD_LINES: usize = 200;
const GH_TIMEOUT: Duration = Duration::from_secs(8);
/// Seconds a looked-up pull request stays trusted before `gh` is asked again.
pub(crate) const PR_REFRESH_SECS: i64 = 300;

/// A pull request as `gh` reports it, with the commit and base the closer
/// compares against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFound {
    pub number: u64,
    pub kind: PrKind,
    pub head_oid: Option<String>,
    pub base: Option<String>,
}

/// The pull request lookup of `gather`, injected so tests need no `gh`.
pub(crate) type PrLookup<'a> = &'a (dyn Fn(&Path, &str) -> Option<PrFound> + Sync);
/// Stores a lookup's outcome for a session; a failure is the caller's to ignore.
pub(crate) type WriteBack<'a> =
    &'a (dyn Fn(&Path, &str, &store::CacheUpdate) -> Result<(), String> + Sync);

/// The `cwd` of the first transcript line that has one, for the Claude Code
/// transcript of `session_id` under `projects`. The prompt text on those lines
/// is never kept.
pub fn transcript_cwd(projects: &Path, session_id: &str) -> Option<String> {
    let file = std::fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|d| d.path().join(format!("{session_id}.jsonl")))
        .find(|p| p.is_file())?;
    let reader = BufReader::new(std::fs::File::open(file).ok()?);
    reader
        .lines()
        .take(HEAD_LINES)
        .map_while(Result::ok)
        .find_map(|line| cwd_of_line(&line))
}

fn cwd_of_line(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value
        .get("cwd")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Whether [`place_of`] runs `git status` to fill [`Place::dirty`].
#[derive(Debug, Clone, Copy)]
pub enum Dirty {
    /// Read it: one more `git` call.
    Check,
    /// Leave it `None`, for a caller that never reads it.
    Skip,
}

/// The place of a working directory: linked worktree or main checkout, the
/// playbook convention, and whether it holds uncommitted changes. The
/// session-start hook stores no dirty flag, so it passes [`Dirty::Skip`].
pub fn place_of(cwd: Option<String>, home: &Path, dirty: Dirty) -> Place {
    let Some(dir) = cwd else {
        return Place::default();
    };
    let path = PathBuf::from(&dir);
    if !path.is_dir() {
        return Place {
            cwd: Some(dir),
            gone: true,
            ..Place::default()
        };
    }
    let is_dirty = match dirty {
        Dirty::Check => git::raw(&path, &["status", "--porcelain"]).map(|s| !s.trim().is_empty()),
        Dirty::Skip => None,
    };
    let parsed = git::raw(
        &path,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-dir",
            "--git-common-dir",
        ],
    )
    .and_then(|out| parse_rev_parse(&out));
    let (worktree, convention) = match parsed {
        Some((top, true)) => {
            let convention = convention_label(crate::worktree::classify(Path::new(&top), home));
            (Some(top), convention)
        }
        _ => (None, ""),
    };
    Place {
        cwd: Some(dir),
        worktree,
        convention,
        gone: false,
        dirty: is_dirty,
    }
}

/// The root of the repository that holds `dir`: the parent of the common git
/// dir. `None` outside a repository, in a bare one, or when the git dir lives
/// apart from the working tree, because none of those has a root to name.
pub fn repo_of(dir: &Path) -> Option<PathBuf> {
    let out = git::output(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Path::new(&out)
        .ends_with(".git")
        .then(|| Path::new(&out).parent().map(Path::to_path_buf))
        .flatten()
}

/// `(toplevel, is_linked_worktree)` from the three lines of
/// `git rev-parse --show-toplevel --git-dir --git-common-dir`.
pub fn parse_rev_parse(out: &str) -> Option<(String, bool)> {
    let mut lines = out.lines();
    let top = lines.next()?.trim().to_string();
    let git_dir = lines.next()?.trim();
    let common = lines.next()?.trim();
    (!top.is_empty()).then_some((top, git_dir != common))
}

fn convention_label(c: crate::worktree::Convention) -> &'static str {
    use crate::worktree::Convention::*;
    match c {
        Wu => "wu",
        AgentTool => "agent",
        CcLauncher => "cc",
        Review => "review",
        Unmanaged => "",
    }
}

/// The newest pull request of `branch`, read with `gh` from `dir`. `None` when
/// `gh` is missing, offline or finds none.
pub fn pr_of(dir: &Path, branch: &str) -> Option<PrFound> {
    let mut command = Command::new("gh");
    command
        .current_dir(dir)
        .args([
            "pr", "list", "--head", branch, "--state", "all", "--limit", "1",
        ])
        .args(["--json", "number,state,headRefOid,baseRefName"]);
    let out = run_with_timeout(&mut command, GH_TIMEOUT).filter(|o| o.status.success())?;
    parse_pr_list(&String::from_utf8_lossy(&out.stdout))
}

fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

pub fn parse_pr_list(json: &str) -> Option<PrFound> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let first = value.as_array()?.first()?;
    let kind = match first.get("state")?.as_str()? {
        "OPEN" => PrKind::Open,
        "MERGED" => PrKind::Merged,
        "CLOSED" => PrKind::Closed,
        _ => return None,
    };
    Some(PrFound {
        number: first.get("number")?.as_u64()?,
        kind,
        head_oid: text_field(first, "headRefOid"),
        base: text_field(first, "baseRefName"),
    })
}

/// Looks up the place and the PR of every distinct session in `newest`, one
/// scoped thread each so slow `gh` calls overlap. Keyed by session id.
pub(crate) fn gather(
    newest: &[&UsageEvent],
    paths: &Paths,
    home: &Path,
    now: i64,
    force: bool,
    pr_of: PrLookup,
    write_back: WriteBack,
) -> HashMap<String, (Place, Option<Pr>)> {
    let ids: Vec<String> = newest.iter().map(|e| e.session_id.clone()).collect();
    let db = store::db_path_from(home);
    let cache = store::read_pr_cache(&db, &ids);
    let (db, cache) = (&db, &cache);
    std::thread::scope(|scope| {
        let handles: Vec<_> = newest
            .iter()
            .map(|e| {
                scope.spawn(move || {
                    let cwd = transcript_cwd(&paths.claude_projects, &e.session_id);
                    let place = place_of(cwd, home, Dirty::Check);
                    let row = cache.get(&e.session_id);
                    // A session that changed branch must not show the old branch's PR.
                    let cached = row.filter(|c| c.branch.as_deref() == Some(e.branch.as_str()));
                    let dir = match (&place.cwd, place.gone) {
                        (Some(dir), false) if !is_trunk(&e.branch) => Some(dir),
                        _ => None,
                    };
                    let Some(dir) = dir else {
                        return (e.session_id.clone(), (place, None));
                    };
                    let is_fresh = cached
                        .and_then(|c| c.checked_at)
                        .is_some_and(|at| !force && now - at < PR_REFRESH_SECS);
                    let looked_up = (!is_fresh).then(|| pr_of(Path::new(dir), &e.branch));
                    let cached_pr = cached.and_then(|c| c.pr);
                    let pr = match &looked_up {
                        // No PR found keeps the cached one: `gh` also fails offline.
                        Some(found) => found
                            .as_ref()
                            .map(|f| Pr {
                                number: f.number,
                                kind: f.kind,
                            })
                            .or(cached_pr),
                        None => cached_pr,
                    };
                    let mut update = store::CacheUpdate::default();
                    if let Some(found) = &looked_up {
                        update.pr_checked_at = Some(now);
                        if let Some(f) = found {
                            update.pr_number = i64::try_from(f.number).ok();
                            update.pr_kind = Some(f.kind.label());
                            update.pr_head_oid = f.head_oid.clone();
                            update.pr_base = f.base.clone();
                        }
                    }
                    if row.is_some() && update != store::CacheUpdate::default() {
                        // A failed write only costs the next read a lookup.
                        let _ = write_back(db, &e.session_id, &update);
                    }
                    (e.session_id.clone(), (place, pr))
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::common::test_support::scratch_dir;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OLD_AGE_SECS: i64 = PR_REFRESH_SECS;

    fn merged() -> PrFound {
        PrFound {
            number: PR_NUMBER,
            kind: PrKind::Merged,
            head_oid: Some("abc".into()),
            base: Some("main".into()),
        }
    }

    const NOW: i64 = 1_788_400_000;
    const SESSION: &str = "s1";
    const BRANCH: &str = "feat/x";
    const PR_NUMBER: u64 = 7;
    const SEEDED_OID: &str = "seeded-oid";
    const SEEDED_BASE: &str = "seeded-base";
    const OTHER_BRANCH: &str = "feat/old";

    fn no_write(_: &Path, _: &str, _: &store::CacheUpdate) -> Result<(), String> {
        Ok(())
    }

    /// A scratch home with a transcript whose cwd is an existing folder and,
    /// once seeded, a `sessions.db` row for [`SESSION`].
    struct Fixture {
        home: PathBuf,
        work: PathBuf,
        branch: &'static str,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let home = scratch_dir(tag);
            let work = home.join("work");
            let projects = home.join(".claude").join("projects").join("-p");
            std::fs::create_dir_all(&work).unwrap();
            std::fs::create_dir_all(&projects).unwrap();
            std::fs::write(
                projects.join(format!("{SESSION}.jsonl")),
                format!("{{\"cwd\":\"{}\"}}\n", work.display()),
            )
            .unwrap();
            Fixture {
                home,
                work,
                branch: BRANCH,
            }
        }

        fn db(&self) -> PathBuf {
            store::db_path_from(&self.home)
        }

        fn seed(&self, checked_at: Option<i64>, kind: Option<&str>) {
            self.seed_on(BRANCH, checked_at, kind);
        }

        fn seed_on(&self, branch: &str, checked_at: Option<i64>, kind: Option<&str>) {
            let conn = store::open_at(&self.db()).unwrap();
            let number = kind.map(|_| PR_NUMBER as i64);
            conn.execute(
                "INSERT INTO sessions (session_id, cwd, branch, pr_number, pr_kind, \
                 pr_head_oid, pr_base, pr_checked_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                playbook_store::params![
                    SESSION,
                    self.work.to_str(),
                    branch,
                    number,
                    kind,
                    SEEDED_OID,
                    SEEDED_BASE,
                    checked_at
                ],
            )
            .unwrap();
        }

        fn column(&self, name: &str) -> Option<String> {
            let conn = store::open_at(&self.db()).unwrap();
            conn.query_row(
                &format!("SELECT CAST({name} AS TEXT) FROM sessions WHERE session_id = ?1"),
                [SESSION],
                |r| r.get(0),
            )
            .unwrap()
        }

        fn gather(
            &self,
            now: i64,
            force: bool,
            lookup: PrLookup,
            write: WriteBack,
        ) -> (Place, Option<Pr>) {
            let event = UsageEvent {
                session_id: SESSION.into(),
                branch: self.branch.into(),
                ..UsageEvent::default()
            };
            let paths = Paths::from_home(&self.home);
            let mut found = gather(&[&event], &paths, &self.home, now, force, lookup, write);
            found.remove(SESSION).expect("the session is gathered")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn a_fresh_cached_pr_does_not_call_the_lookup() {
        // Arrange
        let fixture = Fixture::new("fresh");
        fixture.seed(Some(NOW - (PR_REFRESH_SECS - 1)), Some("open"));
        let lookup = |_: &Path, _: &str| -> Option<PrFound> { panic!("the lookup must not run") };

        // Act
        let (_, pr) = fixture.gather(NOW, false, &lookup, &no_write);

        // Assert
        assert_eq!(
            pr,
            Some(Pr {
                number: PR_NUMBER,
                kind: PrKind::Open
            })
        );
    }

    #[test]
    fn the_lookup_runs_and_is_written_back_when_the_cache_is_old_or_empty() {
        // Arrange
        let cases = [("old", Some(NOW - OLD_AGE_SECS)), ("never_checked", None)];
        for (tag, checked_at) in cases {
            let fixture = Fixture::new(tag);
            fixture.seed(checked_at, Some("open"));
            let calls = AtomicUsize::new(0);
            let lookup = |_: &Path, _: &str| {
                calls.fetch_add(1, Ordering::SeqCst);
                Some(merged())
            };

            // Act
            let (_, pr) = fixture.gather(NOW, false, &lookup, &store::write_back);

            // Assert
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{tag}");
            assert_eq!(pr.map(|p| p.kind), Some(PrKind::Merged), "{tag}");
            assert_eq!(
                fixture.column("pr_checked_at"),
                Some(NOW.to_string()),
                "{tag}"
            );
        }
    }

    fn counting<'a>(
        calls: &'a AtomicUsize,
        found: Option<PrFound>,
    ) -> impl Fn(&Path, &str) -> Option<PrFound> + Sync + 'a {
        move |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            found.clone()
        }
    }

    #[test]
    fn without_a_database_the_lookup_runs_and_no_file_is_created() {
        // Arrange
        let fixture = Fixture::new("no-db");
        let calls = AtomicUsize::new(0);
        let lookup = counting(&calls, Some(merged()));

        // Act
        let (_, pr) = fixture.gather(NOW, false, &lookup, &store::write_back);

        // Assert
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(pr.map(|p| p.kind), Some(PrKind::Merged));
        assert!(!fixture.db().exists());
    }

    #[test]
    fn a_trunk_branch_or_a_missing_or_gone_place_skips_the_lookup_and_shows_no_pr() {
        // Arrange
        let cases = [
            ("trunk", "main", None),
            ("no_place", BRANCH, Some(".claude")),
            ("gone_place", BRANCH, Some("work")),
        ];
        for (tag, branch, remove) in cases {
            let mut fixture = Fixture::new(tag);
            fixture.branch = branch;
            fixture.seed_on(branch, Some(NOW - 10), Some("open"));
            if let Some(name) = remove {
                std::fs::remove_dir_all(fixture.home.join(name)).unwrap();
            }
            let calls = AtomicUsize::new(0);
            let lookup = counting(&calls, Some(merged()));

            // Act
            let (_, pr) = fixture.gather(NOW, false, &lookup, &store::write_back);

            // Assert
            assert_eq!(calls.load(Ordering::SeqCst), 0, "{tag}");
            assert_eq!(pr, None, "{tag}");
        }
    }

    #[test]
    fn a_forced_lookup_replaces_a_fresh_cached_pr() {
        // Arrange
        let fixture = Fixture::new("force");
        fixture.seed(Some(NOW - 10), Some("open"));
        let calls = AtomicUsize::new(0);
        let lookup = counting(&calls, Some(merged()));

        // Act
        let (_, pr) = fixture.gather(NOW, true, &lookup, &store::write_back);

        // Assert
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(pr.map(|p| p.kind), Some(PrKind::Merged));
        assert_eq!(fixture.column("pr_kind").as_deref(), Some("merged"));
        assert_eq!(fixture.column("pr_head_oid").as_deref(), Some("abc"));
        assert_eq!(fixture.column("pr_base").as_deref(), Some("main"));
    }

    #[test]
    fn a_forced_lookup_that_finds_nothing_keeps_the_cached_pr() {
        // Arrange
        let fixture = Fixture::new("force-none");
        fixture.seed(Some(NOW - 10), Some("open"));
        let calls = AtomicUsize::new(0);
        let lookup = counting(&calls, None);

        // Act
        let (_, pr) = fixture.gather(NOW, true, &lookup, &store::write_back);

        // Assert
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(pr.map(|p| p.kind), Some(PrKind::Open));
        assert_eq!(fixture.column("pr_kind").as_deref(), Some("open"));
        assert_eq!(fixture.column("pr_head_oid").as_deref(), Some(SEEDED_OID));
        assert_eq!(fixture.column("pr_base").as_deref(), Some(SEEDED_BASE));
    }

    #[test]
    fn a_pr_cached_on_another_branch_is_never_shown() {
        // Arrange
        let cases = [("fresh", NOW - 10), ("old", NOW - OLD_AGE_SECS)];
        for (tag, checked_at) in cases {
            let fixture = Fixture::new(tag);
            fixture.seed_on(OTHER_BRANCH, Some(checked_at), Some("open"));
            let calls = AtomicUsize::new(0);
            let lookup = counting(&calls, None);

            // Act
            let (_, pr) = fixture.gather(NOW, false, &lookup, &store::write_back);

            // Assert
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{tag}");
            assert_eq!(pr, None, "{tag}");
        }
    }

    #[test]
    fn a_failed_write_back_still_returns_the_looked_up_pr() {
        // Arrange
        let fixture = Fixture::new("write-fails");
        fixture.seed(None, None);
        let calls = AtomicUsize::new(0);
        let lookup = counting(&calls, Some(merged()));
        let failing = |_: &Path, _: &str, _: &store::CacheUpdate| Err("disk full".to_string());

        // Act
        let (_, pr) = fixture.gather(NOW, false, &lookup, &failing);

        // Assert
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(pr.map(|p| p.kind), Some(PrKind::Merged));
    }

    #[test]
    fn a_branch_with_no_pr_is_marked_checked_and_keeps_its_columns() {
        // Arrange
        let fixture = Fixture::new("no-pr");
        fixture.seed(None, None);
        let calls = AtomicUsize::new(0);
        let lookup = counting(&calls, None);

        // Act
        fixture.gather(NOW, false, &lookup, &store::write_back);

        // Assert
        assert_eq!(fixture.column("pr_kind"), None);
        assert_eq!(fixture.column("pr_checked_at"), Some(NOW.to_string()));
    }

    #[test]
    fn a_pr_list_reads_the_newest_state() {
        let json = r#"[{"number":540,"state":"MERGED","headRefOid":"abc","baseRefName":"main"}]"#;
        let pr = parse_pr_list(json).unwrap();
        assert_eq!((pr.number, pr.kind), (540, PrKind::Merged));
        assert_eq!(
            (pr.head_oid.as_deref(), pr.base.as_deref()),
            (Some("abc"), Some("main"))
        );
        assert_eq!(parse_pr_list("[]"), None);
        assert_eq!(parse_pr_list("not json"), None);
        assert_eq!(parse_pr_list(r#"[{"number":1,"state":"DRAFT"}]"#), None);
    }

    #[test]
    fn a_linked_worktree_has_a_git_dir_apart_from_the_common_dir() {
        let linked = "/w/repo-wt\n/r/.git/worktrees/repo-wt\n/r/.git\n";
        assert_eq!(parse_rev_parse(linked), Some(("/w/repo-wt".into(), true)));
        let main = "/r\n/r/.git\n/r/.git\n";
        assert_eq!(parse_rev_parse(main), Some(("/r".into(), false)));
        assert_eq!(parse_rev_parse(""), None);
    }

    #[test]
    fn the_cwd_comes_from_the_first_line_that_has_one_and_not_from_prompts() {
        assert_eq!(cwd_of_line(r#"{"type":"summary"}"#), None);
        assert_eq!(
            cwd_of_line(r#"{"cwd":"/w/x","message":{"content":"secret prompt"}}"#),
            Some("/w/x".into())
        );
    }

    #[test]
    fn a_transcript_is_found_by_session_id_under_any_project_folder() {
        let dir = std::env::temp_dir().join(format!("pb-sessions-{}", std::process::id()));
        let proj = dir.join("-w-x");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("abc.jsonl"),
            "{\"type\":\"summary\"}\n{\"cwd\":\"/w/x\"}\n",
        )
        .unwrap();
        assert_eq!(transcript_cwd(&dir, "abc"), Some("/w/x".into()));
        assert_eq!(transcript_cwd(&dir, "missing"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn git_init(dir: &Path, extra: &[&str]) {
        let status = Command::new("git")
            .args(["init", "-b", "main", "--quiet"])
            .args(extra)
            .arg(dir)
            .status()
            .expect("git");
        assert!(status.success());
        // Local config only, so a developer's signing or hooks never run.
        let no_hooks = dir.join(".no-hooks");
        std::fs::create_dir_all(&no_hooks).unwrap();
        for (key, value) in [
            ("commit.gpgsign", "false"),
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("core.hooksPath", no_hooks.to_str().unwrap()),
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["config", "--local", key, value])
                .status()
                .expect("git");
            assert!(status.success());
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pb-repo-of-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn a_directory_inside_a_repository_has_that_repository_as_its_root() {
        // Arrange
        let root = scratch("inside");
        git_init(&root, &[]);
        let sub = root.join("a");
        std::fs::create_dir_all(&sub).unwrap();

        // Act
        let repo = repo_of(&sub);

        // Assert
        assert_eq!(repo, Some(root.clone()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_directory_outside_any_repository_has_no_root() {
        // Arrange
        let dir = scratch("outside");

        // Act
        let repo = repo_of(&dir);

        // Assert
        assert_eq!(repo, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_repository_with_a_separate_git_dir_has_no_root() {
        // Arrange
        let root = scratch("separate");
        let work = root.join("work");
        let git = root.join("elsewhere");
        std::fs::create_dir_all(&work).unwrap();
        git_init(&work, &[&format!("--separate-git-dir={}", git.display())]);

        // Act
        let repo = repo_of(&work);

        // Assert
        assert_eq!(repo, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_dirty_flag_is_read_only_when_asked_for() {
        // Arrange
        let root = scratch("dirty");
        git_init(&root, &[]);
        std::fs::write(root.join("untracked.txt"), "x").unwrap();
        let cwd = || Some(root.to_string_lossy().into_owned());

        // Act
        let checked = place_of(cwd(), Path::new("/h"), Dirty::Check);
        let skipped = place_of(cwd(), Path::new("/h"), Dirty::Skip);

        // Assert
        assert_eq!(checked.dirty, Some(true));
        assert_eq!(skipped.dirty, None);
        assert_eq!(skipped.cwd, checked.cwd);
        assert_eq!(skipped.worktree, checked.worktree);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_directory_that_is_gone_reads_as_gone_and_never_as_clean() {
        let place = place_of(
            Some("/definitely/not/here".into()),
            Path::new("/h"),
            Dirty::Check,
        );
        assert!(place.gone);
        assert_eq!(place.dirty, None);
    }
}
