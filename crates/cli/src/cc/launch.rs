// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook cc launch`: the launcher behind `playbook cc` and `playbook ccd`.
//!
//! Subcommands: (none) resume or start, `clean`, `fresh`, `raw [sid]`, `list`,
//! `prune`, `worktree <branch>`. A child cannot change its parent shell's
//! directory, so after a session in a new worktree it prints the `cd` to run.

use super::{bust_cache, clean_resume, config_drift, retention, sessions, worktree_run};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SYSTEM_PROMPT_REL: &str = "prompts/SYSTEM_PROMPT.md";

/// Options that consume the next argument, so it is not mistaken for a subcommand.
fn takes_value(opt: &str) -> bool {
    matches!(
        opt,
        "--system-prompt-file"
            | "--system-prompt"
            | "--append-system-prompt"
            | "--append-system-prompt-file"
            | "--settings"
            | "--setting-sources"
            | "--model"
            | "--fallback-model"
            | "--agents"
            | "--plugin-dir"
            | "--plugin-url"
            | "--effort"
            | "--permission-mode"
            | "--name"
            | "-n"
    )
}

/// Splits leading flags (with their values) from the rest; the subcommand is
/// the first non-flag token, so `playbook ccd clean` works.
fn split_flags(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        let arg = &args[i];
        flags.push(arg.clone());
        i += 1;
        if !arg.contains('=') && takes_value(arg) && i < args.len() {
            flags.push(args[i].clone());
            i += 1;
        }
    }
    (flags, args[i..].to_vec())
}

/// Claude Code's documented switch for its own auto memory.
const DISABLE_AUTO_MEMORY_ENV: &str = "CLAUDE_CODE_DISABLE_AUTO_MEMORY";

/// Whether `memory.source` is `playbook`, so Claude Code's auto memory stays
/// off in this session and only playbook memory is used.
fn playbook_memory_only(home: &Path) -> bool {
    matches!(
        crate::config::resolve("memory.source", home, None),
        Ok((serde_json::Value::String(v), _)) if v == "playbook"
    )
}

/// Flags that replace Claude Code's system prompt.
const REPLACE_FLAGS: [&str; 2] = ["--system-prompt", "--system-prompt-file"];

/// Flags that append to it. The launcher's contract is replace, so these are refused.
const APPEND_FLAGS: [&str; 2] = ["--append-system-prompt", "--append-system-prompt-file"];

/// Whether a flag is present before any `--`, where Claude Code's own option parsing stops.
fn has_flag(args: &[String], flags: &[&str]) -> bool {
    args.iter().take_while(|a| a.as_str() != "--").any(|a| {
        flags
            .iter()
            .any(|f| a == f || a.strip_prefix(f).is_some_and(|r| r.starts_with('=')))
    })
}

/// The system prompt arguments to add, or the exit code when the launch is refused.
///
/// Rule: playbook always replaces Claude Code's prompt with its own file and
/// never appends. A replace flag from the user wins and playbook adds none
/// (one stderr line, interactive runs only). An append flag from the user is
/// refused with exit code 2. With `systemPrompt.enabled` off (`prompt` is
/// `None`) playbook adds nothing and refuses nothing.
fn prompt_args(
    args: &[String],
    prompt: Option<&Path>,
    interactive: bool,
) -> Result<(Vec<String>, Option<String>), i32> {
    let Some(prompt) = prompt else {
        return Ok((Vec::new(), None));
    };
    if has_flag(args, &APPEND_FLAGS) {
        eprintln!(
            "playbook: --append-system-prompt and --append-system-prompt-file are not allowed here. \
             This launcher replaces Claude Code's system prompt and never appends to it. \
             Use --system-prompt-file to supply your own, or run plain claude."
        );
        return Err(2);
    }
    if has_flag(args, &REPLACE_FLAGS) {
        let note = interactive.then(|| {
            "playbook: using your system prompt flag, the playbook prompt is not loaded".to_string()
        });
        return Ok((Vec::new(), note));
    }
    if !prompt.is_file() {
        let note = interactive.then(|| {
            format!(
                "playbook: system prompt not found at {}, starting with Claude Code's own (run /playbook:setup)",
                prompt.display()
            )
        });
        return Ok((Vec::new(), note));
    }
    Ok((
        vec![
            "--system-prompt-file".into(),
            prompt.to_string_lossy().into_owned(),
        ],
        None,
    ))
}

