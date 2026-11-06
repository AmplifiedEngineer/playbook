// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Storage for OpenTelemetry usage rows. These tables sit next to the
//! transcript tables and never touch them: the transcript ingest, its tables
//! and its totals are unchanged. Reverting the feature means dropping the
//! tables (`migrate_down`), which loses no transcript data.
//!
//! Rows hold only numbers and short names (session id, request id, model,
//! effort, agent name). Prompts, tool content, paths, emails and user ids are
//! never stored; the receiver strips them before a row reaches this module.

use rusqlite::{params, Connection, OptionalExtension};

pub(super) const UP_SQL: &str = "
CREATE TABLE IF NOT EXISTS telemetry_requests (
    session_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL DEFAULT '',
    agent_name TEXT NOT NULL DEFAULT '',
    skill_name TEXT NOT NULL DEFAULT '',
    duration_ms INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL CHECK(input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK(output_tokens >= 0),
    cache_creation_tokens INTEGER NOT NULL CHECK(cache_creation_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL CHECK(cache_read_tokens >= 0),
    cost_usd REAL NOT NULL CHECK(cost_usd >= 0),
    PRIMARY KEY (session_id, request_id)
);
CREATE INDEX IF NOT EXISTS telemetry_requests_by_time
    ON telemetry_requests (timestamp, session_id, request_id);
CREATE TABLE IF NOT EXISTS session_sources (
    session_id TEXT PRIMARY KEY,
    source TEXT NOT NULL CHECK(source IN ('telemetry','transcript')),
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS session_reconcile (
    session_id TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK(state IN ('complete','partial','disagree')),
    telemetry_tokens INTEGER NOT NULL,
    transcript_tokens INTEGER NOT NULL,
    telemetry_cost REAL NOT NULL,
    transcript_cost REAL NOT NULL,
    note TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS telemetry_tools (
    session_id TEXT NOT NULL,
    tool_use_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    tool_name TEXT NOT NULL,
    success INTEGER CHECK(success IN (0, 1)),
    duration_ms INTEGER CHECK(duration_ms >= 0),
    source TEXT NOT NULL DEFAULT 'otlp' CHECK(source IN ('otlp','transcript')),
    PRIMARY KEY (session_id, tool_use_id)
);
CREATE INDEX IF NOT EXISTS telemetry_tools_by_time ON telemetry_tools (timestamp);
CREATE INDEX IF NOT EXISTS telemetry_tools_by_use ON telemetry_tools (tool_use_id);
CREATE TABLE IF NOT EXISTS telemetry_errors (
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    model TEXT NOT NULL,
    status_code INTEGER NOT NULL CHECK(status_code >= 0),
    attempt INTEGER NOT NULL CHECK(attempt >= 0),
    duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
    source TEXT NOT NULL DEFAULT 'otlp' CHECK(source IN ('otlp','transcript')),
    PRIMARY KEY (session_id, sequence)
);
CREATE INDEX IF NOT EXISTS telemetry_errors_by_time ON telemetry_errors (timestamp);
CREATE TABLE IF NOT EXISTS telemetry_metrics (
    session_id TEXT NOT NULL,
    metric TEXT NOT NULL,
    kind TEXT NOT NULL,
    start_ns INTEGER NOT NULL,
    end_ns INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    value REAL NOT NULL CHECK(value >= 0),
    PRIMARY KEY (session_id, metric, kind, start_ns, end_ns)
);
CREATE INDEX IF NOT EXISTS telemetry_metrics_by_time ON telemetry_metrics (timestamp);";

const DOWN_SQL: &str = "
DROP INDEX IF EXISTS telemetry_requests_by_time;
DROP TABLE IF EXISTS telemetry_requests;
DROP TABLE IF EXISTS session_sources;
DROP TABLE IF EXISTS session_reconcile;
DROP INDEX IF EXISTS telemetry_tools_by_time;
DROP INDEX IF EXISTS telemetry_tools_by_use;
DROP TABLE IF EXISTS telemetry_tools;
DROP INDEX IF EXISTS telemetry_errors_by_time;
DROP TABLE IF EXISTS telemetry_errors;
DROP INDEX IF EXISTS telemetry_metrics_by_time;
DROP TABLE IF EXISTS telemetry_metrics;";

/// Where a session's numbers come from. A session with no recorded source is
/// read from transcripts, so history before telemetry needs no backfill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Telemetry,
    Transcript,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Telemetry => "telemetry",
            Source::Transcript => "transcript",
        }
    }

    fn parse(text: &str) -> Source {
        if text == "telemetry" {
            Source::Telemetry
        } else {
            Source::Transcript
        }
    }
}

/// One API request as reported by telemetry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryRequest {
    pub session_id: String,
    pub request_id: String,
    pub timestamp: i64,
    pub model: String,
    pub effort: String,
    pub agent_name: String,
    /// The skill active for the request, empty when none.
    pub skill_name: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    /// How long the API call took, in milliseconds.
    pub duration_ms: u64,
}

/// One tool call as reported by telemetry. Only the tool name, the outcome
/// and the time are kept, never the input or the result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryTool {
    pub session_id: String,
    pub tool_use_id: String,
    pub timestamp: i64,
    pub tool_name: String,
    pub success: bool,
    pub duration_ms: u64,
}

/// One failed API attempt. The error text is never read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryError {
    pub session_id: String,
    pub sequence: u64,
    pub timestamp: i64,
    pub model: String,
    pub status_code: u64,
    /// 1 for the first try; 2 or more means a retry was already made.
    pub attempt: u64,
    pub duration_ms: u64,
}

/// One delta data point of a counter metric. The key is the session, the
/// metric, its kind and the exact interval, so a redelivered point is a no-op.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TelemetryMetric {
    pub session_id: String,
    /// `active_time` (seconds), `lines_of_code`, `commits` or `pull_requests`.
    pub metric: String,
    /// The `type` attribute (`user`, `cli`, `added`, `removed`), or empty.
    pub kind: String,
    pub start_ns: i64,
    pub end_ns: i64,
    pub timestamp: i64,
    pub value: f64,
}

/// Creates the telemetry tables. Safe to run any number of times.
pub fn migrate_up(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(UP_SQL)
        .map_err(|e| format!("failed to create telemetry tables: {e}"))?;
    ensure_columns(conn)
}

/// Tables made before a column existed lack it.
pub(super) fn ensure_columns(conn: &Connection) -> Result<(), String> {
    ensure_column(conn, "skill_name", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "duration_ms", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_source_columns(conn)
}

const SOURCE_DDL: &str = "TEXT NOT NULL DEFAULT 'otlp' CHECK(source IN ('otlp','transcript'))";

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
        params![table, column],
        |r| r.get(0),
    )
    .map_err(|e| format!("failed to inspect {table}: {e}"))
}

