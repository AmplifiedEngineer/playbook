// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Usage tracking: a `UsageSource` adapter turns one coding agent's local
//! session history into normalized events. Aggregation and storage never see
//! an agent's own file format, so a second source only implements the trait.

pub mod account;
pub mod account_move;
pub mod account_store;
pub mod agent_names;
pub mod aggregate;
pub mod api;
pub mod backfill;
pub mod burnup;
pub mod claude_code;
pub mod cli;
pub mod codex;
pub mod collector;
pub mod command;
pub mod config;
pub mod daemon;
pub mod data;
pub mod db;
pub mod entry;
pub mod host;
pub mod ingest;
pub mod limits;
pub mod limits_history;
pub mod limits_source;
pub mod local_total;
pub mod migrate;
pub mod monthly_use;
pub mod otlp;
pub mod period;
pub mod plan;
pub mod pricing;
pub mod query;
pub mod receiver;
pub mod reconcile;
pub mod repo;
pub mod run;
pub mod say;
pub mod service;
pub mod session_account;
pub mod session_view;
pub mod spend;
pub mod standalone;
pub mod summary;
pub mod telemetry_db;
pub mod telemetry_detect;
pub mod telemetry_status;
pub mod tui;
pub mod tz;
pub mod util;
pub mod warm;
pub mod writer;

/// One assistant message's token usage and derived cost. `event_id` is the
/// source's own unique message id, the dedup key (a message can span several
/// transcript lines carrying identical usage). `cache_creation_1h_tokens` is
/// the part of `cache_creation_tokens` written to the one hour cache; the rest
/// is the five minute cache. `agent` names the source (empty is filled with
/// the source name at ingest). `cost_usd` and `unpriced` are derived from the
/// token counts by `apply_pricing`, never read back from storage.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageEvent {
    pub event_id: String,
    pub timestamp: i64,
    pub session_id: String,
    pub account: String,
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub repo: String,
    pub branch: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_creation_1h_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    pub unpriced: bool,
}

impl UsageEvent {
    /// Prices the event from its own token counts. A model missing from the
    /// price table costs 0 and is flagged `unpriced` (when it used tokens).
    pub fn apply_pricing(&mut self) {
        let one_hour = self
            .cache_creation_1h_tokens
            .min(self.cache_creation_tokens);
        let tokens = pricing::Tokens {
            input: self.input_tokens,
            output: self.output_tokens,
            cache_write_5m: self.cache_creation_tokens - one_hour,
            cache_write_1h: one_hour,
            cache_read: self.cache_read_tokens,
        };
        match pricing::cost_usd(&self.model, &tokens) {
            Some(usd) => {
                self.cost_usd = usd;
                self.unpriced = false;
            }
            None => {
                self.cost_usd = 0.0;
                self.unpriced = tokens.total() > 0;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Skill,
    Agent,
}

impl ToolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolKind::Skill => "skill",
            ToolKind::Agent => "agent",
        }
    }
}

/// One `Skill` or `Agent` tool call. `event_id` is the tool_use id.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocationEvent {
    pub event_id: String,
    pub timestamp: i64,
    pub session_id: String,
    pub account: String,
    pub kind: ToolKind,
    pub name: String,
}

/// Everything a source yields for one scan.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Events {
    pub usage: Vec<UsageEvent>,
    pub tools: Vec<ToolInvocationEvent>,
}

/// An agent's local history, read as normalized events. Events carry an empty
/// `account`; ingest tags it once per run.
pub trait UsageSource {
    /// Stable name keying this source's watermark.
    fn name(&self) -> &str;
    /// Events with `timestamp >= watermark` (epoch seconds). The boundary is
    /// inclusive: a re-read of the boundary second is absorbed by dedup.
    fn events_since(&self, watermark: i64) -> Result<Events, String>;
}