/// The one-line prompt notice, held until the screen is cleared so it stays visible.
static NOTICES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn hold_notice(note: String) {
    if let Ok(mut notes) = NOTICES.lock() {
        notes.push(note);
    }
}

/// What became of `CLAUDE_CODE_EFFORT_LEVEL` for this launch.
struct EnvEffort {
    /// The variable was set (to anything), so the launcher adds no default effort.
    was_set: bool,
    /// `--effort <level>` standing in for it, empty when there is none.
    flag: Vec<String>,
}

/// Takes `CLAUDE_CODE_EFFORT_LEVEL` out of the child's environment and passes
/// its level as `--effort` instead, ahead of the user's own arguments.
///
/// Claude Code's docs and `${CLAUDE_EFFORT}` probes agree: the variable
/// overrides a skill's or subagent's `effort` frontmatter, while `--effort`
/// does not. Nothing is raised: a component keeps the effort it ships with
/// or is configured to, and the main session keeps the level the variable set.
/// A value that is not a level (including `auto`) is dropped from the
/// environment with no flag, so the model default applies as it would have.
/// An unreadable value is left alone. One line goes to stderr when the
/// environment changes.
fn take_effort_env(args: &[String], all: &mut Vec<String>) -> EnvEffort {
    let Some(raw) = std::env::var_os(crate::models::EFFORT_ENV) else {
        return EnvEffort {
            was_set: false,
            flag: Vec::new(),
        };
    };
    let Some(value) = raw.to_str().map(|v| v.trim().to_ascii_lowercase()) else {
        return EnvEffort {
            was_set: true,
            flag: Vec::new(),
        };
    };
    std::env::remove_var(crate::models::EFFORT_ENV);
    let level = crate::effort::rank(&value).is_some();
    let flag = if level
        && !args
            .iter()
            .any(|a| a == "--effort" || a.starts_with("--effort="))
    {
        vec!["--effort".to_string(), value.clone()]
    } else {
        Vec::new()
    };
    let note = if !flag.is_empty() {
        format!("moved to --effort {value}")
    } else if level {
        "dropped, your --effort applies".to_string()
    } else {
        "dropped (not a level), the model default applies".to_string()
    };
    hold_notice(format!(
        "playbook: {}={value} {note}, so playbook skills, commands and agents keep their own effort",
        crate::models::EFFORT_ENV
    ));
    all.extend(flag.iter().cloned());
    EnvEffort {
        was_set: true,
        flag,
    }
}

/// Runs the launcher and returns the exit code of the session (or the failure).
pub fn run(skip_permissions: bool, args: &[String]) -> i32 {
    let mut all: Vec<String> = Vec::new();
    if skip_permissions {
        all.push("--dangerously-skip-permissions".into());
    }
    let prompt = crate::common::paths::playbook_root().join(SYSTEM_PROMPT_REL);
    let enabled = crate::init::system_prompt::enabled(&crate::common::home_dir());
    {
        use std::io::IsTerminal;
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        match prompt_args(args, enabled.then_some(prompt.as_path()), interactive) {
            Ok((extra, note)) => {
                all.extend(extra);
                note.into_iter().for_each(hold_notice);
            }
            Err(code) => return code,
        }
    }
    let cwd_now = PathBuf::from(super::logical_cwd());
    let home = crate::common::home_dir();
    let user_settings = home.join(".claude/settings.json");
    let ceiling = crate::effort::ceiling(&home, &home.join(".claude"), &cwd_now);
    // `CLAUDE_CODE_EFFORT_LEVEL` beats a skill's or agent's own `effort`, so a
    // session that exports `max` would raise every playbook component to max.
    // The level moves to `--effort`, which a component's own effort overrides.
    let env_effort = take_effort_env(args, &mut all);
    let mut effort = crate::models::effort_in_force(
        &[&env_effort.flag[..], args].concat(),
        &user_settings,
        // Only Claude Code's own cap lowers a stated level: playbook passes no
        // `--settings`, so the session really runs at the level it states.
        ceiling.claude.as_deref(),
    );
    // Playbook owns this command line, not Claude Code's settings: a session
    // that stated no effort runs at medium, and one that did is left alone.
    if let Some(default) = crate::models::session_default_effort(
        effort.as_deref(),
        env_effort.was_set,
        ceiling.effective.as_deref(),
    ) {
        all.push("--effort".into());
        all.push(default.clone());
        effort = Some(default);
    }
    if let Some(flags) = crate::models::launcher_flags(args, &user_settings, effort.as_deref()) {
        all.extend(flags);
    }
    super::stale_variants::remove_leftovers(&std::env::temp_dir());
    for (var, id) in crate::models::env_overrides(&home) {
        std::env::set_var(var, id);
    }
    all.extend_from_slice(args);
    if playbook_memory_only(&crate::common::home_dir()) {
        // The session's own environment only. Claude Code's settings files
        // and memory directories are never touched.
        std::env::set_var(DISABLE_AUTO_MEMORY_ENV, "1");
    }

    // The session's own environment only, and only when no telemetry setting
    // exists anywhere. Claude Code's settings files are read, never written.
    for (var, value) in super::telemetry::session_env(&crate::common::home_dir(), &cwd_now) {
        std::env::set_var(var, value);
    }

    let mut cwd = cwd_now;
    let rc = dispatch(&mut cwd, &all);
    retention::prune(&cwd.to_string_lossy(), crate::common::time::now_secs());
    rc
}

