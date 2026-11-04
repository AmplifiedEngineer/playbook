// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Behavioural tests for the launcher internals ported in WU-16.
//!
//! These modules delete files, so every scenario asserts what SURVIVES as well
//! as what goes. A retention test that only counted deletions would pass while
//! deleting everything.

use playbook::cc::{bust_cache, project_slug, retention};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// HOME, PWD and CCD_KEEP are process-wide, and cargo runs the tests in this
/// binary on parallel threads. One shared lock, not one per module: four
/// separate mutexes each guarding the same variables serialise nothing, and
/// produced an intermittent failure where one module swapped HOME out from
/// under another mid-test.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An isolated HOME, so a test can never reach the real `~/.claude`.
struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let home = std::env::temp_dir().join(format!(
            "playbook-cc-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        fs::create_dir_all(&home).expect("sandbox home");
        Self { home }
    }

    fn claude(&self) -> PathBuf {
        self.home.join(".claude")
    }

    fn touch(&self, rel: &str) -> PathBuf {
        let path = self.claude().join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "x").expect("write");
        path
    }

    fn mkdir(&self, rel: &str) -> PathBuf {
        let path = self.claude().join(rel);
        fs::create_dir_all(&path).expect("mkdir");
        path
    }
}

fn exists(p: &Path) -> bool {
    p.exists()
}

/// Pins a file's mtime to `millis` past a fixed whole-second base, so ordering
/// never depends on the wall clock or on which second a write lands in.
fn set_mtime_ms(path: &Path, millis: u64) {
    let when = std::time::UNIX_EPOCH + Duration::from_millis(1_700_000_000_000 + millis);
    fs::File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_modified(when)
        .expect("set mtime");
}

fn set_mtime(path: &Path, secs: u64) {
    set_mtime_ms(path, secs * 1000);
}

fn age_apart(paths: &[PathBuf]) {
    for (i, p) in paths.iter().enumerate() {
        fs::write(p, "x").expect("write");
        set_mtime(p, i as u64 * 10);
    }
}

mod retention_tests {
    use super::*;

    /// A fixed time for the gc lock that prune takes.
    const NOW: i64 = 1_800_000_000;

    fn playbook_runtime(sb: &Sandbox, sid: &str) -> PathBuf {
        sb.home.join(".config/playbook/runtime").join(sid)
    }

    /// Serialises the env mutations, since CCD_KEEP and HOME are process-wide.
    fn with_env<T>(home: &Path, keep: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = lock_env();
        let prev_home = std::env::var_os("HOME");
        let prev_keep = std::env::var_os("CCD_KEEP");
        std::env::set_var("HOME", home);
        match keep {
            Some(k) => std::env::set_var("CCD_KEEP", k),
            None => std::env::remove_var("CCD_KEEP"),
        }
        let out = f();
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_keep {
            Some(v) => std::env::set_var("CCD_KEEP", v),
            None => std::env::remove_var("CCD_KEEP"),
        }
        out
    }

    /// Three transcripts with distinct mtimes, plus the sidecar and runtime
    /// state retention is supposed to remove alongside each one.
    fn seed(sb: &Sandbox, cwd: &str, count: usize) -> Vec<PathBuf> {
        let dir = format!("projects/{}", project_slug(cwd));
        let mut files = Vec::new();
        for i in 0..count {
            let sid = format!("sess{i}");
            files.push(sb.touch(&format!("{dir}/{sid}.jsonl")));
            sb.mkdir(&format!("{dir}/{sid}"));
            fs::create_dir_all(playbook_runtime(sb, &sid)).expect("runtime dir");
        }
        sb.touch("runtime/sentinel");
        age_apart(&files);
        files
    }

    #[test]
    fn keeps_the_newest_and_deletes_the_rest() {
        let sb = Sandbox::new("keep2");
        let cwd = "/tmp/proj";
        let files = seed(&sb, cwd, 3);
        with_env(&sb.home, Some("2"), || retention::prune(cwd, NOW));

        // seed() ages oldest first, so the last written is newest.
        assert!(exists(&files[2]), "newest must survive");
        assert!(exists(&files[1]), "second newest must survive");
        assert!(!exists(&files[0]), "oldest must go");

        let dir = sb.claude().join("projects").join(project_slug(cwd));
        assert!(
            !exists(&dir.join("sess0")),
            "the sidecar directory goes with its transcript"
        );
        assert!(
            !exists(&playbook_runtime(&sb, "sess0")),
            "the playbook runtime folder is tombstoned with its transcript"
        );
        assert!(
            exists(&playbook_runtime(&sb, "sess2")),
            "a surviving session keeps its runtime folder"
        );
        assert!(
            exists(&sb.claude().join("runtime/sentinel")),
            "nothing under the Claude runtime folder is touched"
        );
        assert!(
            exists(&dir.join("sess2")),
            "a surviving transcript keeps its sidecar"
        );
    }

