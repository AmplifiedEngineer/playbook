// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `pbsync` command line. Commands arrive later; for now it answers
//! `--version` and `--help`.

use std::process::ExitCode;

const EXIT_USAGE: u8 = 2;
const FLAG_VERSION: &str = "--version";
const FLAG_HELP: &str = "--help";
const USAGE: &str = "usage: pbsync [--version | --help]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == FLAG_VERSION => {
            println!("pbsync {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [flag] if flag == FLAG_HELP => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("error: unknown arguments\n{USAGE}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}
