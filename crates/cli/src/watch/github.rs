// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The GitHub list endpoints as an [`EventSource`]: one poller per endpoint,
//! with `since`, ETag, `Link` paging, `X-Poll-Interval` and rate limits. All
//! network traffic goes through [`Http`], so tests replay recorded replies.

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::clock::Clock;
use super::event::{Event, COMMENT_HEAD_CHARS};
use super::rule::On;
use super::source::{EventSource, Poll, SourceError};
use super::store::Cursor;
use crate::common::time::{iso_utc, parse_iso8601_utc_epoch};

/// A page holds at most this many items.
pub const PAGE_SIZE: usize = 100;
/// Pages read in one poll. The cursor stops at the last full page.
pub const MAX_PAGES: usize = 10;
/// A reply body larger than this is refused.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// The headers of a reply that the poller reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub etag: Option<String>,
    /// The `rel="next"` URL of the `Link` header.
    pub next: Option<String>,
    pub poll_interval: Option<i64>,
    pub retry_after: Option<i64>,
    pub remaining: Option<i64>,
    pub reset: Option<i64>,
}

/// One GET. The implementation adds the token and sends `If-None-Match` when
/// `etag` is given. It writes the reply body into `body`.
pub trait Http {
    fn get(
        &mut self,
        url: &str,
        etag: Option<&str>,
        body: &mut Vec<u8>,
    ) -> Result<Reply, SourceError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Issues,
    IssueComments,
    PullComments,
}

impl Endpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::IssueComments => "issues/comments",
            Self::PullComments => "pulls/comments",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Issues => "issues",
            Self::IssueComments => "issue-comments",
            Self::PullComments => "pull-comments",
        }
    }
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Label {
    Name(String),
    Object { name: String },
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    number: u64,
    created_at: String,
    updated_at: String,
    user: User,
    #[serde(default)]
    author_association: String,
    #[serde(default)]
    pull_request: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
}

pub struct GithubPoller<H, C> {
    http: H,
    clock: C,
    repo: String,
    endpoint: Endpoint,
    poll_seconds: i64,
    body: Vec<u8>,
}

impl<H: Http, C: Clock> GithubPoller<H, C> {
    pub fn new(http: H, clock: C, repo: &str, endpoint: Endpoint, poll_seconds: i64) -> Self {
        Self {
            http,
            clock,
            repo: repo.to_string(),
            endpoint,
            poll_seconds,
            body: Vec::new(),
        }
    }

    fn first_url(&self, since: i64) -> String {
        let state = if self.endpoint == Endpoint::Issues {
            "&state=all"
        } else {
            ""
        };
        format!(
            "https://api.github.com/repos/{}/{}?sort=updated&direction=asc&per_page={PAGE_SIZE}&since={}{state}",
            self.repo,
            self.endpoint.path(),
            iso_utc(since)
        )
    }

    /// Turns a status into an error, or lets 200 and 304 through.
    fn check(&self, reply: &Reply) -> Result<(), SourceError> {
        match reply.status {
            200 | 304 => Ok(()),
            401 => Err(SourceError::Unauthorized),
            403 | 429 => {
                if let Some(after) = reply.retry_after {
                    Err(SourceError::RateLimited {
                        retry_at: self.clock.now() + after,
                    })
                } else if let (Some(0), Some(reset)) = (reply.remaining, reply.reset) {
                    Err(SourceError::RateLimited { retry_at: reset })
                } else {
                    Err(SourceError::Transient(format!("HTTP {}", reply.status)))
                }
            }
            other => Err(SourceError::Transient(format!("HTTP {other}"))),
        }
    }

    /// Parses the page in `self.body` into events. Returns the item count.
    fn read_page(
        &self,
        since: i64,
        out: &mut Vec<Event>,
        newest: &mut i64,
    ) -> Result<usize, SourceError> {
        let items: Vec<Item> = serde_json::from_slice(&self.body)
            .map_err(|e| SourceError::Malformed(e.to_string()))?;
        for item in &items {
            let updated = parse_iso8601_utc_epoch(&item.updated_at)
                .ok_or_else(|| SourceError::Malformed("bad updated_at".to_string()))?;
            let created = parse_iso8601_utc_epoch(&item.created_at)
                .ok_or_else(|| SourceError::Malformed("bad created_at".to_string()))?;
            *newest = (*newest).max(updated);
            // An edit or a new comment on an old item is not a new event.
            if created < since {
                continue;
            }
            out.push(self.event(item, updated));
        }
        Ok(items.len())
    }

