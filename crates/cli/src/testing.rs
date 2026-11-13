// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Helpers for the integration tests under `tests/`. Not part of the public
//! interface. They live in `playbook-common`.

pub use playbook_common::testing::*;

/// Stores the graph in `json` (`{"nodes": [...], "edges": [...]}`) in the
/// `memory.db` beside the memory folder `mem_dir`, as a fixture for the tools
/// that read the graph. Panics on a document that is not JSON.
pub fn write_graph_fixture(mem_dir: &std::path::Path, json: &str) {
    if let Err(e) = crate::hooks::rebuild_memory_graph::import_json(mem_dir, json) {
        panic!("could not store the graph fixture: {e}");
    }
}

/// Writes `content` to `path`, creating the parent folders. Panics on failure.
#[cfg(test)]
pub fn write_file(path: &std::path::Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// A scratch folder that is removed on drop, so a failing test cleans up
/// too.
#[cfg(test)]
pub struct Scratch(pub std::path::PathBuf);

#[cfg(test)]
impl Scratch {
    pub fn new(tag: &str) -> Self {
        Scratch(scratch_root(tag))
    }

    /// Writes `lines` as a JSONL file named `t.jsonl` and returns its path.
    pub fn write(&self, lines: &[String]) -> std::path::PathBuf {
        let path = self.0.join("t.jsonl");
        write_file(&path, &(lines.join("\n") + "\n"));
        path
    }
}

#[cfg(test)]
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A fresh, created scratch folder named after `tag`.
#[cfg(test)]
pub fn scratch_root(tag: &str) -> std::path::PathBuf {
    let dir = crate::common::test_support::scratch_dir(tag);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The created playbook root inside a fresh scratch home named after `tag`.
#[cfg(test)]
pub fn scratch_playbook_root(tag: &str) -> std::path::PathBuf {
    let home = crate::common::test_support::scratch_dir(tag);
    let root = crate::common::paths::playbook_root_from(&home);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Set in a child process that [`in_scratch_home`] started, so the child
/// knows to run the test body instead of starting another child.
#[cfg(test)]
const SCRATCH_CHILD_ENV: &str = "PLAYBOOK_TEST_SCRATCH_HOME_CHILD";

/// Panics unless `$HOME` is set and is not the home the account database
/// lists for this user. [`in_scratch_home`] calls it in its child; a hook
/// test that does not use that helper is not covered.
#[cfg(test)]
pub fn assert_home_is_scratch() {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let Some(home) = home.filter(|h| !h.as_os_str().is_empty()) else {
        panic!("HOME is not set: a hook test must run in a scratch home");
    };
    assert!(
        !crate::common::real_home::is_account_home(&home),
        "HOME is the real home of the account ({}): a hook test must run in a scratch home",
        home.display()
    );
}

/// Runs the unit test `test_path` (for example `hooks::tests::my_test`) in a
/// child process whose `$HOME` and `XDG_*` point at a fresh scratch folder,
/// and fails when the child fails. Call it first in the test: in the parent
/// it returns `false` once the child passed, in the child it returns `true`
/// and the body should run. The home is process-wide state, so a child is
/// the only way to give one test its own home while its siblings run on
/// parallel threads.
///
/// ```ignore
/// if !playbook::testing::in_scratch_home("hooks::tests::my_test") {
///     return;
/// }
/// // body: runs with a scratch HOME
/// ```
#[cfg(test)]
pub fn in_scratch_home(test_path: &str) -> bool {
    if std::env::var_os(SCRATCH_CHILD_ENV).is_some() {
        assert_home_is_scratch();
        return true;
    }
    let home = std::env::temp_dir()
        .join(format!("pb-scratch-home-{}", run_id()))
        .join(test_path.replace("::", "-"));
    std::fs::create_dir_all(&home).expect("scratch home");
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("test binary"));
    cmd.args(["--exact", test_path, "--test-threads=1", "--nocapture"]);
    for (key, _) in std::env::vars_os() {
        let drop = key.to_str().is_some_and(|k| {
            ["PLAYBOOK_", "CLAUDE_", "XDG_"]
                .iter()
                .any(|p| k.starts_with(p))
        });
        if drop {
            cmd.env_remove(&key);
        }
    }
    let out = cmd
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
        .env(SCRATCH_CHILD_ENV, "1")
        .output()
        .expect("start the scratch-home child");
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        out.status.success(),
        "{test_path} failed in a scratch home:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // A misspelled path matches no test and "passes" with zero run.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "the scratch-home child ran no test named {test_path}"
    );
    false
}
