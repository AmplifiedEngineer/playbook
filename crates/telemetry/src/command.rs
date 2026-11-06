// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `usage` subcommands, as a clap enum the host program embeds.

use clap::Subcommand;

/// `playbook usage` subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum UsageCommand {
    /// Warm the usage store in the background (run by the `usage-warm` hook)
    #[command(hide = true)]
    Warm,
    /// Removed: use `playbook usage --summary`, the terminal view warms itself
    #[command(hide = true)]
    Ingest,
    /// Run the local OpenTelemetry receiver in the foreground
    ///
    /// Listens on 127.0.0.1 only (default port 47318, chosen once and then kept; a
    /// free port is chosen on the first run when that one is taken; an existing
    /// install keeps the port it recorded) and stores API request cost and token
    /// rows,
    /// tool call timings and API errors from Claude Code's JSON telemetry.
    /// Prompts, tool content, paths, emails and user ids are never stored. A
    /// second start reports the running collector (`playbook usage status`
    /// shows it too). It stops by itself after the idle time, counted from the
    /// last real telemetry request, so a status check does not keep it alive.
    /// Use `--idle-seconds 0` to keep it running in a tmux window. Sessions that sent no
    /// telemetry are read from transcripts as before. While it runs, `cc`
    /// points its sessions at it unless `usage.telemetry` is `off` or you set
    /// telemetry yourself. When the settings that `setup` wrote point at it and
    /// none runs, playbook commands and hooks start one with `--idle-seconds 0`
    /// (see `docs/internals/04-usage-dashboard.md`).
    ///
    /// Example: `playbook usage collect --idle-seconds 1800`
    Collect {
        /// Port to listen on. It is an error when the port is busy. Defaults to the recorded port, chosen once (47318, or a free one when that is taken) and kept. A port given here also replaces the recorded one; run `playbook setup --only telemetry` afterwards (older versions: `playbook init --only telemetry`).
        #[arg(long)]
        port: Option<u16>,
        /// Stop after this many seconds with no request; 0 means never
        #[arg(long, default_value_t = 600)]
        idle_seconds: u64,
    },
    /// Show whether the telemetry receiver is running
    ///
    /// Prints how telemetry is configured (on with the collector running, off because you
    /// disabled it in your Claude Code settings, or off because nothing sets it) with a one-line
    /// fix, then the pid, port, idle limit, how many telemetry rows are stored
    /// (API requests, tool calls, API errors, metric points), the retention limits, and the time of the newest
    /// event. Like most playbook commands it can start the collector first. It never prints the token. With `--json` it
    /// prints the same as JSON. Exit code 0 whether or not a receiver runs.
    ///
    /// Example: `playbook usage status --json`
    Status {
        /// Print the status as JSON
        #[arg(long)]
        json: bool,
    },
    /// Stop the running telemetry collector
    ///
    /// Asks the collector that holds the lock to stop through its
    /// authenticated shutdown route and waits up to 3 seconds for it to let
    /// go. One that does not answer is killed, but only after checking that
    /// the process is really the playbook collector. Prints `stopped`, or
    /// `not running` with exit code 0 when no collector runs. The next
    /// playbook command or session starts a new one. A collector from
    /// 0.38.0 or older holds no lock, so this does not see it.
    ///
    /// Example: `playbook usage stop`
    Stop,
    /// Show where pbtop can read your usage limits on this machine
    ///
    /// Lists which sources exist: the plan fields and cached usage numbers
    /// in the Claude Code config file (`~/.claude.json`), the statusline
    /// reading and the manual `usage.budget.*` keys. It prints field names,
    /// ages, the plan label, the window percentages and the manual budget
    /// amounts, and never an email, name, id, organization or token. Run it
    /// on a machine where the limits look wrong and share the output. Add
    /// `--raw` to also print the values of six plan fields (billing type,
    /// organization type, seat tier, the two rate limit tiers and the extra
    /// usage flag) and the numbers behind each choice: the cache fetch time as
    /// written, the type of each cache field, and the monthly amount used
    /// against the reported limit and your budget, in dollars. It also says
    /// what the statusline budget line will show. It only reads files and
    /// creates nothing.
    ///
    /// Example: `playbook usage doctor --raw`
    Doctor {
        /// Also print the plan field values and the cached spend limit
        #[arg(long)]
        raw: bool,
    },
    /// Delete old telemetry rows
    ///
    /// Removes telemetry rows older than `usage.telemetryRetentionDays` (default
    /// 90, 0 keeps them) and the oldest rows beyond `usage.telemetryMaxRows` per
    /// table (default 500000, 0 means no limit). The receiver does the same when
    /// it starts and every few hours. Transcript data and every total are left
    /// alone, because totals come from transcripts.
    ///
    /// Example: `playbook usage prune --dry-run`
    Prune {
        /// Say what would be removed without deleting anything
        #[arg(long)]
        dry_run: bool,
    },
    /// List the accounts in the usage store, name them and move events
    ///
    /// Every usage event carries an account key: the email Claude Code was
    /// signed in to, or a `token:` fingerprint. These commands show the keys,
    /// give each one a display name and relabel events recorded under the wrong account.
    ///
    /// Example: `playbook usage account list`
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
}

/// `playbook usage account` subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum AccountCommand {
    /// List the accounts with their events, cost and time span
    List,
    /// Give an account key a display name, or clear it with an empty name
    ///
    /// Example: `playbook usage account name you@example.com Company`
    Name {
        /// Account key shown by `account list`
        key: String,
        /// Display name to show; empty clears it
        name: String,
    },
    /// Move the events of a time range to another account
    ///
    /// Prints how many events and how much cost would move and changes
    /// nothing until you add `--confirm`. Times are read on your local clock;
    /// add `--utc` for UTC. The end is exclusive. An applied move is
    /// numbered and `account undo` reverses it. Running the same move again
    /// moves nothing.
    ///
    /// Example: `playbook usage account assign --from 2026-11-01T00:00 --to 2026-11-02T12:00 --as personal`
    Assign {
        /// First instant to move, like 2026-11-01T00:00 (a bare date means midnight)
        #[arg(long)]
        from: String,
        /// First instant not to move, like 2026-11-02T12:00
        #[arg(long)]
        to: String,
        /// Account key or name that receives the events
        #[arg(long = "as", value_name = "KEY_OR_NAME")]
        target: String,
        /// Move only events now labeled with this account key or name
        #[arg(long, value_name = "KEY_OR_NAME")]
        was: Option<String>,
        /// Apply the move; without it nothing changes
        #[arg(long)]
        confirm: bool,
    },
    /// Reverse the newest applied move, or the one with this number
    Undo {
        /// Move number printed by `account assign`
        id: Option<i64>,
    },
}
