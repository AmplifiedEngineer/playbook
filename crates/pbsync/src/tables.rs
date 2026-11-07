// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Which local tables are shared and which of their columns leave the machine.
//! Only listed columns are sent, so a new local column stays local until it is listed.

/// The remote column type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Text,
    Integer,
    Real,
}

impl ColumnKind {
    pub const fn sql(self) -> &'static str {
        match self {
            Self::Text => "TEXT",
            Self::Integer => "INTEGER",
            Self::Real => "REAL",
        }
    }
}

/// What happens to a value before it is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Copy,
    HashAccount,
    HashRepo,
}

#[derive(Debug, Clone, Copy)]
pub struct ColumnDef {
    pub name: &'static str,
    pub kind: ColumnKind,
    pub transform: Transform,
    /// Overwritten on the remote when a re-sent row has grown.
    pub grows: bool,
}

/// One shared table. The remote table also carries the machine id as part of
/// its key, plus the push time and the schema version.
#[derive(Debug, Clone, Copy)]
pub struct TableDef {
    pub local_table: &'static str,
    pub remote_table: &'static str,
    pub key_columns: &'static [&'static str],
    /// A re-sent row replaces the remote one only when this column is larger.
    pub growth_column: &'static str,
    pub columns: &'static [ColumnDef],
}

const fn col(name: &'static str, kind: ColumnKind, transform: Transform) -> ColumnDef {
    ColumnDef {
        name,
        kind,
        transform,
        grows: false,
    }
}

const fn growing(name: &'static str, kind: ColumnKind) -> ColumnDef {
    ColumnDef {
        grows: true,
        ..copy(name, kind)
    }
}

const fn copy(name: &'static str, kind: ColumnKind) -> ColumnDef {
    col(name, kind, Transform::Copy)
}

const USAGE_EVENTS: TableDef = TableDef {
    local_table: "usage_events",
    remote_table: "shared_usage_events",
    key_columns: &["event_id"],
    growth_column: "output_tokens",
    columns: &[
        copy("event_id", ColumnKind::Text),
        copy("timestamp", ColumnKind::Integer),
        col("account", ColumnKind::Text, Transform::HashAccount),
        col("repo", ColumnKind::Text, Transform::HashRepo),
        copy("model", ColumnKind::Text),
        copy("effort", ColumnKind::Text),
        growing("input_tokens", ColumnKind::Integer),
        growing("output_tokens", ColumnKind::Integer),
        growing("cache_creation_tokens", ColumnKind::Integer),
        growing("cache_read_tokens", ColumnKind::Integer),
        growing("cache_creation_1h_tokens", ColumnKind::Integer),
        growing("cost_usd", ColumnKind::Real),
    ],
};

/// The definition for the local `usage_events` table.
pub const fn usage_events() -> TableDef {
    USAGE_EVENTS
}

#[cfg(test)]
mod tests {
    use super::*;

    // The columns of usage_events in crates/telemetry/src/db.rs, plus machine.
    // A copy, because pbsync does not depend on the telemetry crate.
    const LOCAL_COLUMNS: [&str; 16] = [
        "event_id",
        "timestamp",
        "session_id",
        "account",
        "model",
        "effort",
        "repo",
        "branch",
        "input_tokens",
        "output_tokens",
        "cache_creation_tokens",
        "cache_read_tokens",
        "cost_usd",
        "cache_creation_1h_tokens",
        "agent",
        "machine",
    ];
    const NEVER_SENT: [&str; 5] = ["session_id", "agent", "branch", "cwd", "email"];

    #[test]
    fn the_allowlist_excludes_identifying_columns() {
        // Arrange
        let table = usage_events();
        // Act
        let names: Vec<&str> = table.columns.iter().map(|c| c.name).collect();
        // Assert
        for banned in NEVER_SENT {
            assert!(!names.contains(&banned), "{banned} must stay local");
        }
    }

    #[test]
    fn every_listed_column_exists_locally_and_keys_are_listed() {
        // Arrange
        let table = usage_events();
        // Act
        let names: Vec<&str> = table.columns.iter().map(|c| c.name).collect();
        // Assert
        assert!(names.iter().all(|n| LOCAL_COLUMNS.contains(n)));
        assert!(table.key_columns.iter().all(|k| names.contains(k)));
        assert!(table.remote_table.starts_with("shared_"));
    }

    #[test]
    fn account_and_repo_are_hashed_and_nothing_else_is() {
        // Arrange
        let table = usage_events();
        // Act
        let hashed: Vec<(&str, Transform)> = table
            .columns
            .iter()
            .filter(|c| c.transform != Transform::Copy)
            .map(|c| (c.name, c.transform))
            .collect();
        // Assert
        assert_eq!(
            hashed,
            [
                ("account", Transform::HashAccount),
                ("repo", Transform::HashRepo)
            ]
        );
    }
}