/// Adds `source` to the tool and error tables (ADR 0016: an idempotent,
/// self-healing step that runs on every open). Every row that exists becomes
/// `otlp`. Transcript rows have no outcome and no duration, so the tool table
/// also drops NOT NULL on `success` and `duration_ms`. SQLite cannot do that
/// in place, so the table is rebuilt in one transaction. The check runs again
/// under the write lock, so two openers rebuild it once.
fn ensure_source_columns(conn: &Connection) -> Result<(), String> {
    if !has_column(conn, "telemetry_errors", "source")? {
        let sql = format!("ALTER TABLE telemetry_errors ADD COLUMN source {SOURCE_DDL}");
        if let Err(e) = conn.execute_batch(&sql) {
            // A concurrent opener may have added it first.
            if !has_column(conn, "telemetry_errors", "source")? {
                return Err(format!("failed to add telemetry_errors.source: {e}"));
            }
        }
    }
    if has_column(conn, "telemetry_tools", "source")? {
        return Ok(());
    }
    let fail = |e: rusqlite::Error| format!("failed to migrate telemetry_tools: {e}");
    conn.execute_batch("BEGIN IMMEDIATE").map_err(fail)?;
    let rebuilt = (|| -> Result<(), rusqlite::Error> {
        let again: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('telemetry_tools') WHERE name = 'source')",
            [],
            |r| r.get(0),
        )?;
        if again {
            return Ok(());
        }
        conn.execute_batch(
            "DROP TABLE IF EXISTS telemetry_tools_new;
             CREATE TABLE telemetry_tools_new (
                session_id TEXT NOT NULL,
                tool_use_id TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                success INTEGER CHECK(success IN (0, 1)),
                duration_ms INTEGER CHECK(duration_ms >= 0),
                source TEXT NOT NULL DEFAULT 'otlp' CHECK(source IN ('otlp','transcript')),
                PRIMARY KEY (session_id, tool_use_id)
             );
             INSERT INTO telemetry_tools_new
                (session_id, tool_use_id, timestamp, tool_name, success, duration_ms, source)
                SELECT session_id, tool_use_id, timestamp, tool_name, success, duration_ms, 'otlp'
                FROM telemetry_tools;
             DROP TABLE telemetry_tools;
             ALTER TABLE telemetry_tools_new RENAME TO telemetry_tools;
             CREATE INDEX IF NOT EXISTS telemetry_tools_by_time ON telemetry_tools (timestamp);
             CREATE INDEX IF NOT EXISTS telemetry_tools_by_use ON telemetry_tools (tool_use_id);",
        )
    })();
    match rebuilt {
        Ok(()) => conn.execute_batch("COMMIT").map_err(fail),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(fail(e))
        }
    }
}

fn ensure_column(conn: &Connection, column: &str, ddl: &str) -> Result<(), String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(telemetry_requests)")
        .map_err(|e| format!("failed to inspect telemetry tables: {e}"))?;
    let has = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("failed to inspect telemetry tables: {e}"))?
        .any(|c| c.is_ok_and(|n| n == column));
    if has {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "ALTER TABLE telemetry_requests ADD COLUMN {column} {ddl}"
    ))
    .map_err(|e| format!("failed to add the {column} column: {e}"))
}

/// Drops the telemetry tables and nothing else.
pub fn migrate_down(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(DOWN_SQL)
        .map_err(|e| format!("failed to drop telemetry tables: {e}"))
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Stores a request. The pair (session id, request id) is the key, so a
/// redelivered batch is a no-op. Returns 1 when a row was written, else 0.
pub fn insert_request(conn: &Connection, r: &TelemetryRequest) -> Result<usize, String> {
    let cost = if r.cost_usd.is_finite() && r.cost_usd >= 0.0 {
        r.cost_usd
    } else {
        0.0
    };
    conn.execute(
        "INSERT OR IGNORE INTO telemetry_requests (session_id, request_id, timestamp, model,
            effort, agent_name, skill_name, input_tokens, output_tokens,
            cache_creation_tokens, cache_read_tokens, cost_usd, duration_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            r.session_id,
            r.request_id,
            r.timestamp,
            r.model,
            r.effort,
            r.agent_name,
            r.skill_name,
            to_i64(r.input_tokens),
            to_i64(r.output_tokens),
            to_i64(r.cache_creation_tokens),
            to_i64(r.cache_read_tokens),
            cost,
            to_i64(r.duration_ms)
        ],
    )
    .map_err(|e| format!("failed to insert telemetry request {}: {e}", r.request_id))
}

/// Stores a tool call. Keyed by session and tool use id, so a redelivered
/// batch is a no-op.
pub fn insert_tool(conn: &Connection, t: &TelemetryTool) -> Result<usize, String> {
    // A live row replaces a transcript row of the same call, never the reverse.
    conn.execute(
        "DELETE FROM telemetry_tools WHERE tool_use_id = ?1 AND source = 'transcript'",
        [&t.tool_use_id],
    )
    .map_err(|e| format!("failed to replace transcript tool {}: {e}", t.tool_use_id))?;
    conn.execute(
        "INSERT OR IGNORE INTO telemetry_tools
            (session_id, tool_use_id, timestamp, tool_name, success, duration_ms, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'otlp')",
        params![
            t.session_id,
            t.tool_use_id,
            t.timestamp,
            t.tool_name,
            i64::from(t.success),
            to_i64(t.duration_ms)
        ],
    )
    .map_err(|e| format!("failed to insert telemetry tool {}: {e}", t.tool_use_id))
}

/// Stores a failed API attempt. Keyed by session and event sequence.
pub fn insert_metric(conn: &Connection, m: &TelemetryMetric) -> Result<usize, String> {
    conn.execute(
        "INSERT OR IGNORE INTO telemetry_metrics
         (session_id, metric, kind, start_ns, end_ns, timestamp, value)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            m.session_id,
            m.metric,
            m.kind,
            m.start_ns,
            m.end_ns,
            m.timestamp,
            if m.value.is_finite() && m.value >= 0.0 {
                m.value
            } else {
                0.0
            }
        ],
    )
    .map_err(|e| format!("failed to store a telemetry metric: {e}"))
}

pub fn insert_error(conn: &Connection, e: &TelemetryError) -> Result<usize, String> {
    conn.execute(
        "INSERT OR IGNORE INTO telemetry_errors
            (session_id, sequence, timestamp, model, status_code, attempt, duration_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            e.session_id,
            to_i64(e.sequence),
            e.timestamp,
            e.model,
            to_i64(e.status_code),
            to_i64(e.attempt),
            to_i64(e.duration_ms)
        ],
    )
    .map_err(|err| format!("failed to insert telemetry error: {err}"))
}

/// Rows written per statement while the transcript backfill runs.
const BACKFILL_CHUNK: i64 = 2_000;

