// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook init` is the deprecated name for `playbook setup` (ADR-0040).
//! Each test runs both names in their own scratch `$HOME` and checks they
//! write the same files, print the same output and exit with the same code.
//! Without a terminal, `init` prints no deprecation line. With stderr on a
//! pseudo terminal it prints the line once, on stderr, and nothing else
//! changes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The collector port every scratch home records before setup runs. Without
/// it, setup takes the default 47318 or, when a real collector holds that
/// port, the first free one it probes, which differs between the two runs.
/// Nothing listens here: setup only writes the number into the settings.
const PINNED_PORT: u16 = 47_999;

/// The token recorded with [`PINNED_PORT`].
const PINNED_TOKEN: &str = "setup-alias-test-token";

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    files: BTreeMap<String, String>,
}

/// Runs `playbook <name> <args>` in a fresh scratch home, with a cleared
/// environment so nothing reaches the real home through `XDG_*` or
/// `PLAYBOOK_*` variables.
fn run(name: &str, args: &[&str]) -> Run {
    run_with(name, args, false)
}

/// Like [`run`]. With `terminal_stderr`, stderr is a pseudo terminal, so the
/// binary sees a person at the terminal there while stdout stays a pipe.
fn run_with(name: &str, args: &[&str], terminal_stderr: bool) -> Run {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!(
        "playbook-setup-alias-{name}-{}-{n}",
        playbook::testing::run_id()
    ));
    assert!(home.starts_with(std::env::temp_dir()) && home != std::env::temp_dir());
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".claude")).expect("scratch home");
    let state_dir = home.join(".config/playbook/data");
    fs::create_dir_all(&state_dir).expect("scratch state dir");
    fs::write(
        state_dir.join("collector.json"),
        format!(r#"{{"pid":0,"port":{PINNED_PORT},"token":"{PINNED_TOKEN}"}}"#),
    )
    .expect("pin the collector port");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
    cmd.arg(name)
        .args(args)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/zsh")
        .env("TMPDIR", &home)
        .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
        .current_dir(&home);
    let out = if terminal_stderr {
        // A terminal on stderr turns on the release check, which would call
        // the network and write to the scratch home. Headless turns it off.
        cmd.env("PLAYBOOK_HEADLESS", "1");
        terminal::output(cmd)
    } else {
        cmd.output().expect("playbook should spawn")
    };
    let scrub = |text: &str| scrub_tokens(&text.replace(&home.display().to_string(), "{HOME}"));
    let mut files = BTreeMap::new();
    collect(&home, &home, &mut files);
    let files = files
        .into_iter()
        .map(|(path, body)| (path, scrub(&body)))
        .collect();
    let result = Run {
        code: out.status.code(),
        stdout: scrub(&String::from_utf8_lossy(&out.stdout)),
        stderr: scrub(&String::from_utf8_lossy(&out.stderr)),
        files,
    };
    let _ = fs::remove_dir_all(&home);
    result
}

/// Replaces each run of exactly 32 hex digits, such as the random telemetry
/// token, with `{TOKEN}`.
fn scrub_tokens(text: &str) -> String {
    let hex = regex::Regex::new("[0-9a-fA-F]+").unwrap();
    hex.replace_all(text, |m: &regex::Captures| {
        if m[0].len() == 32 {
            "{TOKEN}".to_string()
        } else {
            m[0].to_string()
        }
    })
    .into_owned()
}

