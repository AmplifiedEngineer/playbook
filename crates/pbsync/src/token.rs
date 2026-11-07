// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The database token: resolved from config or the environment, and never
//! printed.

use std::fmt;

/// The environment variable read when the config has no token.
pub const DEFAULT_TOKEN_ENV: &str = "PBSYNC_TOKEN";
const REDACTED: &str = "[redacted]";

/// A bearer token. Debug and Display print `[redacted]`.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// The raw secret, for the Authorization header only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Why no token could be resolved.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    Missing { env_name: String },
    Empty,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { env_name } => write!(
                f,
                "no sync token found: set the token config key or the {env_name} environment variable"
            ),
            Self::Empty => f.write_str("the sync token is empty: set a non-empty token"),
        }
    }
}

impl std::error::Error for TokenError {}

/// A config value wins over the named environment variable.
pub fn resolve_token(
    config_value: Option<&str>,
    env_name: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Token, TokenError> {
    let raw = match config_value {
        Some(value) => value.to_string(),
        None => env(env_name).ok_or_else(|| TokenError::Missing {
            env_name: env_name.to_string(),
        })?,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(TokenError::Empty);
    }
    Ok(Token(trimmed.to_string()))
}