/// Copies tool calls from `tool_invocation_events` (what the transcripts say)
/// into `telemetry_tools` as `transcript` rows, newest first, for calls newer
/// than the retention window. A call that has any row already, live or from an
/// earlier run, is skipped by its tool use id, so a live row is never
/// overwritten or doubled and a second run changes nothing. Transcripts carry
/// no outcome and no duration, so both are NULL. Stops after `budget`; the
/// next run continues. Returns the rows written.
pub fn backfill_transcript_tools(
    conn: &Connection,
    now: i64,
    retention: Retention,
    budget: std::time::Duration,
) -> Result<usize, String> {
    let cutoff = if retention.days > 0 {
        now - to_i64(retention.days).saturating_mul(86_400)
    } else {
        i64::MIN
    };
    let started = std::time::Instant::now();
    let mut total = 0;
    loop {
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO telemetry_tools
                    (session_id, tool_use_id, timestamp, tool_name, success, duration_ms, source)
                 SELECT e.session_id, e.event_id, e.timestamp,
                        CASE e.kind WHEN 'agent' THEN 'Agent' ELSE 'Skill' END,
                        NULL, NULL, 'transcript'
                 FROM tool_invocation_events e
                 WHERE e.timestamp >= ?1
                   AND NOT EXISTS (SELECT 1 FROM telemetry_tools t WHERE t.tool_use_id = e.event_id)
                 ORDER BY e.timestamp DESC LIMIT ?2",
                params![cutoff, BACKFILL_CHUNK],
            )
            .map_err(|e| format!("failed to backfill tool calls from transcripts: {e}"))?;
        total += n;
        if n < BACKFILL_CHUNK as usize || started.elapsed() >= budget {
            return Ok(total);
        }
    }
}

/// When Claude Code logged an HTTP 429 in `[since, until)`, oldest first. One
/// entry per `(session, sequence)`, the key of the table, so one failed attempt
/// counts once however often its row is sent again.
pub fn rate_limited_at(conn: &Connection, since: i64, until: i64) -> Result<Vec<i64>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT timestamp FROM telemetry_errors
             WHERE status_code = 429 AND timestamp >= ?1 AND timestamp < ?2
             ORDER BY timestamp",
        )
        .map_err(|e| format!("failed to read rate limit errors: {e}"))?;
    let rows = stmt
        .query_map(params![since, until], |r| r.get::<_, i64>(0))
        .map_err(|e| format!("failed to read rate limit errors: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read rate limit errors: {e}"))
}

/// Tool calls of one tool in a window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolStat {
    pub name: String,
    pub calls: u64,
    pub failures: u64,
    pub total_ms: u64,
    pub max_ms: u64,
}

impl ToolStat {
    /// Failures over calls, 0 to 1.
    pub fn error_rate(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.failures as f64 / self.calls as f64
        }
    }

    pub fn avg_ms(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.total_ms as f64 / self.calls as f64
        }
    }
}

/// The tools used in `[since, until)`, busiest first, then by name.
pub fn tool_stats(conn: &Connection, since: i64, until: i64) -> Result<Vec<ToolStat>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT CASE WHEN tool_name = '' THEN 'unknown' ELSE tool_name END AS n,
                    COUNT(*), SUM(1 - success), SUM(duration_ms), MAX(duration_ms)
             FROM telemetry_tools
             WHERE source = 'otlp' AND timestamp >= ?1 AND timestamp < ?2
             GROUP BY n ORDER BY COUNT(*) DESC, n",
        )
        .map_err(|e| format!("failed to read telemetry tools: {e}"))?;
    let rows = stmt
        .query_map(params![since, until], |r| {
            let n = |i: usize| -> rusqlite::Result<u64> {
                Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
            };
            Ok(ToolStat {
                name: r.get(0)?,
                calls: n(1)?,
                failures: n(2)?,
                total_ms: n(3)?,
                max_ms: n(4)?,
            })
        })
        .map_err(|e| format!("failed to read telemetry tools: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to read telemetry tools: {e}"))
}

/// The health figures of a window: API calls and their failures, tool calls,
/// and the time spent waiting on the API and on tools.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub requests: u64,
    pub api_ms: u64,
    pub errors: u64,
    /// Failed attempts that came after at least one earlier try.
    pub retried: u64,
    pub tool_calls: u64,
    pub tool_failures: u64,
    pub tool_ms: u64,
    /// Claude Code's own `active_time` metric, user and cli, in milliseconds.
    pub active_ms: u64,
    pub lines_changed: u64,
    pub commits: u64,
    pub pull_requests: u64,
}

impl Health {
    /// Whether the window has any telemetry row of any kind.
    pub fn is_empty(&self) -> bool {
        self.requests == 0
            && self.errors == 0
            && self.tool_calls == 0
            && self.active_ms == 0
            && self.lines_changed == 0
            && self.commits == 0
            && self.pull_requests == 0
    }

    /// Time waiting on the API or on tools. This is a floor for the time
    /// Claude Code was working, not its own active time metric.
    pub fn busy_ms(&self) -> u64 {
        self.api_ms + self.tool_ms
    }

    /// Failed attempts over all attempts (requests that finished plus
    /// failures), 0 to 1.
    pub fn api_error_rate(&self) -> f64 {
        ratio(self.errors, self.requests + self.errors)
    }

    pub fn tool_error_rate(&self) -> f64 {
        ratio(self.tool_failures, self.tool_calls)
    }
}

fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// The figures of `[since, until)`.
pub fn health(conn: &Connection, since: i64, until: i64) -> Result<Health, String> {
    let one = |sql: &str| -> Result<Vec<u64>, String> {
        conn.query_row(sql, params![since, until], |r| {
            (0..r.as_ref().column_count())
                .map(|i| Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0)))
                .collect()
        })
        .map_err(|e| format!("failed to read telemetry health: {e}"))
    };
    let req = one(
        "SELECT COUNT(*), COALESCE(SUM(duration_ms), 0) FROM telemetry_requests
                   WHERE timestamp >= ?1 AND timestamp < ?2",
    )?;
    let err = one(
        "SELECT COUNT(*), COALESCE(SUM(attempt > 1), 0) FROM telemetry_errors
                   WHERE timestamp >= ?1 AND timestamp < ?2",
    )?;
    let tool = one(
        "SELECT COUNT(*), COALESCE(SUM(1 - success), 0), COALESCE(SUM(duration_ms), 0)
                    FROM telemetry_tools
                    WHERE source = 'otlp' AND timestamp >= ?1 AND timestamp < ?2",
    )?;
    Ok(Health {
        requests: req[0],
        api_ms: req[1],
        errors: err[0],
        retried: err[1],
        tool_calls: tool[0],
        tool_failures: tool[1],
        tool_ms: tool[2],
        active_ms: metric_sum(conn, "active_time", since, until)?.saturating_mul(1000),
        lines_changed: metric_sum(conn, "lines_of_code", since, until)?,
        commits: metric_sum(conn, "commits", since, until)?,
        pull_requests: metric_sum(conn, "pull_requests", since, until)?,
    })
}

