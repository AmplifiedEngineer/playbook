// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The command line of `pbtop`, the standalone Agent Budget Dashboard (ADR 0041).
use crate::command::UsageCommand;
use crate::entry::{interactive, run, Opts};
use crate::run::Paths;
use clap::Parser;

/// Show token and cost usage across your Claude Code sessions
///
/// With no subcommand, opens the terminal view, which opens on this week
/// compared with last week. When stdout is not a terminal, or with
/// `--summary`, prints a text summary of estimated cost instead. `--json`
/// prints the same numbers as JSON for scripts. Times and days use your local
/// time zone, shown in the output; `--utc` switches to UTC, or set `usage.timeZone`
/// or `PLAYBOOK_TELEMETRY_TZ` to `local` or `utc`. Ranges: today,
/// week, last-week, month, last-month, 30d, 60d, 90d, all.
///
/// Example: `pbtop --range today`. With playbook installed, `playbook usage`
/// runs the same code.
#[derive(Parser, Debug)]
#[command(name = "pbtop", bin_name = "pbtop", version)]
struct Cli {
    #[command(subcommand)]
    sub: Option<UsageCommand>,
    /// Print the totals, groupings and live view as JSON
    #[arg(long, conflicts_with_all = ["summary"])]
    json: bool,
    /// Print the text summary and exit (every recorded session unless `--range` is given)
    #[arg(long, conflicts_with_all = ["json"])]
    summary: bool,
    /// Date range, counted in days of the chosen time zone. One of today, week, last-week, month, last-month, 30d, 60d, 90d or all. Default week for the terminal view, 30d for `--json`, all for `--summary`
    #[arg(long, value_name = "RANGE")]
    range: Option<String>,
    /// Show times and group days in UTC. Overrides `usage.timeZone` and `PLAYBOOK_TELEMETRY_TZ`
    #[arg(long, global = true, conflicts_with = "local")]
    utc: bool,
    /// Show times and group days in your local time zone. This is the default. Overrides `usage.timeZone` and `PLAYBOOK_TELEMETRY_TZ`
    #[arg(long, global = true, conflicts_with = "utc")]
    local: bool,
}

/// The entry point of `pbtop`.
pub fn main() {
    let cli = Cli::parse();
    let flag = match (cli.utc, cli.local) {
        (true, _) => Some(crate::tz::Zone::Utc),
        (_, true) => Some(crate::tz::Zone::Local),
        _ => None,
    };
    crate::tz::init(flag);
    let paths = Paths::real();
    let opts = Opts {
        json: cli.json,
        summary: cli.summary,
        range: cli.range,
        account: None,
    };
    match run(cli.sub, &opts, &paths, interactive()) {
        Ok(output) => {
            if !output.is_empty() {
                println!("{}", output.trim_end());
            }
        }
        Err(err) => {
            eprintln!("pbtop: {err}");
            std::process::exit(1);
        }
    }
}