/// Runs `f` with SIGINT swallowed: Ctrl-C reaches the whole foreground
/// group, and the launcher must outlive the session to prune and report. The
/// handler resets on exec, so claude keeps its own behaviour. Afterwards the
/// default action is back on.
#[cfg(unix)]
fn with_sigint_held<T>(f: impl FnOnce() -> T) -> T {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    // While false the handler does nothing; once true it ends the process.
    static DEFAULT_ACTION: OnceLock<Option<Arc<AtomicBool>>> = OnceLock::new();
    let flag = DEFAULT_ACTION.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register_conditional_default(signal_hook::consts::SIGINT, flag.clone())
            .ok()
            .map(|_| flag)
    });
    if let Some(flag) = flag {
        flag.store(false, Ordering::SeqCst);
    }
    let out = f();
    if let Some(flag) = flag {
        flag.store(true, Ordering::SeqCst);
    }
    out
}

#[cfg(not(unix))]
fn with_sigint_held<T>(f: impl FnOnce() -> T) -> T {
    f()
}

fn dispatch(cwd: &mut PathBuf, args: &[String]) -> i32 {
    bust_cache::bust();
    clear_screen();
    for note in NOTICES
        .lock()
        .map(|mut notes| std::mem::take(&mut *notes))
        .unwrap_or_default()
    {
        eprintln!("{note}");
    }
    let (flags, rest) = split_flags(args);
    let sub = rest.first().map(String::as_str).unwrap_or("");
    let cwd_str = cwd.to_string_lossy().into_owned();
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let project_dir = sessions::project_dir(&super::claude_dir(), &cwd_str);

    if !matches!(
        sub,
        "list"
            | "ls"
            | "--list"
            | "prune"
            | "--prune"
            | "worktree"
            | "--worktree"
            | "new"
            | "--new"
    ) {
        let _ = crate::trust::run(&cwd_str);
    }
    let tail = rest.get(1..).unwrap_or(&[]);

    match sub {
        "clean" | "--clean" => clean(cwd, &project_dir, &name, &flags, tail),
        "fresh" | "--fresh" => {
            println!("-> cc: fresh session (no resume; settings.json applied)");
            config_drift::stamp(&cwd_str);
            claude(cwd, &[&flags[..], &["-n".into(), name], tail].concat())
        }
        "raw" | "--raw" => raw(cwd, &project_dir, &name, &flags, tail),
        "list" | "ls" | "--list" => {
            print!("{}", sessions::render_list(&project_dir, &cwd_str));
            0
        }
        "prune" | "--prune" => {
            retention::prune(&cwd_str, crate::common::time::now_secs());
            0
        }
        "worktree" | "--worktree" | "new" | "--new" => worktree(cwd, &flags, tail),
        _ => resume(cwd, &project_dir, &name, &flags, &rest),
    }
}

fn clean(cwd: &Path, project_dir: &Path, name: &str, flags: &[String], tail: &[String]) -> i32 {
    let Some(old) = sessions::find_by_title(project_dir, name) else {
        println!("-> cc clean: no matching session for '{name}'; starting fresh");
        return claude(
            cwd,
            &[&["-n".into(), name.to_string()], flags, tail].concat(),
        );
    };
    match clean_resume::prepare(project_dir, &old) {
        Ok(done) => {
            println!(
                "-> cc clean: cloned {}... -> {}... (stripped {} override entries)",
                short(&old),
                short(&done.new_sid),
                done.stripped
            );
            println!("-> cc clean: settings.json + plugins + hooks reload from current config");
            config_drift::stamp(&cwd.to_string_lossy());
            let head = ["--resume".into(), done.new_sid, "-n".into(), name.into()];
            claude(cwd, &[&head[..], flags, tail].concat())
        }
        Err(clean_resume::CleanError::TranscriptMissing(path)) => {
            println!("-> cc clean: transcript missing at {}", path.display());
            1
        }
        Err(clean_resume::CleanError::Io(err)) => {
            println!("-> cc clean: {err}");
            1
        }
    }
}