    fn event(&self, item: &Item, updated_at: i64) -> Event {
        let body = item.body.as_deref().unwrap_or("");
        let (key, kind, object) = match self.endpoint {
            Endpoint::Issues if item.pull_request.is_some() => (
                format!("pr:{}:opened", item.number),
                On::PrOpened,
                item.number,
            ),
            Endpoint::Issues => (
                format!("issue:{}:opened", item.number),
                On::IssueOpened,
                item.number,
            ),
            Endpoint::IssueComments => {
                let on_pr = item
                    .html_url
                    .as_deref()
                    .is_some_and(|u| u.contains("/pull/"));
                (
                    format!("comment:{}:created", item.id),
                    if on_pr {
                        On::PrCommented
                    } else {
                        On::IssueCommented
                    },
                    item.id,
                )
            }
            Endpoint::PullComments => (
                format!("reviewcomment:{}:created", item.id),
                On::PrCommented,
                item.id,
            ),
        };
        Event {
            key,
            repo: self.repo.clone(),
            kind,
            author: item.user.login.clone(),
            association: item.author_association.clone(),
            object,
            updated_at,
            body_hash: Sha256::digest(body.as_bytes()).into(),
            labels: item
                .labels
                .iter()
                .map(|l| match l {
                    Label::Name(n) | Label::Object { name: n } => n.clone(),
                })
                .collect(),
            comment_head: if self.endpoint == Endpoint::Issues {
                String::new()
            } else {
                body.chars().take(COMMENT_HEAD_CHARS).collect()
            },
        }
    }
}

impl<H: Http, C: Clock> EventSource for GithubPoller<H, C> {
    fn name(&self) -> &str {
        self.endpoint.name()
    }

