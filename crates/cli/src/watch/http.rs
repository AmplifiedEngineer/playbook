// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The real [`Http`] for the GitHub list endpoints, over `ureq`.

use std::io::Read;
use std::time::Duration;

use super::github::{Http, Reply, MAX_BODY};
use super::source::SourceError;

const TIMEOUT: Duration = Duration::from_secs(30);

pub struct UreqHttp {
    agent: ureq::Agent,
    token: String,
}

impl UreqHttp {
    pub fn new(token: &str) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .https_only(true)
            // 304 and rate limit replies carry the headers the poller reads.
            .http_status_as_error(false)
            .build()
            .into();
        Self {
            agent,
            token: token.to_string(),
        }
    }
}

/// The `rel="next"` URL of a `Link` header.
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (target, rel) = part.split_once(';')?;
        if !rel.contains("rel=\"next\"") {
            return None;
        }
        let url = target.trim().strip_prefix('<')?.strip_suffix('>')?;
        Some(url.to_string())
    })
}

/// Builds a [`Reply`] from a status and a header lookup.
pub fn reply_from<'a>(status: u16, header: impl Fn(&str) -> Option<&'a str>) -> Reply {
    let number = |name: &str| header(name).and_then(|v| v.trim().parse::<i64>().ok());
    Reply {
        status,
        etag: header("etag").map(str::to_string),
        next: header("link").and_then(next_link),
        poll_interval: number("x-poll-interval"),
        retry_after: number("retry-after"),
        remaining: number("x-ratelimit-remaining"),
        reset: number("x-ratelimit-reset"),
    }
}

impl Http for UreqHttp {
    fn get(
        &mut self,
        url: &str,
        etag: Option<&str>,
        body: &mut Vec<u8>,
    ) -> Result<Reply, SourceError> {
        let mut request = self
            .agent
            .get(url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(
                "User-Agent",
                concat!("playbook/", env!("CARGO_PKG_VERSION")),
            );
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        // The error text names the URL and the cause, never a header.
        let mut response = request
            .call()
            .map_err(|e| SourceError::Transient(e.to_string()))?;
        let reply = reply_from(response.status().as_u16(), |name| {
            response.headers().get(name).and_then(|v| v.to_str().ok())
        });
        // One byte past the limit is enough for the poller to refuse the page.
        response
            .body_mut()
            .as_reader()
            .take(MAX_BODY as u64 + 1)
            .read_to_end(body)
            .map_err(|e| SourceError::Transient(e.to_string()))?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn the_next_page_is_the_link_with_rel_next() {
        let header = r#"<https://api.github.com/a?page=2>; rel="next", <https://api.github.com/a?page=9>; rel="last""#;
        assert_eq!(
            next_link(header).as_deref(),
            Some("https://api.github.com/a?page=2")
        );
    }

    #[test]
    fn a_link_without_a_next_has_no_next_page() {
        assert_eq!(next_link(r#"<https://x/?page=1>; rel="prev""#), None);
    }

    #[test]
    fn a_reply_reads_the_headers_the_poller_needs() {
        // Arrange
        let headers: HashMap<&str, &str> = HashMap::from([
            ("etag", "W/\"abc\""),
            ("x-poll-interval", "120"),
            ("retry-after", "30"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1700000000"),
        ]);

        // Act
        let reply = reply_from(403, |name| headers.get(name).copied());

        // Assert
        assert_eq!(reply.status, 403);
        assert_eq!(reply.etag.as_deref(), Some("W/\"abc\""));
        assert_eq!(reply.poll_interval, Some(120));
        assert_eq!(reply.retry_after, Some(30));
        assert_eq!(
            (reply.remaining, reply.reset),
            (Some(0), Some(1_700_000_000))
        );
        assert_eq!(reply.next, None);
    }

    #[test]
    fn a_header_that_is_not_a_number_is_ignored() {
        let reply = reply_from(200, |name| (name == "x-poll-interval").then_some("soon"));
        assert_eq!(reply.poll_interval, None);
    }
}