/// The rows of every table in a SQLite file, one line each, with times
/// replaced by `{TIME}`, since the raw pages differ from run to run.
fn dump(path: &Path) -> String {
    use playbook_store::types::ValueRef;
    // Opened immutable and read only, so the dump writes no `-wal` or `-shm`
    // file into the folder the walk is listing.
    let uri = format!("file:{}?immutable=1", path.display());
    let flags = playbook_store::OpenFlags::SQLITE_OPEN_READ_ONLY
        | playbook_store::OpenFlags::SQLITE_OPEN_URI;
    let db = playbook_store::Connection::open_with_flags(uri, flags).expect("open db");
    let mut tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("list tables");
    let names: Vec<String> = tables
        .query_map([], |r| r.get(0))
        .expect("table names")
        .flatten()
        .collect();
    let mut out = String::new();
    for name in names {
        let mut rows = db
            .prepare(&format!("SELECT * FROM \"{name}\" ORDER BY 1"))
            .expect("select rows");
        let width = rows.column_count();
        let mut it = rows.query([]).expect("rows");
        while let Some(row) = it.next().expect("row") {
            let cells: Vec<String> = (0..width)
                .map(|i| match row.get_ref(i).expect("cell") {
                    ValueRef::Integer(n) if n > 1_000_000_000 => "{TIME}".into(),
                    ValueRef::Text(t) if t.starts_with(b"20") && t.contains(&b':') => {
                        "{TIME}".into()
                    }
                    other => format!("{other:?}"),
                })
                .collect();
            out.push_str(&format!("{name}: {}\n", cells.join(" | ")));
        }
    }
    out
}

/// A backup carries the time in its name, as in `settings.json.bak.1792073671`.
/// Replaces each run of nine or more digits with `{TIME}`.
fn stamp_free(name: &str) -> String {
    let digits = regex::Regex::new("[0-9]{9,}").unwrap();
    digits.replace_all(name, "{TIME}").into_owned()
}

/// Every regular file under `dir`, keyed by its path relative to `root`.
fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
            continue;
        }
        let rel = stamp_free(&path.strip_prefix(root).unwrap().display().to_string());
        let body = if rel.ends_with(".db") {
            dump(&path)
        } else {
            fs::read(&path)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        out.insert(rel, body);
    }
}

fn assert_same(args: &[&str]) -> Run {
    let setup = run("setup", args);
    let init = run("init", args);
    assert_eq!(setup.code, init.code, "exit codes differ for {args:?}");
    assert_eq!(setup.stdout, init.stdout, "stdout differs for {args:?}");
    assert_eq!(setup.stderr, init.stderr, "stderr differs for {args:?}");
    assert_eq!(
        setup.files.keys().collect::<Vec<_>>(),
        init.files.keys().collect::<Vec<_>>(),
        "different files for {args:?}"
    );
    for (path, body) in &setup.files {
        assert_eq!(
            Some(body),
            init.files.get(path),
            "{path} differs for {args:?}"
        );
    }
    assert!(
        !init.stderr.contains("deprecated") && !init.stdout.contains("deprecated"),
        "init printed a deprecation line without a terminal: {}{}",
        init.stdout,
        init.stderr
    );
    setup
}

#[test]
fn a_full_run_writes_the_same_files_under_both_names() {
    let setup = assert_same(&["--yes", "--no-telemetry", "--no-plugin"]);
    assert!(setup.code == Some(0) && setup.files.contains_key(".claude/settings.json"));
}

#[test]
fn a_run_with_part_flags_writes_the_same_files_under_both_names() {
    let setup = assert_same(&["--no-hooks", "--statusline", "--security", "--no-telemetry"]);
    assert_eq!(setup.code, Some(0));
    assert!(setup.files[".claude/settings.json"].contains("statusLine"));
}

#[test]
fn a_run_without_a_terminal_or_yes_matches_under_both_names() {
    assert_eq!(assert_same(&[]).code, Some(0));
}