    #[test]
    fn retention_leaves_claude_runtime_alone() {
        let sb = Sandbox::new("claude-runtime");
        let cwd = "/tmp/proj";
        seed(&sb, cwd, 3);
        sb.mkdir("runtime/sess0");

        with_env(&sb.home, Some("2"), || retention::prune(cwd, NOW));

        assert!(exists(&sb.claude().join("runtime/sess0")));
    }

    #[test]
    fn pruned_runtime_folder_can_be_restored() {
        let sb = Sandbox::new("restore");
        let cwd = "/tmp/proj";
        seed(&sb, cwd, 3);
        fs::write(playbook_runtime(&sb, "sess0").join("state"), "kept").expect("state");

        with_env(&sb.home, Some("2"), || retention::prune(cwd, NOW));
        let index = fs::read_to_string(sb.home.join(".config/playbook/gc/index.jsonl"))
            .expect("the move is recorded");

        assert!(!exists(&playbook_runtime(&sb, "sess0")));
        assert!(index.contains("sess0"));
    }

    #[test]
    fn retention_skips_when_locked() {
        let sb = Sandbox::new("locked");
        let cwd = "/tmp/proj";
        seed(&sb, cwd, 3);
        let gc_dir = sb.home.join(".config/playbook/gc");
        let now = playbook::common::time::now_secs();
        let clock = playbook::gc::FixedClock(now);

        let held = playbook::gc::lock::with_run_lock(
            &gc_dir,
            std::time::Duration::ZERO,
            &clock,
            &playbook::gc::lock::NoHook,
            |_guard| with_env(&sb.home, Some("2"), || retention::prune(cwd, now)),
        );

        assert!(held.is_ok());
        assert!(
            exists(&playbook_runtime(&sb, "sess0")),
            "a held gc lock leaves the folder for the next sweep"
        );
    }

    /// The floor exists because fork and clean resume both read the
    /// second-newest transcript as their parent.
    #[test]
    fn a_keep_of_one_is_raised_to_the_floor_of_two() {
        let sb = Sandbox::new("floor");
        let cwd = "/tmp/proj";
        let files = seed(&sb, cwd, 3);
        with_env(&sb.home, Some("1"), || retention::prune(cwd, NOW));

        assert!(exists(&files[2]), "newest survives");
        assert!(exists(&files[1]), "the floor keeps the second newest");
        assert!(!exists(&files[0]), "the oldest still goes");
    }

    #[test]
    fn zero_disables_retention_entirely() {
        let sb = Sandbox::new("zero");
        let cwd = "/tmp/proj";
        let files = seed(&sb, cwd, 3);
        with_env(&sb.home, Some("0"), || retention::prune(cwd, NOW));
        for f in &files {
            assert!(exists(f), "nothing may be deleted when disabled");
        }
    }

    #[test]
    fn fewer_transcripts_than_keep_deletes_nothing() {
        let sb = Sandbox::new("under");
        let cwd = "/tmp/proj";
        let files = seed(&sb, cwd, 2);
        with_env(&sb.home, Some("5"), || retention::prune(cwd, NOW));
        for f in &files {
            assert!(exists(f), "under the limit nothing is pruned");
        }
    }

    #[test]
    fn a_missing_project_directory_is_a_no_op() {
        let sb = Sandbox::new("missing");
        with_env(&sb.home, Some("2"), || {
            retention::prune("/tmp/never-used", NOW)
        });
    }

    /// The slug has to match the shell's `${PWD//[^a-zA-Z0-9]/-}` exactly, or
    /// the binary would prune a different directory than the launcher wrote.
    #[test]
    fn the_project_slug_matches_the_shell_expansion() {
        assert_eq!(project_slug("/Users/x/Work_dir.1"), "-Users-x-Work-dir-1");
        assert_eq!(project_slug("abc123"), "abc123");
        assert_eq!(project_slug("/a b/c"), "-a-b-c");
    }