    fn poll(&mut self, cursor: &Cursor, out: &mut Vec<Event>) -> Result<Poll, SourceError> {
        out.clear();
        let first = self.first_url(cursor.since);
        let stored_etag = cursor
            .etag_url
            .as_deref()
            .filter(|u| *u == first)
            .and(cursor.etag.as_deref());

        let mut next_in = self.poll_seconds;
        let mut newest = cursor.since;
        let mut items = 0;
        let mut first_etag = None;
        let mut url = first.clone();
        for page in 0..MAX_PAGES {
            self.body.clear();
            let etag = if page == 0 { stored_etag } else { None };
            let reply = self.http.get(&url, etag, &mut self.body)?;
            self.check(&reply)?;
            next_in = next_in
                .max(reply.poll_interval.unwrap_or(0))
                .max(reply.retry_after.unwrap_or(0));
            if reply.status == 304 {
                return Ok(Poll::Unchanged { next_in });
            }
            if self.body.len() > MAX_BODY {
                return Err(SourceError::Malformed("reply body over 4 MiB".to_string()));
            }
            if page == 0 {
                first_etag = reply.etag.clone();
            }
            let count = self.read_page(cursor.since, out, &mut newest)?;
            items += count;
            match reply.next {
                Some(next) if count >= PAGE_SIZE => url = next,
                _ => break,
            }
        }
        if items == 0 {
            return Ok(Poll::Unchanged { next_in });
        }
        // The ETag belongs to the first URL. It stays useful only while that
        // URL, and so `since`, does not move.
        let keeps_url = newest == cursor.since;
        Ok(Poll::Events {
            next: Cursor {
                since: newest,
                etag_url: keeps_url.then_some(first),
                etag: if keeps_url { first_etag } else { None },
                next_poll_at: cursor.next_poll_at,
            },
            next_in,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::clock::ManualClock;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::Path;

    struct Fake {
        replies: VecDeque<(Reply, Vec<u8>)>,
        urls: Vec<String>,
        etags: Vec<Option<String>>,
    }

    impl Fake {
        fn new(replies: Vec<(Reply, Vec<u8>)>) -> Self {
            Self {
                replies: replies.into(),
                urls: Vec::new(),
                etags: Vec::new(),
            }
        }
    }

    impl Http for Fake {
        fn get(
            &mut self,
            url: &str,
            etag: Option<&str>,
            body: &mut Vec<u8>,
        ) -> Result<Reply, SourceError> {
            self.urls.push(url.to_string());
            self.etags.push(etag.map(str::to_string));
            let (reply, bytes) = self.replies.pop_front().expect("an unexpected request");
            body.extend_from_slice(&bytes);
            Ok(reply)
        }
    }

    fn ok(body: &str) -> (Reply, Vec<u8>) {
        (
            Reply {
                status: 200,
                etag: Some("W/\"e1\"".into()),
                ..Reply::default()
            },
            body.as_bytes().to_vec(),
        )
    }

    fn status(code: u16) -> (Reply, Vec<u8>) {
        (
            Reply {
                status: code,
                ..Reply::default()
            },
            Vec::new(),
        )
    }

    fn poller(
        replies: Vec<(Reply, Vec<u8>)>,
        endpoint: Endpoint,
    ) -> GithubPoller<Fake, ManualClock> {
        GithubPoller::new(
            Fake::new(replies),
            ManualClock::new(1_000),
            "o/r",
            endpoint,
            60,
        )
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/watch")
            .join(name);
        fs::read_to_string(path).unwrap()
    }

    fn item(id: u64, created: &str, updated: &str) -> String {
        format!(
            r#"{{"id":{id},"number":{id},"created_at":"{created}","updated_at":"{updated}","user":{{"login":"user-1"}},"author_association":"NONE","labels":[{{"name":"bug"}}],"body":"hello"}}"#
        )
    }

    fn cursor(since: i64) -> Cursor {
        Cursor {
            since,
            ..Cursor::default()
        }
    }

    #[test]
    fn the_recorded_200_gives_events_in_ascending_order_and_moves_since() {
        // Arrange
        let mut p = poller(vec![ok(&fixture("issues.200.json"))], Endpoint::Issues);
        let mut out = Vec::new();

        // Act
        let poll = p.poll(&cursor(1_759_000_000), &mut out).unwrap();

        // Assert
        let times: Vec<i64> = out.iter().map(|e| e.updated_at).collect();
        assert_eq!(out.len(), 2);
        assert!(times.windows(2).all(|w| w[0] <= w[1]));
        let Poll::Events { next, .. } = poll else {
            panic!("expected events");
        };
        assert_eq!(next.since, *times.last().unwrap());
    }

    #[test]
    fn the_recorded_comment_replies_become_comment_events() {
        let mut p = poller(
            vec![ok(&fixture("issue-comments.200.json"))],
            Endpoint::IssueComments,
        );
        let mut out = Vec::new();
        p.poll(&cursor(1_759_000_000), &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out
            .iter()
            .all(|e| e.key.starts_with("comment:") && e.kind == On::IssueCommented));
    }

    #[test]
    fn a_304_is_unchanged_leaves_the_buffer_empty_and_keeps_its_capacity() {
        // Arrange
        let mut p = poller(vec![status(304)], Endpoint::Issues);
        let mut out = Vec::with_capacity(8);
        let before = out.capacity();

        // Act
        let poll = p.poll(&cursor(5), &mut out).unwrap();

        // Assert
        assert_eq!(poll, Poll::Unchanged { next_in: 60 });
        assert!(out.is_empty());
        assert_eq!(out.capacity(), before);
        assert_eq!(p.body.capacity(), 0);
    }

    #[test]
    fn the_stored_etag_is_sent_only_for_the_same_url() {
        let mut p = poller(vec![status(304), status(304)], Endpoint::Issues);
        let mut out = Vec::new();
        let url = p.first_url(100);
        let same = Cursor {
            since: 100,
            etag_url: Some(url),
            etag: Some("W/\"x\"".into()),
            next_poll_at: 0,
        };
        let moved = Cursor {
            since: 200,
            ..same.clone()
        };
        p.poll(&same, &mut out).unwrap();
        p.poll(&moved, &mut out).unwrap();
        assert_eq!(p.http.etags, vec![Some("W/\"x\"".to_string()), None]);
    }

    #[test]
    fn an_empty_page_is_unchanged() {
        let mut p = poller(vec![ok("[]")], Endpoint::Issues);
        let mut out = Vec::new();
        assert_eq!(
            p.poll(&cursor(5), &mut out).unwrap(),
            Poll::Unchanged { next_in: 60 }
        );
    }

    #[test]
    fn an_old_issue_that_was_only_updated_moves_the_cursor_without_an_event() {
        let body = format!(
            "[{}]",
            item(7, "2026-01-01T00:00:00Z", "2026-11-01T00:00:00Z")
        );
        let mut p = poller(vec![ok(&body)], Endpoint::Issues);
        let mut out = Vec::new();
        let poll = p.poll(&cursor(1_790_000_000), &mut out).unwrap();
        assert!(out.is_empty());
        let Poll::Events { next, .. } = poll else {
            panic!("expected a cursor move");
        };
        assert!(next.since > 1_790_000_000);
    }

    #[test]
    fn a_full_page_with_a_next_link_reads_the_next_page() {
        let full: Vec<String> = (1..=100)
            .map(|n| item(n, "2026-11-01T00:00:00Z", "2026-11-01T00:00:00Z"))
            .collect();
        let page1 = ok(&format!("[{}]", full.join(",")));
        let (mut reply, body) = page1;
        reply.next = Some("https://api.github.com/page2".into());
        let page2 = ok(&format!(
            "[{}]",
            item(101, "2026-11-01T00:00:00Z", "2026-11-01T00:00:01Z")
        ));
        let mut p = poller(vec![(reply, body), page2], Endpoint::Issues);
        let mut out = Vec::new();
        p.poll(&cursor(0), &mut out).unwrap();
        assert_eq!(out.len(), 101);
        assert_eq!(p.http.urls[1], "https://api.github.com/page2");
    }

    #[test]
    fn at_most_ten_pages_are_read() {
        let full: Vec<String> = (1..=100)
            .map(|n| item(n, "2026-11-01T00:00:00Z", "2026-11-01T00:00:00Z"))
            .collect();
        let body = format!("[{}]", full.join(","));
        let replies: Vec<_> = (0..11)
            .map(|_| {
                let (mut r, b) = ok(&body);
                r.next = Some("https://api.github.com/more".into());
                (r, b)
            })
            .collect();
        let mut p = poller(replies, Endpoint::Issues);
        let mut out = Vec::new();
        p.poll(&cursor(0), &mut out).unwrap();
        assert_eq!(p.http.urls.len(), MAX_PAGES);
    }

    #[test]
    fn a_body_over_four_mib_is_malformed_and_exactly_four_is_read() {
        let pad = |n: usize| {
            format!(
                "[{}{}]",
                item(1, "2026-11-01T00:00:00Z", "2026-11-01T00:00:00Z"),
                " ".repeat(n)
            )
        };
        let base = pad(0).len();
        let mut out = Vec::new();
        let mut at_limit = poller(vec![ok(&pad(MAX_BODY - base))], Endpoint::Issues);
        assert!(at_limit.poll(&cursor(0), &mut out).is_ok());
        let mut over = poller(vec![ok(&pad(MAX_BODY - base + 1))], Endpoint::Issues);
        assert!(matches!(
            over.poll(&cursor(0), &mut out),
            Err(SourceError::Malformed(_))
        ));
    }

    #[test]
    fn malformed_json_is_an_error() {
        let mut p = poller(vec![ok("{not json")], Endpoint::Issues);
        assert!(matches!(
            p.poll(&cursor(0), &mut Vec::new()),
            Err(SourceError::Malformed(_))
        ));
    }

    #[test]
    fn an_exhausted_limit_names_the_reset_time() {
        let reply = Reply {
            status: 403,
            remaining: Some(0),
            reset: Some(5_000),
            ..Reply::default()
        };
        let mut p = poller(vec![(reply, Vec::new())], Endpoint::Issues);
        assert_eq!(
            p.poll(&cursor(0), &mut Vec::new()),
            Err(SourceError::RateLimited { retry_at: 5_000 })
        );
    }

    #[test]
    fn retry_after_is_counted_from_now() {
        let reply = Reply {
            status: 429,
            retry_after: Some(30),
            ..Reply::default()
        };
        let mut p = poller(vec![(reply, Vec::new())], Endpoint::Issues);
        assert_eq!(
            p.poll(&cursor(0), &mut Vec::new()),
            Err(SourceError::RateLimited { retry_at: 1_030 })
        );
    }

    #[test]
    fn a_refused_token_and_a_server_error_are_told_apart() {
        let mut p = poller(vec![status(401), status(502)], Endpoint::Issues);
        assert_eq!(
            p.poll(&cursor(0), &mut Vec::new()),
            Err(SourceError::Unauthorized)
        );
        assert!(matches!(
            p.poll(&cursor(0), &mut Vec::new()),
            Err(SourceError::Transient(_))
        ));
    }

    #[test]
    fn the_wait_is_the_longest_of_the_setting_the_interval_and_retry_after() {
        for (interval, retry, want) in [
            (Some(120), None, 120),
            (Some(30), None, 60),
            (None, Some(3600), 3600),
            (None, Some(0), 60),
        ] {
            let reply = Reply {
                status: 304,
                poll_interval: interval,
                retry_after: retry,
                ..Reply::default()
            };
            let mut p = poller(vec![(reply, Vec::new())], Endpoint::Issues);
            assert_eq!(
                p.poll(&cursor(0), &mut Vec::new()).unwrap(),
                Poll::Unchanged { next_in: want },
                "{interval:?} {retry:?}"
            );
        }
    }

    #[test]
    fn a_comment_on_a_pull_request_is_a_pr_comment_and_keeps_only_its_head() {
        let long = "x".repeat(200);
        let body = format!(
            r#"[{{"id":9,"created_at":"2026-11-01T00:00:00Z","updated_at":"2026-11-01T00:00:00Z","user":{{"login":"user-2"}},"author_association":"MEMBER","html_url":"https://github.com/o/r/pull/4#issuecomment-9","body":"{long}"}}]"#
        );
        let mut p = poller(vec![ok(&body)], Endpoint::IssueComments);
        let mut out = Vec::new();
        p.poll(&cursor(0), &mut out).unwrap();
        assert_eq!(out[0].kind, On::PrCommented);
        assert_eq!(out[0].comment_head.len(), COMMENT_HEAD_CHARS);
    }
}
