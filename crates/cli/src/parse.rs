// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Turns argv into a [`Cli`], or into the text and exit code `main` prints.

use crate::renames::{rewrite, Rename, COMMAND_RENAMES};
use crate::Cli;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::FromArgMatches;
use std::ffi::OsString;

/// Exit code for a usage error.
pub const EXIT_USAGE: i32 = 2;
/// Prefix of the one error line printed after a command's help.
pub const ERROR_PREFIX: &str = "error: ";
const MISSING_ARGUMENTS: &str = "the following required arguments were not provided:";

/// What `main` prints to stderr before exiting with `code`.
#[derive(Debug)]
pub struct ParseExit {
    pub code: i32,
    pub stderr: String,
}

/// Why argv did not become a [`Cli`].
enum Failure {
    /// An error clap prints itself (help and version to stdout, other errors
    /// to stderr with colour), exiting with its own code.
    Clap(clap::Error),
    Exit(ParseExit),
}

/// The clap command tree for [`Cli`].
pub fn command() -> clap::Command {
    let mut cmd = <Cli as clap::CommandFactory>::command();
    cmd.build();
    let template = crate::command_index::grouped_template(&cmd);
    cmd.help_template(template)
}

/// Parses `args` (including the program name), applying the command renames.
pub fn parse(args: &[OsString]) -> Result<Cli, ParseExit> {
    parse_with_notice(args).map(|(cli, _)| cli)
}

/// Like [`parse`], and also returns the deprecation line when `args` named a
/// renamed command by its old path.
pub fn parse_with_notice(args: &[OsString]) -> Result<(Cli, Option<String>), ParseExit> {
    parse_renamed(args, COMMAND_RENAMES).map_err(|failure| match failure {
        Failure::Clap(err) => ParseExit {
            code: err.exit_code(),
            stderr: err.to_string(),
        },
        Failure::Exit(exit) => exit,
    })
}

/// Parses `args`, or prints the failure and exits like `Cli::parse` does.
/// Returns the deprecation line for a renamed command path, if any.
pub fn parse_or_exit(args: &[OsString]) -> (Cli, Option<String>) {
    match parse_renamed(args, COMMAND_RENAMES) {
        Ok(parsed) => parsed,
        Err(Failure::Clap(err)) => err.exit(),
        Err(Failure::Exit(exit)) => {
            eprint!("{}", exit.stderr);
            std::process::exit(exit.code)
        }
    }
}

fn parse_renamed(args: &[OsString], table: &[Rename]) -> Result<(Cli, Option<String>), Failure> {
    let (rewritten, notice) = rewrite(args, table);
    parse_inner(&rewritten).map(|cli| (cli, notice))
}

fn parse_inner(args: &[OsString]) -> Result<Cli, Failure> {
    let mut cmd = command();
    let mut matches = match cmd.try_get_matches_from_mut(args) {
        Ok(matches) => matches,
        Err(err) => return Err(classify(err, &cmd, args)),
    };
    Cli::from_arg_matches_mut(&mut matches).map_err(|err| classify(err, &cmd, args))
}

fn classify(err: clap::Error, cmd: &clap::Command, args: &[OsString]) -> Failure {
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => Failure::Clap(err),
        ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let help = deepest_command(cmd, args)
                .clone()
                .render_long_help()
                .to_string();
            Failure::Exit(ParseExit {
                code: EXIT_USAGE,
                stderr: help,
            })
        }
        ErrorKind::MissingRequiredArgument | ErrorKind::MissingSubcommand => {
            let help = deepest_command(cmd, args)
                .clone()
                .render_long_help()
                .to_string();
            Failure::Exit(ParseExit {
                code: EXIT_USAGE,
                stderr: format!("{help}\n{}\n", missing_line(&err)),
            })
        }
        _ => Failure::Clap(err),
    }
}

/// Walks `args` past the program name, skipping flags, down to the last word
/// that names a subcommand.
fn deepest_command<'a>(root: &'a clap::Command, args: &[OsString]) -> &'a clap::Command {
    let mut current = root;
    for word in args.iter().skip(1).filter_map(|a| a.to_str()) {
        if word.starts_with('-') {
            continue;
        }
        match current.find_subcommand(word) {
            Some(sub) => current = sub,
            None => break,
        }
    }
    current
}