    /// `$PWD` wins over `current_dir()`, which resolves symlinks. macOS ships
    /// `/tmp` and `/var` as symlinks into `/private`, so resolving would build
    /// a slug the launcher never wrote to and prune nothing. Found by the
    /// differential: the shell deleted a transcript and the port did not.
    #[test]
    fn the_logical_path_is_preferred_over_the_resolved_one() {
        let _guard = lock_env();
        let prev = std::env::var_os("PWD");
        std::env::set_var("PWD", "/tmp/logical-path");
        assert_eq!(playbook::cc::logical_cwd(), "/tmp/logical-path");
        match prev {
            Some(v) => std::env::set_var("PWD", v),
            None => std::env::remove_var("PWD"),
        }
    }
}

mod config_drift_tests {
    use super::*;
    use playbook::cc::config_drift;

    /// A sandbox HOME with a settings.json the config hash covers.
    fn sandbox_with_hasher(tag: &str) -> Sandbox {
        let sb = Sandbox::new(tag);
        fs::create_dir_all(sb.claude()).expect("mkdir .claude");
        fs::write(sb.claude().join("settings.json"), "{\"a\":1}\n").expect("write settings");
        sb
    }

    fn set_settings(sb: &Sandbox, body: &str) {
        fs::write(sb.claude().join("settings.json"), body).expect("write settings");
    }

    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = lock_env();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    const CWD: &str = "/tmp/some-project";

    #[test]
    fn the_first_check_reports_drift_and_creates_a_baseline() {
        let sb = sandbox_with_hasher("first");
        let (drifted, marker) = with_home(&sb.home, || {
            (config_drift::drifted(CWD), config_drift::marker_path(CWD))
        });
        assert!(drifted, "with no baseline, config counts as changed");
        assert!(marker.is_file(), "the check must leave a baseline behind");
    }

    /// The behaviour the blueprint singles out: `drifted` re-stamps whether or
    /// not it found a match. Without that, one drift would be reported forever.
    #[test]
    fn a_second_check_reports_no_drift_because_the_first_re_stamped() {
        let sb = sandbox_with_hasher("restamp");
        let (first, second) = with_home(&sb.home, || {
            (config_drift::drifted(CWD), config_drift::drifted(CWD))
        });
        assert!(first, "first call has no baseline");
        assert!(!second, "the first call must have re-stamped");
    }

    #[test]
    fn changing_config_reports_drift_once_then_clears() {
        let sb = sandbox_with_hasher("change");
        with_home(&sb.home, || config_drift::stamp(CWD));

        set_settings(&sb, "{\"a\":2}\n");
        let (changed, after) = with_home(&sb.home, || {
            (config_drift::drifted(CWD), config_drift::drifted(CWD))
        });
        assert!(changed, "a settings edit changes the hash");
        assert!(!after, "and the report clears once re-stamped");
    }

    #[test]
    fn stamping_makes_the_next_check_quiet() {
        let sb = sandbox_with_hasher("stamp");
        let quiet = with_home(&sb.home, || {
            config_drift::stamp(CWD);
            config_drift::drifted(CWD)
        });
        assert!(!quiet, "a fresh stamp is the current config by definition");
    }
}

mod clean_resume_tests {
    use super::*;
    use playbook::cc::clean_resume::{self, CleanError};

    const OLD_SID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    /// One transcript covering every branch: kept types, each of the five
    /// override commands, a non-override slash command, a permission entry,
    /// and lines with no sessionId at all.
    fn transcript() -> String {
        [
            r#"{"type":"user","sessionId":"old","content":"hello"}"#,
            r#"{"type":"system","sessionId":"old","content":"<command-name>/model</command-name> sonnet"}"#,
            r#"{"type":"system","sessionId":"old","content":"<command-name>/effort</command-name> high"}"#,
            r#"{"type":"system","sessionId":"old","content":"<command-name>/clear</command-name>"}"#,
            r#"{"type":"assistant","sessionId":"old","content":"hi"}"#,
            r#"{"type":"system","sessionId":"old","content":"<command-name>/output-style</command-name> x"}"#,
            r#"{"type":"system","sessionId":"old","content":"<command-name>/style</command-name> y"}"#,
            r#"{"type":"system","sessionId":"old","content":"<command-name>/config</command-name> z"}"#,
            r#"{"type":"system","sessionId":"old","content":"permission mode changed"}"#,
            r#"{"type":"summary","customTitle":"proj"}"#,
            r#"{"type":"user","content":"no session id field"}"#,
        ]
        .join("\n")
            + "\n"
    }