/// Sum of one metric in `[since, until)`, over all kinds.
fn metric_sum(conn: &Connection, metric: &str, since: i64, until: i64) -> Result<u64, String> {
    let v: f64 = conn
        .query_row(
            "SELECT COALESCE(SUM(value), 0) FROM telemetry_metrics
             WHERE metric = ?1 AND timestamp >= ?2 AND timestamp < ?3",
            params![metric, since, until],
            |r| r.get(0),
        )
        .map_err(|e| format!("failed to read telemetry metrics: {e}"))?;
    Ok(v.round() as u64)
}

/// Defaults of the two telemetry size limits (config keys
/// `usage.telemetryRetentionDays` and `usage.telemetryMaxRows`).
pub const DEFAULT_RETENTION_DAYS: u64 = 90;
pub const DEFAULT_MAX_ROWS: u64 = 500_000;

const PRUNED_TABLES: [&str; 4] = [
    "telemetry_requests",
    "telemetry_tools",
    "telemetry_errors",
    "telemetry_metrics",
];

/// How long and how much telemetry is kept. 0 means no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Retention {
    /// Rows older than this many days are deleted.
    pub days: u64,
    /// At most this many rows are kept per table, the newest ones.
    pub max_rows: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            days: DEFAULT_RETENTION_DAYS,
            max_rows: DEFAULT_MAX_ROWS,
        }
    }
}

/// Rows removed (or, in a dry run, that would be) per table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Pruned {
    pub requests: u64,
    pub tools: u64,
    pub errors: u64,
    pub metrics: u64,
}

impl Pruned {
    pub fn total(&self) -> u64 {
        self.requests + self.tools + self.errors + self.metrics
    }
}

/// Deletes telemetry rows past the limits: first those older than
/// `retention.days`, then the oldest rows beyond `retention.max_rows` in each
/// table. Transcript tables are never touched, and totals come from them, so
/// no total changes. With `dry_run` nothing is deleted and the counts say what
/// would be.
pub fn prune(
    conn: &Connection,
    now: i64,
    retention: Retention,
    dry_run: bool,
) -> Result<Pruned, String> {
    let cutoff = (retention.days > 0).then(|| now - to_i64(retention.days).saturating_mul(86_400));
    let mut out = [0u64; 4];
    for (slot, table) in out.iter_mut().zip(PRUNED_TABLES) {
        let fail = |e: rusqlite::Error| format!("failed to prune {table}: {e}");
        let total: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .map_err(fail)?;
        let old: i64 = match cutoff {
            Some(c) => conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE timestamp < ?1"),
                    params![c],
                    |r| r.get(0),
                )
                .map_err(fail)?,
            None => 0,
        };
        let excess = if retention.max_rows > 0 {
            (total - old - to_i64(retention.max_rows)).max(0)
        } else {
            0
        };
        if !dry_run {
            if let Some(c) = cutoff {
                conn.execute(
                    &format!("DELETE FROM {table} WHERE timestamp < ?1"),
                    params![c],
                )
                .map_err(fail)?;
            }
            if excess > 0 {
                conn.execute(
                    &format!(
                        "DELETE FROM {table} WHERE rowid IN
                         (SELECT rowid FROM {table} ORDER BY timestamp, rowid LIMIT ?1)"
                    ),
                    params![excess],
                )
                .map_err(fail)?;
            }
        }
        *slot = u64::try_from(old + excess).unwrap_or(0);
    }
    Ok(Pruned {
        requests: out[0],
        tools: out[1],
        errors: out[2],
        metrics: out[3],
    })
}

pub fn count_requests(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM telemetry_requests", [], |r| r.get(0))
        .map_err(|e| format!("failed to count telemetry requests: {e}"))
}

/// Records which source a session uses.
pub fn set_session_source(
    conn: &Connection,
    session_id: &str,
    source: Source,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO session_sources (session_id, source, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET source = excluded.source,
            updated_at = excluded.updated_at",
        params![session_id, source.as_str(), now],
    )
    .map(|_| ())
    .map_err(|e| format!("failed to set the source of {session_id}: {e}"))
}