fn raw(cwd: &Path, project_dir: &Path, name: &str, flags: &[String], tail: &[String]) -> i32 {
    let (sid, tail) = match tail.first() {
        Some(first) => (Some(first.clone()), &tail[1..]),
        None => (sessions::find_by_title(project_dir, name), tail),
    };
    let name_args = ["-n".to_string(), name.to_string()];
    match sid {
        None => {
            println!("-> cc raw: no matching session; starting fresh");
            claude(cwd, &[flags, &name_args[..], tail].concat())
        }
        Some(sid) => {
            println!("-> cc raw: resuming {sid} (no fork, overrides preserved)");
            let head = ["--resume".to_string(), sid];
            claude(cwd, &[flags, &head[..], &name_args[..], tail].concat())
        }
    }
}

/// Default: resume the newest titled session, forking when config drifted.
fn resume(cwd: &Path, project_dir: &Path, name: &str, flags: &[String], rest: &[String]) -> i32 {
    let cwd_str = cwd.to_string_lossy().into_owned();
    let name_args = ["-n".to_string(), name.to_string()];
    let Some(sid) = sessions::find_by_title(project_dir, name) else {
        config_drift::stamp(&cwd_str);
        return claude(cwd, &[flags, &name_args[..], rest].concat());
    };
    let mut head = vec!["--resume".to_string(), sid.clone()];
    if config_drift::drifted(&cwd_str) {
        head.push("--fork-session".into());
        println!("-> cc: config changed; forking to reload settings/plugins/hooks");
    }
    let (err_path, stderr) = private_err_file();
    let argv = [flags, &name_args[..], &head[..], rest].concat();
    let rc = spawn(cwd, &argv, stderr);
    let err = fs::read_to_string(&err_path).unwrap_or_default();
    let _ = fs::remove_file(&err_path);
    if err.contains("No conversation found") {
        println!(
            "-> cc: session {}... not found; starting fresh",
            short(&sid)
        );
        return claude(cwd, &[flags, &name_args[..], rest].concat());
    }
    eprint!("{err}");
    rc
}

fn worktree(cwd: &mut PathBuf, flags: &[String], tail: &[String]) -> i32 {
    let mut positional: Vec<&str> = Vec::new();
    let mut no_push = false;
    for arg in tail {
        match arg.as_str() {
            "--ai-resolve" | "--" => {}
            "--no-push" => no_push = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: playbook cc launch worktree <branch> [env-base-folder] [--no-push]"
                );
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("worktree: unknown option: {other}");
                return 2;
            }
            other => positional.push(other),
        }
    }
    let branch = positional.first().copied().unwrap_or("");
    // The flag reaches the worktree code through the same variable the
    // shell used; it is cleared again so the session does not inherit it.
    let had_no_push = std::env::var_os("WORKTREE_NO_PUSH").is_some();
    if no_push {
        std::env::set_var("WORKTREE_NO_PUSH", "1");
    }
    let setup = worktree_run::setup(cwd, branch, positional.get(1).copied());
    if no_push && !had_no_push {
        std::env::remove_var("WORKTREE_NO_PUSH");
    }
    let path = match setup {
        Ok((path, housekeep)) => {
            housekeep.spawn_detached();
            path
        }
        Err(code) => return code,
    };
    *cwd = path;
    let rc = dispatch(cwd, flags);
    eprintln!("{}", cd_hint(cwd));
    rc
}

fn cd_hint(path: &Path) -> String {
    let word = crate::common::shell::word(&path.to_string_lossy());
    format!("-> cc: to go to the worktree, run: cd {word}")
}