fn missing_line(err: &clap::Error) -> String {
    match err.get(ContextKind::InvalidArg) {
        Some(ContextValue::Strings(names)) => {
            format!("{ERROR_PREFIX}{MISSING_ARGUMENTS} {}", names.join(" "))
        }
        _ => format!("{ERROR_PREFIX}{}", err.kind()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Command;
    use clap::Parser;

    fn argv(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn parse_err(words: &[&str]) -> ParseExit {
        parse(&argv(words)).expect_err("the argv should not parse")
    }

    fn last_line(stderr: &str) -> &str {
        stderr.trim_end().lines().last().expect("a last line")
    }

    fn assert_names_missing(stderr: &str, name: &str) {
        let line = last_line(stderr);
        assert!(
            line.starts_with("error: the following required arguments were not provided:"),
            "{line}"
        );
        assert!(line.contains(name), "{line}");
    }

    /// The long help of the command at `path`, as clap renders it.
    fn long_help(path: &[&str]) -> String {
        let mut cmd = command();
        cmd.build();
        let mut cur = &mut cmd;
        for word in path {
            cur = cur
                .find_subcommand_mut(word)
                .expect("the path names a command");
        }
        cur.render_long_help().to_string()
    }

    #[test]
    fn a_missing_positional_prints_long_help_then_one_error_line() {
        // Arrange
        let args = ["playbook", "trust"];

        // Act
        let exit = parse_err(&args);

        // Assert
        assert_eq!(exit.code, 2);
        assert!(exit.stderr.contains("Usage: playbook trust <PATH>"));
        assert!(exit.stderr.contains("Absolute path"));
        let about = command()
            .find_subcommand("trust")
            .and_then(|c| c.get_long_about().map(ToString::to_string))
            .expect("trust has a long about");
        let first_line = about.lines().next().expect("a first line");
        assert!(exit.stderr.contains(first_line));
        assert_eq!(
            exit.stderr.trim_end().lines().last(),
            Some("error: the following required arguments were not provided: <PATH>")
        );
    }

    #[test]
    fn a_missing_nested_positional_names_the_deepest_command() {
        // Arrange
        let rows: [(&[&str], &[&str], &str); 2] = [
            (&["playbook", "config", "get"], &["config", "get"], "KEY"),
            (
                &["playbook", "config", "set", "KEY"],
                &["config", "set"],
                "VALUE",
            ),
        ];
        for (args, path, missing) in rows {
            // Act
            let exit = parse_err(args);

            // Assert
            assert_eq!(exit.code, 2, "{args:?}");
            assert!(exit.stderr.starts_with(&long_help(path)), "{args:?}");
            assert_names_missing(&exit.stderr, missing);
        }
    }

    #[test]
    fn a_missing_required_flag_prints_that_commands_help() {
        // Arrange
        let args = ["playbook", "pr", "reply"];

        // Act
        let exit = parse_err(&args);

        // Assert
        assert_eq!(exit.code, 2);
        assert!(exit.stderr.starts_with(&long_help(&["pr", "reply"])));
        assert_names_missing(&exit.stderr, "--pr <PR>");
    }

    #[test]
    fn a_flag_before_the_missing_positional_does_not_hide_the_command() {
        // Arrange
        let args = ["playbook", "route", "--tier", "high"];

        // Act
        let exit = parse_err(&args);

        // Assert
        assert_eq!(exit.code, 2);
        assert!(exit.stderr.starts_with(&long_help(&["route"])));
        assert_names_missing(&exit.stderr, "<KIND>");
    }

    #[test]
    fn a_bare_group_still_prints_its_help_and_exits_two() {
        // Arrange
        for group in ["plan", "config"] {
            // Act
            let exit = parse_err(&["playbook", group]);

            // Assert
            assert_eq!(exit.code, 2, "{group}");
            assert!(exit.stderr.starts_with(&long_help(&[group])), "{group}");
        }
    }

    #[test]
    fn commands_with_a_bare_form_still_parse_without_arguments() {
        // Arrange
        type Check = fn(&Command) -> bool;
        let rows: [(&str, Check); 4] = [
            (
                "cc",
                |c| matches!(c, Command::Cc { sub: None, args } if args.is_empty()),
            ),
            ("effort", |c| {
                matches!(
                    c,
                    Command::Effort {
                        sub: None,
                        level: None,
                        json: false
                    }
                )
            }),
            ("usage", |c| {
                matches!(
                    c,
                    Command::Usage {
                        sub: None,
                        json: false,
                        summary: false,
                        range: None,
                        account: None,
                        utc: false,
                        local: false,
                    }
                )
            }),
            ("sessions", |c| {
                matches!(
                    c,
                    Command::Sessions {
                        sub: None,
                        json: false
                    }
                )
            }),
        ];
        for (word, is_bare_form) in rows {
            // Act
            let cli = parse(&argv(&["playbook", word])).expect("a bare form parses");

            // Assert
            assert!(is_bare_form(&cli.command), "{word}: {:?}", cli.command);
        }
    }

    #[test]
    fn an_unrelated_clap_error_is_left_to_clap() {
        // Arrange
        let rows: [&[&str]; 2] = [&["playbook", "trust", "a", "b"], &["playbook", "nope"]];
        for args in rows {
            let clap_text = Cli::try_parse_from(args)
                .expect_err("clap rejects it")
                .to_string();

            // Act
            let exit = parse_err(args);

            // Assert
            assert_eq!(exit.code, 2, "{args:?}");
            assert_eq!(exit.stderr, clap_text, "{args:?}");
        }
    }

    #[test]
    fn parse_rewrites_an_old_path_from_the_table() {
        // Arrange
        let table = [Rename {
            old: "oldtrust",
            new: "trust",
            extra: &[],
        }];

        // Act
        let (cli, notice) = parse_renamed(&argv(&["playbook", "oldtrust", "/x"]), &table)
            .ok()
            .expect("the old path parses");

        // Assert
        assert!(
            matches!(cli.command, Command::Trust { .. }),
            "{:?}",
            cli.command
        );
        assert!(notice.is_some_and(|n| n.contains("oldtrust")));
    }
}
