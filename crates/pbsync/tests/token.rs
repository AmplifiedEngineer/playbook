// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

use pbsync::token::{resolve_token, TokenError, DEFAULT_TOKEN_ENV};

fn env_with(value: &'static str) -> impl Fn(&str) -> Option<String> {
    move |name| (name == DEFAULT_TOKEN_ENV).then(|| value.to_string())
}

#[test]
fn config_value_beats_env() {
    // Arrange
    let env = env_with("from-env");
    // Act
    let token = resolve_token(Some("from-config"), DEFAULT_TOKEN_ENV, &env).unwrap();
    // Assert
    assert_eq!(
        token,
        resolve_token(Some("from-config"), DEFAULT_TOKEN_ENV, &|_| None).unwrap()
    );
}

#[test]
fn env_is_used_without_config() {
    // Arrange
    let env = env_with("from-env");
    // Act
    let token = resolve_token(None, DEFAULT_TOKEN_ENV, &env).unwrap();
    // Assert
    assert_eq!(
        token,
        resolve_token(Some("from-env"), DEFAULT_TOKEN_ENV, &|_| None).unwrap()
    );
}

#[test]
fn empty_token_is_an_error() {
    // Arrange
    let env = env_with("  ");
    // Act
    let from_env = resolve_token(None, DEFAULT_TOKEN_ENV, &env);
    let from_config = resolve_token(Some(""), DEFAULT_TOKEN_ENV, &env);
    // Assert
    assert_eq!(from_env.unwrap_err(), TokenError::Empty);
    assert_eq!(from_config.unwrap_err(), TokenError::Empty);
}

#[test]
fn missing_token_names_the_env_var() {
    // Arrange
    let env = |_: &str| None;
    // Act
    let error = resolve_token(None, DEFAULT_TOKEN_ENV, &env).unwrap_err();
    // Assert
    assert!(error.to_string().contains(DEFAULT_TOKEN_ENV));
}

#[test]
fn debug_and_display_are_redacted() {
    // Arrange
    let env = |_: &str| None;
    let token = resolve_token(Some("s3cret-value"), DEFAULT_TOKEN_ENV, &env).unwrap();
    // Act
    let shown = format!("{token} {token:?} {token:#?}");
    // Assert
    assert!(!shown.contains("s3cret-value"));
    assert!(shown.contains("[redacted]"));
}