/// A fresh 0600 file that fails rather than follow a planted path, standing
/// in for the `mktemp` the shell used.
fn private_err_file() -> (PathBuf, Option<Stdio>) {
    let nanos = crate::common::time::now_nanos();
    let path = std::env::temp_dir().join(format!("playbook-cc-err-{}-{nanos}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(&path).ok();
    (path, file.map(Stdio::from))
}

fn claude(cwd: &Path, args: &[String]) -> i32 {
    spawn(cwd, args, None)
}

fn spawn(cwd: &Path, args: &[String], stderr: Option<Stdio>) -> i32 {
    let mut cmd = Command::new("claude");
    cmd.args(args)
        .current_dir(cwd)
        .env("PWD", cwd)
        // A shell that loaded the retired launcher function may still set it.
        .env_remove("PLAYBOOK_CC_CD_FILE");
    // Hooks inherit this PATH and read no rc file, so the session must be
    // able to find the binary that launched it even when the shell's PATH
    // does not list its directory.
    if let (Ok(exe), Some(path)) = (
        std::env::current_exe(),
        std::env::var_os("PATH").or_else(|| Some(Default::default())),
    ) {
        if let Some(fixed) = crate::init::path::path_with_self(&path, &exe) {
            cmd.env("PATH", fixed);
        }
    }
    if let Some(stderr) = stderr {
        cmd.stderr(stderr);
    }
    match with_sigint_held(|| cmd.status()) {
        Ok(status) => exit_code(status),
        Err(err) => {
            eprintln!("-> cc: could not run claude: {err}");
            127
        }
    }
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

fn clear_screen() {
    use std::io::{IsTerminal, Write};
    if std::io::stdout().is_terminal() {
        print!("\x1b[2J\x1b[H");
        let _ = std::io::stdout().flush();
    }
}

fn short(sid: &str) -> &str {
    sid.get(..8).unwrap_or(sid)
}

#[cfg(test)]
mod tests {
    use super::{cd_hint, prompt_args};
    use std::path::Path;

    #[test]
    fn the_cd_hint_quotes_a_path_with_shell_characters() {
        assert_eq!(
            cd_hint(Path::new("/r/.worktrees/a;id")),
            "-> cc: to go to the worktree, run: cd '/r/.worktrees/a;id'"
        );
        assert!(cd_hint(Path::new("/r/.worktrees/x")).ends_with("run: cd /r/.worktrees/x"));
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_notice_is_only_for_interactive_runs() {
        let missing = Some(Path::new("/nonexistent/SYSTEM_PROMPT.md"));
        let (_, quiet) = prompt_args(&[], missing, false).unwrap_or_default();
        assert!(quiet.is_none());
        let (_, loud) = prompt_args(&[], missing, true).unwrap_or_default();
        assert!(loud.is_some_and(|n| n.contains("not found")));
        let (args, note) =
            prompt_args(&v(&["--system-prompt=x"]), missing, true).unwrap_or_default();
        assert!(args.is_empty());
        assert!(note.is_some_and(|n| n.contains("your system prompt flag")));
        let (_, none) = prompt_args(&v(&["--system-prompt=x"]), missing, false).unwrap_or_default();
        assert!(none.is_none());
    }

    #[test]
    fn append_flags_after_a_double_dash_are_prompt_text() {
        let missing = Some(Path::new("/nonexistent/SYSTEM_PROMPT.md"));
        assert_eq!(
            prompt_args(&v(&["--append-system-prompt=x"]), missing, false),
            Err(2)
        );
        assert!(prompt_args(&v(&["--", "--append-system-prompt"]), missing, false).is_ok());
    }

    #[test]
    fn with_the_prompt_off_nothing_is_added_or_refused() {
        let got = prompt_args(&v(&["--append-system-prompt=x"]), None, true);

        assert_eq!(got, Ok((Vec::new(), None)));
    }

    use super::*;

    #[test]
    fn value_taking_flag_does_not_leak_its_value_as_the_subcommand() {
        let (flags, rest) = split_flags(&v(&["--model", "opus", "clean", "x"]));
        assert_eq!(flags, v(&["--model", "opus"]));
        assert_eq!(rest, v(&["clean", "x"]));
    }

    #[test]
    fn equals_form_and_boolean_flags_take_no_value() {
        let (flags, rest) = split_flags(&v(&["--model=opus", "--verbose", "list"]));
        assert_eq!(flags, v(&["--model=opus", "--verbose"]));
        assert_eq!(rest, v(&["list"]));
    }

    #[test]
    fn a_trailing_value_flag_with_no_value_is_kept_alone() {
        let (flags, rest) = split_flags(&v(&["--model"]));
        assert_eq!(flags, v(&["--model"]));
        assert!(rest.is_empty());
    }
}
