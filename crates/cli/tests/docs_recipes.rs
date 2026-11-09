// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Runs the docs pages that carry the `<!-- verify: run -->` marker.
//!
//! Every page under `docs/recipes/` must start with a marker, `run` or
//! `names`. A `run` page (and any other docs page with the same marker, such
//! as `docs/code-search.md`) has its `bash` blocks executed for real in a
//! sandbox: a temp folder, a cleared environment, no network, the fixture repo
//! as the working folder. A block fails on a non-zero exit, on a timeout, or
//! when an `# expect: <text>` line matches no full output line (`*` matches
//! any run of characters). A `names` page is only read by `docs_names.rs`.
//!
//! The runner takes the bin folder links, the timeout and the parent
//! environment as parameters, so the synthetic cases below use `true`,
//! `false`, `echo` and `sleep` and never need the real binary.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MARKER_RUN: &str = "<!-- verify: run -->";
const MARKER_NAMES: &str = "<!-- verify: names -->";
const EXPECT: &str = "# expect:";
/// The limit for one block of a real recipe.
const REAL_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------- parsing

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Run,
    Names,
}

#[derive(Debug)]
struct Block {
    /// 1-based line of the opening fence.
    line: usize,
    code: String,
    /// True when `<!-- verify: names -->` sits on the line before the fence.
    names_only: bool,
}

/// The marker on the first non-blank line after the first H1 outside a fence.
fn page_class(text: &str) -> Option<Class> {
    let mut in_fence = false;
    let mut seen_h1 = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if !seen_h1 {
            seen_h1 = line.starts_with("# ");
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        return match line.trim() {
            MARKER_RUN => Some(Class::Run),
            MARKER_NAMES => Some(Class::Names),
            _ => None,
        };
    }
    None
}

/// The fenced `bash` blocks of a page, in order.
fn bash_blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(info) = lines[i].trim_start().strip_prefix("```") {
            let lang = info.split_whitespace().next().unwrap_or("");
            // A fence inside a list item is indented; its code is too.
            let indent = lines[i].len() - lines[i].trim_start().len();
            let start = i;
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i].get(indent..).unwrap_or(lines[i].trim_start()));
                i += 1;
            }
            if lang == "bash" {
                blocks.push(Block {
                    line: start + 1,
                    code: code.join("\n"),
                    names_only: start > 0 && lines[start - 1].trim() == MARKER_NAMES,
                });
            }
        }
        i += 1;
    }
    blocks
}

// ----------------------------------------------------------------- expect

/// True when `pattern` matches the whole of `line`, `*` matching any run.
fn glob_matches(pattern: &str, line: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == line;
    }
    let first = parts[0];
    let last = parts[parts.len() - 1];
    if !line.starts_with(first) {
        return false;
    }
    let mut rest = &line[first.len()..];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(at) => rest = &rest[at + mid.len()..],
            None => return false,
        }
    }
    rest.ends_with(last) && rest.len() >= last.len()
}

/// The `# expect:` patterns of a block.
fn expectations(code: &str) -> Vec<String> {
    code.lines()
        .filter_map(|l| l.trim().strip_prefix(EXPECT))
        .map(|t| t.trim().to_string())
        .collect()
}

/// Names the first expectation that matches no output line.
fn missing_expectation(code: &str, output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().map(str::trim_end).collect();
    expectations(code)
        .into_iter()
        .find(|want| !lines.iter().any(|l| glob_matches(want, l)))
}

// ----------------------------------------------------------------- runner

/// What the runner needs from its caller.
struct Setup<'a> {
    /// Names linked into the sandbox bin folder, each to a real file.
    links: &'a [(&'a str, &'a Path)],
    /// The limit for one block.
    timeout: Duration,
    /// The parent environment. Only its `PATH` is read, to find `git`.
    parent_env: &'a BTreeMap<String, String>,
    /// Copied into the working folder, then made a git repo with one commit.
    template: Option<&'a Path>,
}