/// The source of a session; transcripts when none was recorded.
pub fn session_source(conn: &Connection, session_id: &str) -> Result<Source, String> {
    conn.query_row(
        "SELECT source FROM session_sources WHERE session_id = ?1",
        [session_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map(|found| found.map_or(Source::Transcript, |s| Source::parse(&s)))
    .map_err(|e| format!("failed to read the source of {session_id}: {e}"))
}

/// Telemetry requests in `[since, until)` grouped by agent name, sorted by
/// name. The main session (no agent name) is `main`.
pub fn agent_groups(
    conn: &Connection,
    since: i64,
    until: i64,
) -> Result<Vec<super::aggregate::Group>, String> {
    groups(conn, "agent_name", "main", since, until)
}

/// Telemetry requests in `[since, until)` grouped by the skill that was
/// active, sorted by name. Requests made with no skill active are left out.
pub fn skill_groups(
    conn: &Connection,
    since: i64,
    until: i64,
) -> Result<Vec<super::aggregate::Group>, String> {
    groups(conn, "skill_name", "", since, until)
}

/// Agent or skill invocations in `[since, until)` counted from transcripts,
/// sorted by name. `messages` holds the count; tokens and cost stay zero.
pub fn invocation_groups(
    conn: &Connection,
    kind: crate::ToolKind,
    since: i64,
    until: i64,
) -> Result<Vec<super::aggregate::Group>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT name, COUNT(*) FROM tool_invocation_events
             WHERE kind = ?1 AND timestamp >= ?2 AND timestamp < ?3
             GROUP BY name ORDER BY name",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map((kind.as_str(), since, until), |r| {
            Ok(super::aggregate::Group {
                key: r.get(0)?,
                messages: r.get::<_, i64>(1)?.max(0) as u64,
                ..Default::default()
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// `column` is one of two fixed names, never user input.
fn groups(
    conn: &Connection,
    column: &str,
    empty_as: &str,
    since: i64,
    until: i64,
) -> Result<Vec<super::aggregate::Group>, String> {
    let filter = if empty_as.is_empty() {
        format!("AND {column} <> ''")
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT CASE WHEN {column} = '' THEN ?3 ELSE {column} END AS k,
                COUNT(*), SUM(input_tokens), SUM(output_tokens),
                SUM(cache_creation_tokens), SUM(cache_read_tokens), SUM(cost_usd)
         FROM telemetry_requests
         WHERE timestamp >= ?1 AND timestamp < ?2 {filter}
         GROUP BY k ORDER BY k"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("failed to read telemetry groups: {e}"))?;
    let rows = stmt
        .query_map(params![since, until, empty_as], |r| {
            let n = |i: usize| -> rusqlite::Result<u64> {
                Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
            };
            Ok(super::aggregate::Group {
                key: r.get(0)?,
                messages: n(1)?,
                input_tokens: n(2)?,
                output_tokens: n(3)?,
                cache_creation_tokens: n(4)?,
                cache_read_tokens: n(5)?,
                cost_usd: r.get(6)?,
                unpriced: 0,
            })
        })
        .map_err(|e| format!("failed to read telemetry groups: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to read telemetry groups: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{count_usage_events, open_db, upsert_usage_event};
    use crate::UsageEvent;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate_up(&conn).unwrap();
        conn
    }

    fn request(id: &str) -> TelemetryRequest {
        TelemetryRequest {
            session_id: "s1".into(),
            request_id: id.into(),
            timestamp: 100,
            model: "opus".into(),
            agent_name: "reviewer".into(),
            input_tokens: 10,
            output_tokens: 5,
            cost_usd: 0.25,
            ..TelemetryRequest::default()
        }
    }

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [name],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn inserting_the_same_request_twice_stores_one_row() {
        let conn = mem();
        assert_eq!(insert_request(&conn, &request("r1")).unwrap(), 1);
        assert_eq!(insert_request(&conn, &request("r1")).unwrap(), 0);
        assert_eq!(count_requests(&conn).unwrap(), 1);
    }

    #[test]
    fn the_same_request_id_in_two_sessions_is_two_rows() {
        let conn = mem();
        let mut other = request("r1");
        other.session_id = "s2".into();
        insert_request(&conn, &request("r1")).unwrap();
        insert_request(&conn, &other).unwrap();
        assert_eq!(count_requests(&conn).unwrap(), 2);
    }

    #[test]
    fn a_bad_cost_is_stored_as_zero() {
        let conn = mem();
        let mut r = request("r1");
        r.cost_usd = f64::NAN;
        insert_request(&conn, &r).unwrap();
        let cost: f64 = conn
            .query_row("SELECT cost_usd FROM telemetry_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn a_session_without_a_record_reads_from_transcripts() {
        let conn = mem();
        assert_eq!(session_source(&conn, "none").unwrap(), Source::Transcript);
    }

    #[test]
    fn the_session_source_can_change_and_is_kept_per_session() {
        let conn = mem();
        set_session_source(&conn, "s1", Source::Telemetry, 1).unwrap();
        set_session_source(&conn, "s2", Source::Transcript, 1).unwrap();
        assert_eq!(session_source(&conn, "s1").unwrap(), Source::Telemetry);
        set_session_source(&conn, "s1", Source::Transcript, 2).unwrap();
        assert_eq!(session_source(&conn, "s1").unwrap(), Source::Transcript);
        assert_eq!(session_source(&conn, "s2").unwrap(), Source::Transcript);
    }

    #[test]
    fn the_source_column_rejects_an_unknown_value() {
        let conn = mem();
        let err = conn.execute("INSERT INTO session_sources VALUES ('s', 'guess', 1)", []);
        assert!(err.is_err());
    }

    #[test]
    fn migrate_up_is_idempotent_and_down_removes_only_telemetry_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE usage_events (event_id TEXT);")
            .unwrap();
        migrate_up(&conn).unwrap();
        migrate_up(&conn).unwrap();
        insert_request(&conn, &request("r1")).unwrap();
        assert!(table_exists(&conn, "telemetry_requests"));

        migrate_down(&conn).unwrap();
        migrate_down(&conn).unwrap();

        assert!(!table_exists(&conn, "telemetry_requests"));
        assert!(!table_exists(&conn, "session_sources"));
        assert!(!table_exists(&conn, "session_reconcile"));
        assert!(!table_exists(&conn, "telemetry_tools"));
        assert!(!table_exists(&conn, "telemetry_errors"));
        assert!(table_exists(&conn, "usage_events"));
        migrate_up(&conn).unwrap();
        assert_eq!(count_requests(&conn).unwrap(), 0);
    }

    #[test]
    fn opening_the_usage_db_creates_the_tables_and_leaves_transcript_ingest_alone() {
        let dir = crate::util::scratch_dir("telemetry-open");
        let conn = open_db(&dir.join("usage.db")).unwrap();
        assert!(table_exists(&conn, "telemetry_requests"));
        let event = UsageEvent {
            event_id: "m1".into(),
            session_id: "s1".into(),
            model: "opus".into(),
            input_tokens: 3,
            ..UsageEvent::default()
        };
        upsert_usage_event(&conn, &event).unwrap();
        upsert_usage_event(&conn, &event).unwrap();
        assert_eq!(count_usage_events(&conn).unwrap(), 1);
        assert_eq!(count_requests(&conn).unwrap(), 0);
        assert_eq!(session_source(&conn, "s1").unwrap(), Source::Transcript);
    }
    #[test]
    fn agent_groups_sum_cost_per_agent_inside_the_window_and_name_the_main_session() {
        let conn = mem();
        let mut main = request("r1");
        main.agent_name = String::new();
        main.cost_usd = 1.0;
        let mut sub = request("r2");
        sub.agent_name = "reviewer".into();
        sub.cost_usd = 0.5;
        let mut sub2 = request("r3");
        sub2.agent_name = "reviewer".into();
        sub2.cost_usd = 0.25;
        let mut outside = request("r4");
        outside.timestamp = 10_000;
        for r in [&main, &sub, &sub2, &outside] {
            insert_request(&conn, r).unwrap();
        }
        let groups = agent_groups(&conn, 0, 1_000).unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(names, ["main", "reviewer"]);
        assert!((groups[1].cost_usd - 0.75).abs() < 1e-9);
        assert_eq!(groups[1].messages, 2);
        assert!(agent_groups(&conn, 5_000, 6_000).unwrap().is_empty());
    }

    #[test]
    fn skill_groups_sum_cost_per_skill_and_skip_requests_without_one() {
        let conn = mem();
        let mut a = request("r1");
        a.skill_name = "review".into();
        a.cost_usd = 0.5;
        let mut b = request("r2");
        b.skill_name = "review".into();
        b.cost_usd = 0.25;
        let mut c = request("r3");
        c.skill_name = "plan".into();
        let none = request("r4");
        for r in [&a, &b, &c, &none] {
            insert_request(&conn, r).unwrap();
        }
        let groups = skill_groups(&conn, 0, 1_000).unwrap();
        let names: Vec<&str> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(names, ["plan", "review"]);
        assert!((groups[1].cost_usd - 0.75).abs() < 1e-9);
        assert_eq!(groups[1].messages, 2);
    }

    #[test]
    fn a_table_made_before_skills_gains_the_column_and_keeps_its_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE telemetry_requests (
                session_id TEXT NOT NULL, request_id TEXT NOT NULL, timestamp INTEGER NOT NULL,
                model TEXT NOT NULL, effort TEXT NOT NULL DEFAULT '',
                agent_name TEXT NOT NULL DEFAULT '', input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL, cache_creation_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL, cost_usd REAL NOT NULL,
                PRIMARY KEY (session_id, request_id));
             INSERT INTO telemetry_requests VALUES ('s','r',1,'m','','',1,1,0,0,0.5);",
        )
        .unwrap();
        migrate_up(&conn).unwrap();
        migrate_up(&conn).unwrap();
        assert_eq!(count_requests(&conn).unwrap(), 1);
        assert!(skill_groups(&conn, 0, 10).unwrap().is_empty());
    }

    fn tool(id: &str, name: &str, ok: bool, ms: u64, ts: i64) -> TelemetryTool {
        TelemetryTool {
            session_id: "s1".into(),
            tool_use_id: id.into(),
            timestamp: ts,
            tool_name: name.into(),
            success: ok,
            duration_ms: ms,
        }
    }

    fn throttled(session: &str, seq: u64, attempt: u64, ts: i64, code: u64) -> TelemetryError {
        TelemetryError {
            session_id: session.into(),
            status_code: code,
            ..failure(seq, attempt, ts)
        }
    }

    #[test]
    fn only_a_429_inside_the_window_is_a_rate_limit_and_each_call_counts_once() {
        let conn = mem();
        let (start, end) = (1_000, 2_000);
        let rows = [
            throttled("s1", 1, 1, start - 1, 429),
            throttled("s1", 2, 1, start, 429),
            throttled("s1", 3, 1, end - 1, 429),
            throttled("s1", 4, 1, end, 429),
            // the same row sent again with another attempt is ignored, and a
            // 529 in the window is not a rate limit
            throttled("s1", 3, 2, end - 1, 429),
            // a later row of a retried call has its own sequence and time
            throttled("s1", 6, 2, start + 3, 429),
            throttled("s1", 5, 1, start + 5, 529),
            // another session may reuse a sequence number
            throttled("s2", 2, 1, start + 7, 429),
        ];
        for r in &rows {
            insert_error(&conn, r).unwrap();
        }

        let got = rate_limited_at(&conn, start, end).unwrap();

        assert_eq!(got, vec![start, start + 3, start + 7, end - 1]);
        assert!(rate_limited_at(&conn, 5_000, 6_000).unwrap().is_empty());
    }

    fn failure(seq: u64, attempt: u64, ts: i64) -> TelemetryError {
        TelemetryError {
            session_id: "s1".into(),
            sequence: seq,
            timestamp: ts,
            status_code: 529,
            attempt,
            duration_ms: 100,
            ..TelemetryError::default()
        }
    }

    #[test]
    fn tool_and_error_rows_dedupe_on_their_keys() {
        let conn = mem();
        assert_eq!(
            insert_tool(&conn, &tool("t1", "Bash", true, 5, 1)).unwrap(),
            1
        );
        assert_eq!(
            insert_tool(&conn, &tool("t1", "Bash", true, 5, 1)).unwrap(),
            0
        );
        assert_eq!(insert_error(&conn, &failure(1, 1, 1)).unwrap(), 1);
        assert_eq!(insert_error(&conn, &failure(1, 1, 1)).unwrap(), 0);
        assert_eq!(insert_error(&conn, &failure(2, 1, 1)).unwrap(), 1);
    }

    #[test]
    fn tool_stats_give_calls_failures_and_latency_busiest_first() {
        let conn = mem();
        for t in [
            tool("a", "Bash", true, 100, 10),
            tool("b", "Bash", false, 300, 20),
            tool("c", "Read", true, 10, 30),
            tool("d", "Read", true, 10, 5_000),
            tool("e", "", true, 1, 40),
        ] {
            insert_tool(&conn, &t).unwrap();
        }
        let stats = tool_stats(&conn, 0, 1_000).unwrap();
        let names: Vec<&str> = stats.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Bash", "Read", "unknown"]);
        let bash = &stats[0];
        assert_eq!((bash.calls, bash.failures, bash.max_ms), (2, 1, 300));
        assert_eq!(bash.avg_ms(), 200.0);
        assert_eq!(bash.error_rate(), 0.5);
        assert_eq!(
            stats[1].calls, 1,
            "the call outside the window is not counted"
        );
        assert!(tool_stats(&conn, 9_000, 9_100).unwrap().is_empty());
    }

    #[test]
    fn health_sums_requests_errors_retries_and_busy_time() {
        let conn = mem();
        let mut r = request("r1");
        r.duration_ms = 1_000;
        insert_request(&conn, &r).unwrap();
        insert_tool(&conn, &tool("a", "Bash", false, 500, 10)).unwrap();
        insert_error(&conn, &failure(1, 1, 10)).unwrap();
        insert_error(&conn, &failure(2, 2, 10)).unwrap();
        let h = health(&conn, 0, 1_000).unwrap();
        assert_eq!((h.requests, h.errors, h.retried), (1, 2, 1));
        assert_eq!((h.tool_calls, h.tool_failures), (1, 1));
        assert_eq!(h.busy_ms(), 1_500);
        assert!((h.api_error_rate() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(h.tool_error_rate(), 1.0);
        assert!(!h.is_empty());
        let none = health(&conn, 5_000, 6_000).unwrap();
        assert!(none.is_empty());
        assert_eq!((none.api_error_rate(), none.tool_error_rate()), (0.0, 0.0));
    }

    #[test]
    fn health_adds_active_time_lines_commits_and_pull_requests() {
        let conn = mem();
        let m = |metric: &str, kind: &str, end: i64, v: f64| TelemetryMetric {
            session_id: "s".into(),
            metric: metric.into(),
            kind: kind.into(),
            start_ns: end - 1,
            end_ns: end,
            timestamp: end,
            value: v,
        };
        for row in [
            m("active_time", "user", 10, 30.0),
            m("active_time", "cli", 10, 90.0),
            m("lines_of_code", "added", 10, 40.0),
            m("lines_of_code", "removed", 10, 10.0),
            m("commits", "", 10, 2.0),
            m("pull_requests", "", 10, 1.0),
            m("active_time", "cli", 10, 5.0),
        ] {
            insert_metric(&conn, &row).unwrap();
        }
        // The same interval, session, metric and kind is one point.
        let h = health(&conn, 0, 100).unwrap();
        assert_eq!(h.active_ms, 120_000);
        assert_eq!((h.lines_changed, h.commits, h.pull_requests), (50, 2, 1));
        assert!(!h.is_empty());
        assert!(health(&conn, 100, 200).unwrap().is_empty());
    }

    fn fill(conn: &Connection, n: i64) {
        for i in 0..n {
            let mut r = request(&format!("r{i}"));
            r.timestamp = 1_000 + i;
            insert_request(conn, &r).unwrap();
            insert_tool(conn, &tool(&format!("t{i}"), "Bash", true, 1, 1_000 + i)).unwrap();
            insert_error(conn, &failure(i as u64, 1, 1_000 + i)).unwrap();
            let m = TelemetryMetric {
                session_id: "s".into(),
                metric: "commits".into(),
                kind: String::new(),
                start_ns: i,
                end_ns: i + 1,
                timestamp: 1_000 + i,
                value: 1.0,
            };
            insert_metric(conn, &m).unwrap();
        }
    }

    #[test]
    fn prune_drops_rows_older_than_the_retention_in_every_table() {
        let conn = mem();
        fill(&conn, 10);
        let day = 86_400;
        // Rows are at 1000..1009; keep rows newer than now - 1 day = 1005.
        let r = Retention {
            days: 1,
            max_rows: 0,
        };
        let dry = prune(&conn, 1_005 + day, r, true).unwrap();
        assert_eq!(dry.total(), 20, "{dry:?}");
        assert_eq!(
            count_requests(&conn).unwrap(),
            10,
            "a dry run deletes nothing"
        );
        let done = prune(&conn, 1_005 + day, r, false).unwrap();
        assert_eq!(done, dry);
        assert_eq!(count_requests(&conn).unwrap(), 5);
        let left: i64 = conn
            .query_row("SELECT MIN(timestamp) FROM telemetry_metrics", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(left, 1_005);
    }

    #[test]
    fn prune_keeps_the_newest_rows_up_to_the_row_limit_and_zero_means_no_limit() {
        let conn = mem();
        fill(&conn, 10);
        let r = Retention {
            days: 0,
            max_rows: 4,
        };
        let done = prune(&conn, 5_000_000, r, false).unwrap();
        assert_eq!(
            (done.requests, done.tools, done.errors, done.metrics),
            (6, 6, 6, 6)
        );
        let oldest: i64 = conn
            .query_row("SELECT MIN(timestamp) FROM telemetry_requests", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(oldest, 1_006);
        let none = Retention {
            days: 0,
            max_rows: 0,
        };
        assert_eq!(prune(&conn, 9_000_000, none, false).unwrap().total(), 0);
        assert_eq!(count_requests(&conn).unwrap(), 4);
    }

    #[test]
    fn prune_leaves_the_transcript_tables_alone() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE usage_events (event_id TEXT); INSERT INTO usage_events VALUES ('a');",
        )
        .unwrap();
        migrate_up(&conn).unwrap();
        prune(
            &conn,
            i64::MAX / 4,
            Retention {
                days: 1,
                max_rows: 1,
            },
            false,
        )
        .unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_table_from_before_slice_two_gains_the_duration_column() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE telemetry_requests (
                session_id TEXT NOT NULL, request_id TEXT NOT NULL, timestamp INTEGER NOT NULL,
                model TEXT NOT NULL, effort TEXT NOT NULL DEFAULT '',
                agent_name TEXT NOT NULL DEFAULT '', skill_name TEXT NOT NULL DEFAULT '',
                input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                cache_creation_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL, PRIMARY KEY (session_id, request_id));
             INSERT INTO telemetry_requests VALUES ('s','r',1,'m','','','',1,1,0,0,0.5);",
        )
        .unwrap();
        migrate_up(&conn).unwrap();
        assert_eq!(health(&conn, 0, 10).unwrap().api_ms, 0);
        insert_request(&conn, &request("r2")).unwrap();
    }

    #[test]
    fn invocation_groups_count_transcript_agents_inside_the_window_by_kind() {
        let dir = std::env::temp_dir().join(format!("pbtop-inv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::open_db(&dir.join("usage.db")).unwrap();
        let tool =
            |id: &str, ts: i64, kind: crate::ToolKind, name: &str| crate::ToolInvocationEvent {
                event_id: id.into(),
                timestamp: ts,
                session_id: "s".into(),
                account: "a".into(),
                kind,
                name: name.into(),
            };
        for e in [
            tool("1", 10, crate::ToolKind::Agent, "Explore"),
            tool("2", 20, crate::ToolKind::Agent, "Explore"),
            tool("3", 30, crate::ToolKind::Agent, "reviewer"),
            tool("4", 40, crate::ToolKind::Skill, "plan"),
            tool("5", 5_000, crate::ToolKind::Agent, "late"),
        ] {
            crate::db::insert_tool_event(&conn, &e).unwrap();
        }

        let groups = invocation_groups(&conn, crate::ToolKind::Agent, 0, 1_000).unwrap();

        let got: Vec<(&str, u64)> = groups
            .iter()
            .map(|g| (g.key.as_str(), g.messages))
            .collect();
        assert_eq!(got, [("Explore", 2), ("reviewer", 1)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    const OLD_TOOLS_AND_ERRORS: &str = "
        CREATE TABLE telemetry_tools (
            session_id TEXT NOT NULL, tool_use_id TEXT NOT NULL, timestamp INTEGER NOT NULL,
            tool_name TEXT NOT NULL, success INTEGER NOT NULL CHECK(success IN (0, 1)),
            duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
            PRIMARY KEY (session_id, tool_use_id));
        CREATE INDEX telemetry_tools_by_time ON telemetry_tools (timestamp);
        CREATE TABLE telemetry_errors (
            session_id TEXT NOT NULL, sequence INTEGER NOT NULL, timestamp INTEGER NOT NULL,
            model TEXT NOT NULL, status_code INTEGER NOT NULL, attempt INTEGER NOT NULL,
            duration_ms INTEGER NOT NULL, PRIMARY KEY (session_id, sequence));
        INSERT INTO telemetry_tools VALUES ('s1', 'tu1', 100, 'Bash', 0, 40);
        INSERT INTO telemetry_tools VALUES ('s1', 'tu2', 200, 'Read', 1, 5);
        INSERT INTO telemetry_errors VALUES ('s1', 1, 100, 'opus', 529, 1, 90);";

    fn sources(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("SELECT source FROM {table} ORDER BY rowid"))
            .unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    #[test]
    fn migrating_the_previous_schema_keeps_rows_and_marks_them_otlp() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(OLD_TOOLS_AND_ERRORS).unwrap();
        migrate_up(&conn).unwrap();
        migrate_up(&conn).unwrap();
        assert_eq!(sources(&conn, "telemetry_tools"), ["otlp", "otlp"]);
        assert_eq!(sources(&conn, "telemetry_errors"), ["otlp"]);
        let (ok, ms): (i64, i64) = conn
            .query_row(
                "SELECT success, duration_ms FROM telemetry_tools WHERE tool_use_id = 'tu1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((ok, ms), (0, 40));
        // The rebuilt table accepts an unknown outcome and an unknown duration.
        conn.execute(
            "INSERT INTO telemetry_tools VALUES ('s2', 'tu3', 1, 'Agent', NULL, NULL, 'transcript')",
            [],
        )
        .unwrap();
        let by_use: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'telemetry_tools_by_use'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(by_use, 1);
    }

    fn with_events(events: &[(&str, &str, i64, &str)]) -> Connection {
        let conn = mem();
        conn.execute_batch(
            "CREATE TABLE tool_invocation_events (event_id TEXT PRIMARY KEY,
                timestamp INTEGER NOT NULL, session_id TEXT NOT NULL, account TEXT NOT NULL,
                kind TEXT NOT NULL, name TEXT NOT NULL);",
        )
        .unwrap();
        for (id, session, ts, kind) in events {
            let name = if *kind == "agent" {
                "playbook:reviewer"
            } else {
                "playbook:plan"
            };
            conn.execute(
                "INSERT INTO tool_invocation_events VALUES (?1, ?2, ?3, 'a', ?4, ?5)",
                params![id, ts, session, kind, name],
            )
            .unwrap();
        }
        conn
    }

    const FOREVER: Retention = Retention {
        days: 0,
        max_rows: 0,
    };
    const NOW: i64 = 1_000_000_000;
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

    fn backfill(conn: &Connection) -> usize {
        backfill_transcript_tools(conn, NOW, FOREVER, BUDGET).unwrap()
    }

    fn by_name(conn: &Connection) -> Vec<(String, i64)> {
        let mut stmt = conn
            .prepare("SELECT tool_name, COUNT(*) FROM telemetry_tools GROUP BY 1 ORDER BY 1")
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        rows.map(Result::unwrap).collect()
    }

    #[test]
    fn backfill_counts_match_a_hand_counted_fixture() {
        let conn = with_events(&[
            ("a1", "s1", 10, "agent"),
            ("a2", "s1", 20, "agent"),
            ("a3", "s2", 30, "agent"),
            ("k1", "s1", 40, "skill"),
            ("k2", "s2", 50, "skill"),
        ]);
        assert_eq!(backfill(&conn), 5);
        assert_eq!(
            by_name(&conn),
            [("Agent".to_string(), 3), ("Skill".to_string(), 2)]
        );
        assert_eq!(sources(&conn, "telemetry_tools"), ["transcript"; 5]);
        let known: i64 = conn
            .query_row(
                "SELECT COUNT(success) + COUNT(duration_ms) FROM telemetry_tools",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(known, 0, "no outcome or duration is invented");
    }

    #[test]
    fn a_second_backfill_run_changes_nothing() {
        let conn = with_events(&[("a1", "s1", 10, "agent"), ("k1", "s1", 40, "skill")]);
        assert_eq!(backfill(&conn), 2);
        let before = by_name(&conn);
        assert_eq!(backfill(&conn), 0);
        assert_eq!(by_name(&conn), before);
    }

    #[test]
    fn a_live_row_with_the_same_tool_use_id_is_kept_once_and_untouched() {
        let conn = with_events(&[("tu9", "s1", 10, "agent"), ("tu8", "s1", 20, "agent")]);
        insert_tool(&conn, &tool("tu9", "Agent", false, 321, 11)).unwrap();
        backfill(&conn);
        let (n, src, ok, ms): (i64, String, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), MAX(source), MIN(success), MIN(duration_ms)
                 FROM telemetry_tools WHERE tool_use_id = 'tu9'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((n, src.as_str(), ok, ms), (1, "otlp", 0, 321));
        assert_eq!(sources(&conn, "telemetry_tools").len(), 2);
    }

    #[test]
    fn a_live_row_that_arrives_later_replaces_the_transcript_row() {
        let conn = with_events(&[("tu9", "s1", 10, "agent")]);
        backfill(&conn);
        insert_tool(&conn, &tool("tu9", "Agent", true, 77, 11)).unwrap();
        insert_tool(&conn, &tool("tu9", "Agent", true, 77, 11)).unwrap();
        assert_eq!(sources(&conn, "telemetry_tools"), ["otlp"]);
        backfill(&conn);
        assert_eq!(sources(&conn, "telemetry_tools"), ["otlp"]);
    }

    #[test]
    fn a_differing_agent_name_does_not_duplicate_the_call() {
        // The live data calls the agent `custom`, the transcript says
        // `playbook:reviewer`. Both are one Agent call with one tool use id.
        let conn = with_events(&[("tu1", "s1", 10, "agent")]);
        insert_tool(&conn, &tool("tu1", "Agent", true, 9, 10)).unwrap();
        backfill(&conn);
        assert_eq!(by_name(&conn), [("Agent".to_string(), 1)]);
    }

    #[test]
    fn transcript_only_and_live_only_days_both_keep_their_rows() {
        let day = 86_400;
        let conn = with_events(&[("old1", "s1", 2 * day + 5, "agent")]);
        insert_tool(&conn, &tool("live1", "Bash", true, 3, 5 * day + 5)).unwrap();
        backfill(&conn);
        let per_day = |lo: i64, hi: i64| -> Vec<String> {
            let mut stmt = conn
                .prepare(
                    "SELECT source FROM telemetry_tools WHERE timestamp >= ?1 AND timestamp < ?2",
                )
                .unwrap();
            let rows = stmt.query_map([lo, hi], |r| r.get(0)).unwrap();
            rows.map(Result::unwrap).collect()
        };
        assert_eq!(per_day(2 * day, 3 * day), ["transcript"]);
        assert_eq!(per_day(5 * day, 6 * day), ["otlp"]);
    }

    #[test]
    fn health_and_tool_stats_stay_live_only() {
        let conn = with_events(&[("a1", "s1", 10, "agent"), ("a2", "s1", 11, "agent")]);
        insert_tool(&conn, &tool("live", "Bash", false, 40, 12)).unwrap();
        backfill(&conn);
        let h = health(&conn, 0, 100).unwrap();
        assert_eq!((h.tool_calls, h.tool_failures, h.tool_ms), (1, 1, 40));
        let stats = tool_stats(&conn, 0, 100).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!((stats[0].name.as_str(), stats[0].calls), ("Bash", 1));
        assert!(
            health(&conn, 10, 12).unwrap().is_empty(),
            "no live rows, no data"
        );
    }

    #[test]
    fn backfill_follows_the_retention_window() {
        let day = 86_400;
        let conn = with_events(&[
            ("old", "s1", NOW - 100 * day, "agent"),
            ("new", "s1", NOW - day, "agent"),
        ]);
        let r = Retention {
            days: 90,
            max_rows: 0,
        };
        assert_eq!(backfill_transcript_tools(&conn, NOW, r, BUDGET).unwrap(), 1);
        assert_eq!(backfill(&conn), 1, "no limit takes the rest");
    }

    #[test]
    fn a_spent_budget_still_writes_one_chunk_and_the_next_run_continues() {
        let ids: Vec<String> = (0..2_500).map(|i| format!("t{i}")).collect();
        let events: Vec<(&str, &str, i64, &str)> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), "s1", 1_000 + i as i64, "skill"))
            .collect();
        let conn = with_events(&events);
        let none = std::time::Duration::ZERO;
        let run = || backfill_transcript_tools(&conn, NOW, FOREVER, none).unwrap();
        assert_eq!(run(), 2_000);
        assert_eq!(run(), 500);
        assert_eq!(run(), 0);
    }
}