    fn seed(sb: &Sandbox) -> PathBuf {
        let dir = sb.mkdir("projects/proj");
        fs::write(dir.join(format!("{OLD_SID}.jsonl")), transcript()).expect("write");
        let sidecar = dir.join(OLD_SID);
        fs::create_dir_all(&sidecar).expect("mkdir");
        fs::write(sidecar.join("tool.txt"), "sidecar").expect("write");
        dir
    }

    fn lines_of(path: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(path)
            .expect("read")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid json line"))
            .collect()
    }

    #[test]
    fn strips_every_override_command_and_keeps_the_rest() {
        let sb = Sandbox::new("clean");
        let dir = seed(&sb);
        let prepared = clean_resume::prepare(&dir, OLD_SID).expect("prepare");

        assert_eq!(prepared.stripped, 5, "the five override commands go");
        assert_eq!(prepared.kept, 6);

        let out = lines_of(&dir.join(format!("{}.jsonl", prepared.new_sid)));
        let contents: Vec<String> = out
            .iter()
            .map(|v| {
                v.get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();

        // Kept on purpose: other slash commands, and permission entries, which
        // the user should have to re-grant rather than have replayed silently.
        assert!(contents.iter().any(|c| c.contains("/clear")));
        assert!(contents.iter().any(|c| c.contains("permission mode")));
        for banned in ["/model", "/effort", "/config", "/output-style", "/style"] {
            assert!(
                !contents.iter().any(|c| c.contains(banned)),
                "{banned} must be stripped"
            );
        }
    }

    #[test]
    fn rewrites_session_ids_without_inventing_the_field() {
        let sb = Sandbox::new("sid");
        let dir = seed(&sb);
        let prepared = clean_resume::prepare(&dir, OLD_SID).expect("prepare");
        let out = lines_of(&dir.join(format!("{}.jsonl", prepared.new_sid)));

        let with_field: Vec<&serde_json::Value> = out
            .iter()
            .filter(|v| v.get("sessionId").is_some())
            .collect();
        assert!(!with_field.is_empty());
        for v in with_field {
            assert_eq!(v["sessionId"].as_str(), Some(prepared.new_sid.as_str()));
        }
        // A line that never had sessionId must not gain one.
        assert!(out
            .iter()
            .any(|v| v.get("sessionId").is_none() && v.get("customTitle").is_some()));
    }

    #[test]
    fn the_original_transcript_and_sidecar_are_untouched() {
        let sb = Sandbox::new("original");
        let dir = seed(&sb);
        let before = fs::read_to_string(dir.join(format!("{OLD_SID}.jsonl"))).expect("read");
        let prepared = clean_resume::prepare(&dir, OLD_SID).expect("prepare");

        let after = fs::read_to_string(dir.join(format!("{OLD_SID}.jsonl"))).expect("read");
        assert_eq!(before, after, "the original must never be rewritten");

        // Copied, not symlinked, so harness writes cannot leak back.
        let copied = dir.join(&prepared.new_sid).join("tool.txt");
        assert!(copied.is_file(), "the sidecar is copied to the new session");
        assert!(
            !copied
                .symlink_metadata()
                .expect("stat")
                .file_type()
                .is_symlink(),
            "a symlink would leak new-session writes into the original"
        );
    }

    #[test]
    fn a_missing_transcript_is_an_error_not_a_panic() {
        let sb = Sandbox::new("missing");
        let dir = sb.mkdir("projects/proj");
        match clean_resume::prepare(&dir, "nope") {
            Err(CleanError::TranscriptMissing(_)) => {}
            other => panic!("expected TranscriptMissing, got {other:?}"),
        }
    }

    #[test]
    fn each_new_session_id_is_distinct_and_uuid_shaped() {
        let sb = Sandbox::new("uuid");
        let dir = seed(&sb);
        let a = clean_resume::prepare(&dir, OLD_SID)
            .expect("prepare")
            .new_sid;
        let b = clean_resume::prepare(&dir, OLD_SID)
            .expect("prepare")
            .new_sid;
        assert_ne!(a, b, "two clones must not collide");
        assert!(
            playbook::cc::sessions::is_uuid(&a),
            "{a} is not uuid shaped"
        );
        assert!(playbook::cc::sessions::is_uuid(&b));
    }

    /// Differential against the real `jq` pipeline the shell used. Skipped when
    /// jq is absent, which is the state WU-14 leaves the machine in.
    #[test]
    fn matches_the_jq_pipeline_line_for_line() {
        if Command::new("jq").arg("--version").output().is_err() {
            eprintln!("SKIP: jq not installed");
            return;
        }
        let sb = Sandbox::new("jq-diff");
        let dir = seed(&sb);
        let src = dir.join(format!("{OLD_SID}.jsonl"));

        let pattern = r"<command-name>/(model|effort|config|output-style|style)</command-name>";
        let filtered = Command::new("jq")
            .args(["-c", "--arg", "pat", pattern])
            .arg(r#"select(.type != "system" or ((.content // "") | test($pat) | not))"#)
            .arg(&src)
            .output()
            .expect("jq should run");
        assert!(filtered.status.success());
        let shell_lines: Vec<serde_json::Value> = String::from_utf8_lossy(&filtered.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("jq emits valid json"))
            .collect();

        let prepared = clean_resume::prepare(&dir, OLD_SID).expect("prepare");
        let mut rust_lines = lines_of(&dir.join(format!("{}.jsonl", prepared.new_sid)));

        // The two mint different session ids by design, so normalise that field
        // and compare everything else.
        let normalise = |v: &mut serde_json::Value| {
            if let Some(obj) = v.as_object_mut() {
                if obj.contains_key("sessionId") {
                    obj.insert("sessionId".into(), serde_json::Value::String("N".into()));
                }
            }
        };
        let mut shell_lines = shell_lines;
        shell_lines.iter_mut().for_each(normalise);
        rust_lines.iter_mut().for_each(normalise);

        assert_eq!(
            shell_lines, rust_lines,
            "the port must keep exactly the lines jq keeps"
        );
    }
}

mod sessions_tests {
    use super::*;
    use playbook::cc::sessions;

    const UUID_A: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const UUID_B: &str = "11111111-2222-3333-4444-555555555555";

    /// Transcripts pinned ten whole seconds apart, since the shell ranks and
    /// deduplicates on whole-second mtimes.
    fn seed(sb: &Sandbox, entries: &[(&str, &str)]) -> PathBuf {
        let dir = sb.mkdir("projects/proj");
        for (i, (sid, title)) in entries.iter().enumerate() {
            let path = dir.join(format!("{sid}.jsonl"));
            fs::write(
                &path,
                format!("{{\"type\":\"summary\",\"customTitle\":\"{title}\"}}\n"),
            )
            .expect("write");
            set_mtime(&path, i as u64 * 10);
        }
        dir
    }

    #[test]
    fn finds_a_session_by_its_custom_title() {
        let sb = Sandbox::new("find");
        let dir = seed(&sb, &[(UUID_A, "oldsession"), (UUID_B, "newsession")]);
        assert_eq!(
            sessions::find_by_title(&dir, "oldsession").as_deref(),
            Some(UUID_A)
        );
        assert_eq!(
            sessions::find_by_title(&dir, "newsession").as_deref(),
            Some(UUID_B)
        );
    }

    #[test]
    fn a_lookup_miss_returns_nothing() {
        let sb = Sandbox::new("miss");
        let dir = seed(&sb, &[(UUID_A, "oldsession")]);
        assert!(sessions::find_by_title(&dir, "nosuchtitle").is_none());
        assert!(sessions::find_by_title(Path::new("/nope"), "x").is_none());
    }

    #[test]
    fn enumeration_is_newest_first() {
        let sb = Sandbox::new("order");
        let dir = seed(&sb, &[(UUID_A, "oldsession"), (UUID_B, "newsession")]);
        let listed = sessions::enumerate(&dir);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, UUID_B, "the newest transcript comes first");
        assert_eq!(listed[0].title, "newsession");
    }

    /// Pins the `sort -rnu -k1,1` quirk. `-u` deduplicates on the sort key, so
    /// two sessions written in the same second collapse to one and the other
    /// vanishes from the listing. Ported for parity, not because it is right.
    #[test]
    fn same_second_transcripts_collapse_to_one() {
        let sb = Sandbox::new("dedup");
        let dir = sb.mkdir("projects/proj");
        for (i, sid) in [UUID_A, UUID_B].into_iter().enumerate() {
            let path = dir.join(format!("{sid}.jsonl"));
            fs::write(&path, "{\"customTitle\":\"x\"}\n").expect("write");
            // Same whole second, different sub-second times, so the test
            // pins the truncation and never straddles a second boundary.
            set_mtime_ms(&path, i as u64 * 500);
        }
        assert_eq!(
            sessions::enumerate(&dir).len(),
            1,
            "the shell drops all but one per whole-second mtime"
        );
    }

    #[test]
    fn transcripts_in_different_seconds_are_both_listed() {
        let sb = Sandbox::new("twosec");
        let dir = sb.mkdir("projects/proj");
        for (i, sid) in [UUID_A, UUID_B].into_iter().enumerate() {
            let path = dir.join(format!("{sid}.jsonl"));
            fs::write(&path, "{\"customTitle\":\"x\"}\n").expect("write");
            set_mtime(&path, i as u64);
        }
        assert_eq!(sessions::enumerate(&dir).len(), 2);
    }

    #[test]
    fn non_uuid_transcripts_are_skipped() {
        let sb = Sandbox::new("uuid");
        let dir = seed(&sb, &[(UUID_A, "real")]);
        fs::write(dir.join("memory.jsonl"), "{\"customTitle\":\"skip\"}\n").expect("write");
        let listed = sessions::enumerate(&dir);
        assert_eq!(listed.len(), 1, "only UUID-named files are sessions");
        assert_eq!(listed[0].id, UUID_A);
    }

    #[test]
    fn the_uuid_check_matches_the_shell_regex() {
        assert!(sessions::is_uuid(UUID_A));
        assert!(!sessions::is_uuid("memory"));
        assert!(!sessions::is_uuid(""));
        assert!(!sessions::is_uuid("AAAAAAAA-bbbb-cccc-dddd-eeeeeeeeeeee"));
        assert!(!sessions::is_uuid("aaaaaaaa-bbbb-cccc-dddd"));
    }

    #[test]
    fn a_transcript_without_a_title_renders_a_placeholder() {
        let sb = Sandbox::new("untitled");
        let dir = sb.mkdir("projects/proj");
        fs::write(dir.join(format!("{UUID_A}.jsonl")), "{\"type\":\"x\"}\n").expect("write");
        assert_eq!(sessions::enumerate(&dir)[0].title, "(no title)");
    }

    #[test]
    fn a_missing_project_directory_reports_no_sessions() {
        let out = sessions::render_list(Path::new("/nope"), "/tmp/myproject");
        assert_eq!(out, "no sessions for myproject\n");
    }
}

mod bust_cache_tests {
    use super::*;

    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = lock_env();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    #[test]
    fn clears_the_four_caches_and_leaves_everything_else() {
        let sb = Sandbox::new("bust");
        let snapshot = sb.touch("shell-snapshots/snapshot-abc.sh");
        let other_snapshot = sb.touch("shell-snapshots/keep-me.txt");
        let hash = sb.touch("runtime/sess1/config-hash");
        let other_runtime = sb.touch("runtime/sess1/telemetry.jsonl");
        let catalog = sb.touch("plugins/plugin-catalog-cache.json");
        let other_plugin = sb.touch("plugins/keep-me.json");
        let backup = sb.touch("backups/install-1/file.txt");
        let settings = sb.touch("settings.json");

        with_home(&sb.home, bust_cache::bust);

        assert!(!exists(&snapshot), "stale snapshot must go");
        assert!(!exists(&hash), "config-hash must go, nested one level down");
        assert!(!exists(&catalog), "plugin catalog cache must go");

        // A POPULATED backup directory survives. The shell used `find -delete`,
        // which cannot remove a non-empty directory, so using remove_dir_all
        // here would destroy backups the shell preserved. Caught by the
        // differential; both spellings clear the cache, only one keeps the data.
        assert!(exists(&backup), "a non-empty backup directory must survive");

        // The other half of the contract: nothing beyond those caches.
        assert!(exists(&other_snapshot), "a non-snapshot file stays");
        assert!(exists(&other_runtime), "other runtime state stays");
        assert!(exists(&other_plugin), "other plugin files stay");
        assert!(exists(&settings), "settings.json is never touched");
    }

    #[test]
    fn an_empty_backup_directory_is_removed() {
        let sb = Sandbox::new("bust-empty-dir");
        let empty = sb.mkdir("backups/empty-one");
        with_home(&sb.home, bust_cache::bust);
        assert!(
            !exists(&empty),
            "an empty directory is what find -delete can remove"
        );
    }

    #[test]
    fn missing_directories_are_a_no_op() {
        let sb = Sandbox::new("bust-empty");
        fs::create_dir_all(sb.claude()).expect("mkdir");
        with_home(&sb.home, bust_cache::bust);
    }
}