/// The sandbox folder of one page: the process id keeps parallel test
/// binaries apart and the name keeps two pages apart.
fn sandbox_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("docs-recipes-{}-{name}", std::process::id()))
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy");
        }
    }
}

/// The folder in the parent `PATH` that holds `git`, if any.
fn git_dir(parent_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let path = parent_env.get("PATH")?;
    std::env::split_paths(path).find(|dir| dir.join("git").is_file())
}

/// The cleared environment of every child. Nothing from the parent passes
/// through except the folder that holds `git`.
fn child_env(
    home: &Path,
    bin: &Path,
    parent_env: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    let home_s = |p: &Path| p.to_string_lossy().into_owned();
    let mut path = vec![home_s(bin)];
    if let Some(git) = git_dir(parent_env) {
        path.push(home_s(&git));
    }
    path.push("/usr/bin".into());
    path.push("/bin".into());
    let kv = |k: &str, v: String| (k.to_string(), v);
    vec![
        kv("HOME", home_s(home)),
        kv("XDG_CONFIG_HOME", home_s(&home.join("config"))),
        kv("XDG_DATA_HOME", home_s(&home.join("data"))),
        kv("XDG_CACHE_HOME", home_s(&home.join("cache"))),
        kv("CLAUDE_CONFIG_DIR", home_s(&home.join("claude"))),
        kv("PATH", path.join(":")),
        kv("NO_COLOR", "1".into()),
        kv("TZ", "UTC".into()),
        kv("PLAYBOOK_TELEMETRY_TZ", "UTC".into()),
        kv("LANG", "C.UTF-8".into()),
        kv("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        kv("GIT_CONFIG_NOSYSTEM", "1".into()),
        kv("GIT_AUTHOR_NAME", "Recipe Runner".into()),
        kv("GIT_AUTHOR_EMAIL", "runner@example.invalid".into()),
        kv("GIT_COMMITTER_NAME", "Recipe Runner".into()),
        kv("GIT_COMMITTER_EMAIL", "runner@example.invalid".into()),
    ]
}

/// What one block did.
struct Outcome {
    status: Option<i32>,
    timed_out: bool,
    output: String,
}

/// Runs `code` with `bash -euo pipefail` in its own process group. On a
/// timeout, and when the block ends, the whole group is killed, so a
/// background child cannot outlive the block.
fn run_block(
    code: &str,
    cwd: &Path,
    env: &[(String, String)],
    out_file: &Path,
    timeout: Duration,
) -> Outcome {
    let out = fs::File::create(out_file).expect("output file");
    let err = out.try_clone().expect("clone");
    let mut child = Command::new("bash")
        .args(["-euo", "pipefail", "-c", code])
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()
        .expect("spawn bash");
    let group = rustix::process::Pid::from_raw(i32::try_from(child.id()).expect("pid"));
    let kill_group = || {
        if let Some(pid) = group {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    };
    let start = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break (status.code(), false);
        }
        if start.elapsed() >= timeout {
            kill_group();
            let _ = child.wait();
            break (None, true);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    kill_group();
    Outcome {
        status,
        timed_out,
        output: fs::read_to_string(out_file).unwrap_or_default(),
    }
}

/// Runs the `bash` blocks of one page. `name` keys the sandbox folder and
/// `shown` names the page in a failure message.
fn run_page(name: &str, shown: &str, text: &str, setup: &Setup) -> Result<(), String> {
    let root = sandbox_dir(name);
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let bin = root.join("bin");
    let work = root.join("work");
    fs::create_dir_all(&bin).expect("bin dir");
    fs::create_dir_all(&home).expect("home dir");
    for (link, target) in setup.links {
        std::os::unix::fs::symlink(target, bin.join(link)).expect("link");
    }
    match setup.template {
        Some(template) => copy_tree(template, &work),
        None => fs::create_dir_all(&work).expect("work dir"),
    }
    let env = child_env(&home, &bin, setup.parent_env);
    if setup.template.is_some() {
        let git = "git init -q -b main \
                   && git remote add origin https://example.invalid/demo/recipes.git \
                   && git add . && git commit -q -m 'Add the demo files'";
        let made = run_block(
            git,
            &work,
            &env,
            &root.join("git.out"),
            Duration::from_secs(60),
        );
        if made.status != Some(0) {
            return Err(format!(
                "{shown}: could not prepare the sandbox repo: {}",
                made.output
            ));
        }
    }
    let result = run_blocks(shown, text, setup, &work, &env, &root);
    let _ = fs::remove_dir_all(&root);
    result
}

fn run_blocks(
    shown: &str,
    text: &str,
    setup: &Setup,
    work: &Path,
    env: &[(String, String)],
    root: &Path,
) -> Result<(), String> {
    for block in bash_blocks(text) {
        if block.names_only {
            continue;
        }
        let done = run_block(
            &block.code,
            work,
            env,
            &root.join("block.out"),
            setup.timeout,
        );
        let shown_block = format!(
            "{shown}:{} block:\n{}\noutput:\n{}",
            block.line, block.code, done.output
        );
        if done.timed_out {
            return Err(format!(
                "{shown}:{} timed out after {:?}\n{shown_block}",
                block.line, setup.timeout
            ));
        }
        if done.status != Some(0) {
            return Err(format!(
                "{shown}:{} exited with {:?}\n{shown_block}",
                block.line, done.status
            ));
        }
        if let Some(want) = missing_expectation(&block.code, &done.output) {
            return Err(format!(
                "{shown}:{} expected an output line matching `{want}`\n{shown_block}",
                block.line
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------- repo scope

fn root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

/// Every Markdown file under `dir`, skipping the ADR folder.
fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "adr") {
                markdown_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

fn rel(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn recipe_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    markdown_files(&root().join("docs/recipes"), &mut files);
    files.sort();
    files
}

/// Every docs page with the `run` marker: the recipes and any other page.
fn run_pages() -> Vec<PathBuf> {
    let mut files = Vec::new();
    markdown_files(&root().join("docs"), &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|p| page_class(&fs::read_to_string(p).unwrap_or_default()) == Some(Class::Run))
        .collect()
}

// ------------------------------------------------------------ synthetic

fn parent_env() -> BTreeMap<String, String> {
    BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())])
}

fn synthetic(name: &str, body: &str, timeout: Duration) -> Result<(), String> {
    let page = format!("# How to test\n\n{MARKER_RUN}\n\n{body}");
    let parent = parent_env();
    let setup = Setup {
        links: &[],
        timeout,
        parent_env: &parent,
        template: None,
    };
    run_page(name, &format!("{name}.md"), &page, &setup)
}

const SHORT: Duration = Duration::from_secs(1);

#[test]
fn a_passing_block_and_its_expect_line_pass() {
    let body = "```bash\necho ready\n# expect: ready\n```\n";
    assert_eq!(synthetic("pass", body, SHORT), Ok(()));
}

#[test]
fn a_failing_exit_names_the_block_and_the_code() {
    let err = synthetic("exit", "```bash\nfalse\n```\n", SHORT).expect_err("must fail");
    assert!(err.contains("exit.md:5"), "{err}");
    assert!(err.contains("exited with Some(1)"), "{err}");
    assert!(err.contains("false"), "{err}");
}

#[test]
fn an_expect_line_that_matches_nothing_names_its_text() {
    let body = "```bash\necho ready\n# expect: all done\n```\n";
    let err = synthetic("expect", body, SHORT).expect_err("must fail");
    assert!(err.contains("`all done`"), "{err}");
}

#[test]
fn an_expect_line_matches_a_whole_line_only() {
    let body = "```bash\necho ready now\n# expect: ready\n```\n";
    assert!(synthetic("whole", body, SHORT).is_err());
}

#[test]
fn a_star_matches_a_run_of_characters() {
    let body = "```bash\necho 'indexed 3 files (0.1 s)'\n# expect: indexed * files (*)\n```\n";
    assert_eq!(synthetic("star", body, SHORT), Ok(()));
}

#[test]
fn glob_edges() {
    assert!(glob_matches("*", ""));
    assert!(glob_matches("a*", "a"));
    assert!(glob_matches("*z", "xyz"));
    assert!(glob_matches("a*b*c", "a--b--c"));
    assert!(!glob_matches("a*b", "a"));
    assert!(!glob_matches("ab*ba", "aba"), "the ends must not overlap");
    assert!(!glob_matches("a*", "b"));
}

#[test]
fn a_timeout_kills_the_background_child_too() {
    let pid_file = std::env::temp_dir().join(format!("docs-recipes-pid-{}", std::process::id()));
    let _ = fs::remove_file(&pid_file);
    let body = format!(
        "```bash\nsleep 5 &\necho $! > {}\nsleep 5\n```\n",
        pid_file.display()
    );
    let err = synthetic("timeout", &body, SHORT).expect_err("must time out");
    assert!(err.contains("timed out"), "{err}");
    let pid = fs::read_to_string(&pid_file)
        .expect("pid file")
        .trim()
        .to_string();
    let _ = fs::remove_file(&pid_file);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let alive = Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .expect("kill")
            .success();
        let state = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid])
            .output()
            .expect("ps");
        let zombie = String::from_utf8_lossy(&state.stdout)
            .trim()
            .starts_with('Z');
        if !alive || zombie {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "background child {pid} still runs"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_names_block_on_a_run_page_is_skipped() {
    let body =
        format!("{MARKER_NAMES}\n```bash\nfalse\n```\n\n```bash\necho ok\n# expect: ok\n```\n");
    assert_eq!(synthetic("optdown", &body, SHORT), Ok(()));
}

#[test]
fn an_indented_block_in_a_list_runs_with_its_indent_removed() {
    let body = "1. Step.\n\n   ```bash\n   echo ready\n   # expect: ready\n   ```\n";
    assert_eq!(synthetic("indent", body, SHORT), Ok(()));
}

#[test]
fn blocks_of_other_languages_are_not_run() {
    assert_eq!(synthetic("lang", "```text\nfalse\n```\n", SHORT), Ok(()));
}

#[test]
fn blocks_of_one_page_share_a_working_folder() {
    let body = "```bash\necho hi > made.txt\n```\n\n```bash\ncat made.txt\n# expect: hi\n```\n";
    assert_eq!(synthetic("shared", body, SHORT), Ok(()));
}

#[test]
fn two_pages_get_two_folders() {
    assert_ne!(sandbox_dir("one"), sandbox_dir("two"));
    let a = "```bash\necho a > only-a.txt\n```\n";
    let b = "```bash\ntest ! -e only-a.txt\n```\n";
    assert_eq!(synthetic("iso-a", a, SHORT), Ok(()));
    assert_eq!(synthetic("iso-b", b, SHORT), Ok(()));
}

#[test]
fn the_parent_environment_does_not_reach_the_child() {
    let parent = BTreeMap::from([
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        (
            "CLAUDE_CONFIG_DIR".to_string(),
            "/leaked/claude".to_string(),
        ),
        ("SECRET_TOKEN".to_string(), "leaked".to_string()),
    ]);
    let page = format!(
        "# How to test\n\n{MARKER_RUN}\n\n```bash\ntest -z \"${{SECRET_TOKEN:-}}\"\ncase \"$CLAUDE_CONFIG_DIR\" in /leaked*) exit 9;; esac\ncase \"$HOME\" in */docs-recipes-*) ;; *) exit 8;; esac\necho \"$TZ\"\n# expect: UTC\n```\n"
    );
    let setup = Setup {
        links: &[],
        timeout: SHORT,
        parent_env: &parent,
        template: None,
    };
    assert_eq!(run_page("env", "env.md", &page, &setup), Ok(()));
}

#[test]
fn the_sandbox_links_the_binaries_it_is_given() {
    // A script, not /bin/echo: on some systems echo is a multicall binary
    // that refuses to run under another name.
    let target = std::env::temp_dir().join(format!("docs-recipes-tool-{}", std::process::id()));
    fs::write(&target, "#!/bin/sh\necho linked\n").expect("script");
    fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let parent = parent_env();
    let page =
        format!("# How to test\n\n{MARKER_RUN}\n\n```bash\ntool linked\n# expect: linked\n```\n");
    let setup = Setup {
        links: &[("tool", &target)],
        // The first run of a new file can be slow while the OS scans it.
        timeout: Duration::from_secs(20),
        parent_env: &parent,
        template: None,
    };
    let result = run_page("links", "links.md", &page, &setup);
    let _ = fs::remove_file(&target);
    assert_eq!(result, Ok(()));
}

#[test]
fn the_template_becomes_a_git_repo_with_an_origin() {
    let template =
        std::env::temp_dir().join(format!("docs-recipes-template-{}", std::process::id()));
    fs::create_dir_all(&template).expect("template");
    fs::write(template.join("a.txt"), "x\n").expect("file");
    let parent: BTreeMap<String, String> = std::env::vars().collect();
    let page = format!("# How to test\n\n{MARKER_RUN}\n\n```bash\ngit remote get-url origin\n# expect: https://example.invalid/*\ngit ls-files\n# expect: a.txt\n```\n");
    let setup = Setup {
        links: &[],
        timeout: Duration::from_secs(10),
        parent_env: &parent,
        template: Some(&template),
    };
    let result = run_page("template", "template.md", &page, &setup);
    let _ = fs::remove_dir_all(&template);
    assert_eq!(result, Ok(()));
}

#[test]
fn the_marker_is_read_after_the_title() {
    assert_eq!(
        page_class("# T\n\n<!-- verify: run -->\n"),
        Some(Class::Run)
    );
    assert_eq!(
        page_class("# T\n<!-- verify: names -->\n"),
        Some(Class::Names)
    );
    assert_eq!(
        page_class("# T\n\ntext first\n<!-- verify: run -->\n"),
        None
    );
    assert_eq!(page_class("# T\n"), None);
    assert_eq!(page_class("```\n# T\n```\n"), None);
}

// ------------------------------------------------------------- repo cases

#[test]
fn every_recipe_page_has_a_marker() {
    let files = recipe_files();
    assert!(!files.is_empty(), "docs/recipes has no pages");
    for file in files {
        let text = fs::read_to_string(&file).expect("read");
        assert!(
            page_class(&text).is_some(),
            "{} has no marker. Put `{MARKER_RUN}` or `{MARKER_NAMES}` on the first line after the title.",
            rel(&file)
        );
    }
}

#[test]
fn the_recipe_index_lists_every_recipe_with_its_class() {
    let index = fs::read_to_string(root().join("docs/recipes/README.md")).expect("index");
    for file in recipe_files() {
        let name = file
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        if name == "README.md" {
            continue;
        }
        let class = match page_class(&fs::read_to_string(&file).expect("read")) {
            Some(Class::Run) => "run",
            _ => "names",
        };
        let row = index
            .lines()
            .find(|l| l.starts_with('|') && l.contains(&format!("]({name})")));
        let row =
            row.unwrap_or_else(|| panic!("docs/recipes/README.md has no table row for {name}"));
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        assert!(
            cells.contains(&class),
            "the row for {name} must name its class `{class}`: {row}"
        );
    }
}

#[test]
fn the_repo_pages_run() {
    let playbook = Path::new(env!("CARGO_BIN_EXE_playbook"));
    let links = [
        ("playbook", playbook),
        ("pb", playbook),
        ("pbtop", playbook),
    ];
    let template = root().join("crates/cli/tests/fixtures/recipes/repo");
    let parent: BTreeMap<String, String> = std::env::vars().collect();
    let setup = Setup {
        links: &links,
        timeout: REAL_TIMEOUT,
        parent_env: &parent,
        template: Some(&template),
    };
    let mut failures = Vec::new();
    for page in run_pages() {
        let text = fs::read_to_string(&page).expect("read");
        let name = page
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();
        if let Err(e) = run_page(&name, &rel(&page), &text, &setup) {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