#[test]
fn a_dry_run_and_explain_match_under_both_names() {
    for args in [&["--dry-run"][..], &["--explain"], &["--explain", "hooks"]] {
        let run = assert_same(args);
        assert_eq!(run.code, Some(0), "{args:?}");
        assert!(!run.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn only_telemetry_and_short_name_match_under_both_names() {
    let telemetry = assert_same(&["--only", "telemetry"]);
    assert_eq!(telemetry.code, Some(0));
    assert!(telemetry.files[".claude/settings.json"].contains("OTEL_EXPORTER_OTLP_ENDPOINT"));
    assert!(
        telemetry.files[".claude/settings.json"].contains(&format!("127.0.0.1:{PINNED_PORT}")),
        "the endpoint must use the pinned port, not a probed one"
    );
    assert_eq!(
        assert_same(&["--only", "short-name", "--short-name"]).code,
        Some(0)
    );
}

#[test]
fn only_completions_matches_under_both_names() {
    let run = assert_same(&["--only", "completions"]);

    assert_eq!(run.code, Some(0));
    assert!(run.files[".local/share/playbook/completions.zsh"].contains("playbook completions zsh"));
    assert_eq!(
        run.files[".zshrc"],
        format!("{}\n", playbook::init::completion::RC_LINE)
    );
}

#[test]
fn errors_exit_with_the_same_code_under_both_names() {
    assert_eq!(assert_same(&["--only", "no-such-part"]).code, Some(2));
    assert_eq!(assert_same(&["--explain", "no-such-part"]).code, Some(2));
}

#[test]
fn a_flag_conflict_exits_two_under_both_names() {
    // clap names the command in its usage line, so only the code is compared.
    let setup = run("setup", &["--dry-run", "--explain"]);
    let init = run("init", &["--dry-run", "--explain"]);
    assert_eq!(setup.code, Some(2));
    assert_eq!(init.code, Some(2));
}

/// Spawns a command with stderr on a pseudo terminal and stdout on a pipe.
mod terminal {
    use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
    use std::fs::{File, OpenOptions};
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::{Command, Output, Stdio};

    /// Runs `cmd` and returns its output. The text the terminal received is
    /// returned as stderr, with the `\r` the terminal adds removed.
    pub(crate) fn output(mut cmd: Command) -> Output {
        // Close on exec keeps the master out of processes other tests spawn
        // meanwhile; the child still gets the slave as its stderr.
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).expect("openpt failed");
        rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC).expect("fcntl failed");
        grantpt(&master).expect("grantpt failed");
        unlockpt(&master).expect("unlockpt failed");
        let name = ptsname(&master, Vec::new()).expect("ptsname failed");
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as i32)
            .open(std::ffi::OsStr::from_bytes(name.to_bytes()))
            .expect("open the pty slave");
        let master = File::from(master);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(slave));
        let child = cmd.spawn().expect("playbook should spawn");
        // Close the parent's copy of the terminal, so the reader ends once
        // the child exits.
        drop(cmd);
        let reader = std::thread::spawn(move || {
            let mut master = master;
            let mut text = Vec::new();
            let mut buf = [0u8; 4096];
            // The read fails with EIO once the last writer closes.
            while let Ok(n) = master.read(&mut buf) {
                if n == 0 {
                    break;
                }
                text.extend_from_slice(&buf[..n]);
            }
            text
        });
        let mut out = child.wait_with_output().expect("playbook should finish");
        out.stderr = reader
            .join()
            .expect("reader")
            .into_iter()
            .filter(|b| *b != b'\r')
            .collect();
        out
    }
}

#[test]
fn init_warns_once_on_a_terminal_and_only_on_stderr() {
    // Arrange
    let args = ["--dry-run"];

    // Act
    let setup = run_with("setup", &args, true);
    let init = run_with("init", &args, true);

    // Assert
    let line = format!("{}\n", playbook::INIT_DEPRECATION);
    assert_eq!(init.code, setup.code);
    assert_eq!(init.stdout, setup.stdout, "init changed stdout");
    assert!(!init.stdout.contains("deprecated"), "{}", init.stdout);
    assert_eq!(
        init.stderr.matches("deprecated").count(),
        1,
        "{}",
        init.stderr
    );
    assert_eq!(init.stderr, format!("{line}{}", setup.stderr));
    assert!(!setup.stderr.contains("deprecated"), "{}", setup.stderr);
}

#[test]
fn each_retired_collector_service_flag_prints_the_notice_on_a_terminal() {
    let runs: [&[&str]; 3] = [
        &["--only", "collector-service"],
        &["--collector-service", "--only", "hooks"],
        &["--no-collector-service", "--only", "hooks"],
    ];
    let notice = format!(
        "playbook setup: {}\n",
        playbook::init::choices::COLLECTOR_SERVICE_RETIRED
    );

    for args in runs {
        let run = run_with("setup", args, true);

        assert_eq!(run.code, Some(0), "{args:?}: {}", run.stderr);
        assert_eq!(run.stderr.matches(&notice).count(), 1, "{}", run.stderr);
        assert!(!run.stdout.contains("collector-service"), "{}", run.stdout);
    }
}
