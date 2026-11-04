// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `file://` URIs, the only kind playbook sends to or accepts from a server.

use std::path::{Path, PathBuf};

/// The `file://` URI for an absolute path.
pub fn from_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

/// The path a `file://` URI names, or `None` for any other scheme, a remote
/// host or bad percent encoding.
pub fn to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(rest.len());
    let mut input = rest.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let hex = [input.next()?, input.next()?];
            let value = u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?;
            bytes.push(value);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_round_trips_through_its_uri() {
        for text in [
            "/repo/src/a.ts",
            "/repo/my dir/b c.ts",
            "/repo/日本/é.rs",
            "/r/a%b#c?d.py",
        ] {
            // Arrange
            let path = Path::new(text);

            // Act
            let uri = from_path(path);
            let back = to_path(&uri);

            // Assert
            assert!(uri.starts_with("file:///"), "{uri}");
            assert!(
                !uri.contains(' ') && !uri.contains('#') && !uri.contains('?'),
                "{uri}"
            );
            assert_eq!(back.as_deref(), Some(path), "{uri}");
        }
    }

    #[test]
    fn other_schemes_remote_hosts_and_bad_encodings_are_refused() {
        for uri in [
            "untitled:Untitled-1",
            "https://example.com/a.ts",
            "file://remote-host/a.ts",
            "file:///a%zz",
            "file:///a%4",
            "file:///a%ff",
            "",
        ] {
            assert_eq!(to_path(uri), None, "{uri}");
        }
    }

    #[test]
    fn the_localhost_form_is_accepted() {
        assert_eq!(
            to_path("file://localhost/a/b.ts"),
            Some(PathBuf::from("/a/b.ts"))
        );
    }
}
