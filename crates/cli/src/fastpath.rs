// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Hot paths that run before clap builds its command tree.
//!
//! Claude Code runs `playbook hook <name>` on every tool call and
//! `playbook statusline` on every redraw, and the whole clap tree costs 2 to
//! 3 ms to build. These forms are recognised from the raw arguments. Anything
//! else, including a wrong name, an extra argument or a flag, returns `None`
//! and falls through to clap, so `--help` and every error message are unchanged.
//!
//! `shell-init` is the one removed command recognised here (ADR 0044). An old
//! rc line runs `eval "$(playbook shell-init)"` on every shell start, so it
//! gets one line on stderr, nothing on stdout and exit 0 in place of clap's
//! error. Remove this case at the 1.0 release.

use crate::HookName;
use clap::ValueEnum;
use std::ffi::OsString;

/// A command recognised without clap.
#[derive(Debug, Clone, Copy)]
pub enum Fast {
    Hook(HookName),
    Statusline,
    Version,
    /// `playbook shell-init`, removed: print `SHELL_INIT_REMOVED` on stderr
    /// and exit 0.
    RemovedShellInit,
}

/// What `playbook shell-init` prints on stderr since the command was removed.
pub const SHELL_INIT_REMOVED: &str = "playbook shell-init was removed (ADR 0044). \
Delete the line that runs it from your shell rc file, or run: playbook setup";

/// The fast command for `args` (the full argv, program name first), if any.
pub fn classify(args: &[OsString]) -> Option<Fast> {
    let first = args.get(1)?.to_str()?;
    match (first, args.len()) {
        ("hook", 3) => {
            let name = args.get(2)?.to_str()?;
            HookName::from_str(name, false).ok().map(Fast::Hook)
        }
        ("statusline", 2) => Some(Fast::Statusline),
        ("--version" | "-V", 2) => Some(Fast::Version),
        // Old rc lines may pass `--shell zsh` or `--shell bash`.
        ("shell-init", _) => Some(Fast::RemovedShellInit),
        _ => None,
    }
}

/// The name the Homebrew formula links to this binary so that it runs as the
/// standalone dashboard (ADR 0041).
pub const PBTOP_PROGRAM: &str = "pbtop";

/// Whether the binary was started under the name `pbtop`, from the file name
/// of the program in `args` (the full argv). A link keeps the name it was
/// started by, so a `pbtop` link to `playbook` is told apart from `playbook`.
pub fn invoked_as_pbtop(args: &[OsString]) -> bool {
    args.first()
        .map(std::path::Path::new)
        .and_then(std::path::Path::file_stem)
        .is_some_and(|stem| stem == PBTOP_PROGRAM)
}

/// What `playbook --version` prints, the same text clap renders.
pub fn version_text() -> String {
    format!("playbook {}\n", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_three_hot_forms_are_recognised() {
        assert!(matches!(
            classify(&argv(&["playbook", "hook", "session-init"])),
            Some(Fast::Hook(HookName::SessionInit))
        ));
        assert!(matches!(
            classify(&argv(&["playbook", "statusline"])),
            Some(Fast::Statusline)
        ));
        assert!(matches!(
            classify(&argv(&["playbook", "--version"])),
            Some(Fast::Version)
        ));
        assert!(matches!(
            classify(&argv(&["playbook", "-V"])),
            Some(Fast::Version)
        ));
    }

    #[test]
    fn the_removed_shell_init_is_recognised_with_or_without_its_old_flag() {
        for parts in [
            &["playbook", "shell-init"][..],
            &["playbook", "shell-init", "--shell", "zsh"],
        ] {
            assert!(
                matches!(classify(&argv(parts)), Some(Fast::RemovedShellInit)),
                "{parts:?}"
            );
        }
    }

    #[test]
    fn anything_else_falls_through_to_clap() {
        for parts in [
            &["playbook"][..],
            &["playbook", "hook"],
            &["playbook", "hook", "nope"],
            &["playbook", "hook", "session-init", "extra"],
            &["playbook", "hook", "--help"],
            &["playbook", "hook", "Session-Init"],
            &["playbook", "statusline", "--help"],
            &["playbook", "--version", "x"],
            &["playbook", "--help"],
            &["playbook", "config", "list"],
            &["playbook", "shell-ini"],
            &["playbook", "Shell-Init"],
            &["playbook", "shell"],
        ] {
            assert!(classify(&argv(parts)).is_none(), "{parts:?}");
        }
    }

    #[test]
    fn a_program_named_pbtop_is_recognised_by_its_file_name() {
        for program in ["pbtop", "/opt/homebrew/bin/pbtop", "./pbtop", "pbtop.exe"] {
            assert!(
                invoked_as_pbtop(&argv(&[program, "--version"])),
                "{program}"
            );
        }
    }

    #[test]
    fn other_program_names_are_not_pbtop() {
        for program in [
            "playbook",
            "pb",
            "pbtop-old",
            "mypbtop",
            "/opt/pbtop/playbook",
            "",
        ] {
            assert!(!invoked_as_pbtop(&argv(&[program])), "{program:?}");
        }
        assert!(!invoked_as_pbtop(&[]));
    }

    #[test]
    fn a_non_utf8_argument_falls_through() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let args = vec![
                OsString::from("playbook"),
                OsString::from("hook"),
                OsString::from_vec(vec![0xff, 0xfe]),
            ];
            assert!(classify(&args).is_none());
        }
    }
}
