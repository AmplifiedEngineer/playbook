// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `UsageSource` over Claude Code's own session transcripts
//! (`~/.claude/projects/**/*.jsonl`). The root is injected so tests never
//! read the real home directory.
//!
//! One assistant message is split across several transcript lines (one per
//! content block), each repeating the same `usage`. Events are therefore
//! deduped by `message.id` (usage) and `tool_use.id` (tools), not by line.

use super::repo::RepoResolver;
use super::{Events, ToolInvocationEvent, ToolKind, UsageEvent, UsageSource};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

pub struct ClaudeCodeSource {
    root: PathBuf,
}

impl ClaudeCodeSource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl UsageSource for ClaudeCodeSource {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn events_since(&self, watermark: i64) -> Result<Events, String> {
        Ok(self.events_since_with(watermark, worker_count()))
    }
}

/// What one transcript line contributes: the usage of its message and the
/// tool calls it holds, already thinned by the worker's `Dedup`. Records keep
/// file and line order, so the merge applies the dedup rules exactly as one
/// sequential pass would.
struct Record {
    usage: Option<UsageEvent>,
    tools: Vec<ToolInvocationEvent>,
}

impl ClaudeCodeSource {
    /// `events_since` on `threads` worker threads. One thread reads the files
    /// in order on the calling thread; more threads read contiguous slices of
    /// the sorted file list, and the slices are merged in file order, so the
    /// result never depends on the thread count.
    pub(crate) fn events_since_with(&self, watermark: i64, threads: usize) -> Events {
        let mut files = self.files();
        // A file last modified before the watermark cannot hold a newer
        // event, so unchanged transcripts are never re-read. This keeps a
        // repeated ingest (the terminal view polls every few seconds) cheap.
        files.retain(|f| !modified_before(f, watermark));
        self.events_from_files(&files, watermark, threads)
    }

    /// Every transcript under the root, sorted by path.
    pub(crate) fn files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_jsonl(&self.root, &mut files);
        files.sort();
        files
    }

    /// The events of `files` (in the order given) at or after `watermark`.
    pub(crate) fn events_from_files(
        &self,
        files: &[PathBuf],
        watermark: i64,
        threads: usize,
    ) -> Events {
        let records = map_files(files, threads, Dedup::default, |file, repos, seen| {
            let mut out = Vec::new();
            read_lines(file, &mut |line| {
                if let Some(record) = line_record(line, watermark, repos, seen) {
                    out.push(record);
                }
            });
            out
        });

        let mut usage: Vec<UsageEvent> = Vec::new();
        let mut usage_index: HashMap<String, usize> = HashMap::new();
        let mut tools: Vec<ToolInvocationEvent> = Vec::new();
        let mut tool_seen: HashMap<String, ()> = HashMap::new();
        for record in records {
            if let Some(event) = record.usage {
                match usage_index.get(&event.event_id) {
                    Some(&i) if event.output_tokens > usage[i].output_tokens => {
                        // Keep the larger counts but the message's first timestamp.
                        let first = usage[i].timestamp.min(event.timestamp);
                        usage[i] = event;
                        usage[i].timestamp = first;
                    }
                    Some(_) => {}
                    None => {
                        usage_index.insert(event.event_id.clone(), usage.len());
                        usage.push(event);
                    }
                }
            }
            for tool in record.tools {
                if tool_seen.insert(tool.event_id.clone(), ()).is_none() {
                    tools.push(tool);
                }
            }
        }
        Events { usage, tools }
    }
}

/// What one worker thread has already seen, so a message that spans many lines
/// (and many tool calls repeated across them) is kept once, not once per line.
///
/// It keeps exactly the lines the merge could still act on. For a message id
/// that is the first line and every later line with strictly more output
/// tokens (the merge replaces a message only for a strictly larger count and
/// lowers its timestamp only then). For a tool id it is the first line. Every
/// other line is a no-op for the merge, so dropping it here changes nothing
/// except the memory held before the merge.
#[derive(Default)]
pub(crate) struct Dedup {
    most_output: HashMap<String, u64>,
    tools: HashMap<String, ()>,
}

/// The record for one transcript line, or `None` for a line that is not an
/// assistant message with a usable timestamp at or after the watermark, or
/// that adds nothing the merge needs.
fn line_record(
    line: &str,
    watermark: i64,
    repos: &mut RepoResolver,
    seen: &mut Dedup,
) -> Option<Record> {
    // Most lines are user turns and tool results. A line that never says
    // "assistant" cannot be an assistant message, so skip the JSON parse.
    if !line.contains("assistant") {
        return None;
    }
    let value = serde_json::from_str::<Value>(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_iso8601_utc)?;
    if timestamp < watermark {
        return None;
    }
    let usage = usage_event(&value, timestamp, repos).filter(|event| {
        match seen.most_output.get_mut(&event.event_id) {
            Some(most) if event.output_tokens > *most => {
                *most = event.output_tokens;
                true
            }
            Some(_) => false,
            None => {
                seen.most_output
                    .insert(event.event_id.clone(), event.output_tokens);
                true
            }
        }
    });
    let mut tools = tool_events(&value, timestamp);
    tools.retain(|tool| seen.tools.insert(tool.event_id.clone(), ()).is_none());
    if usage.is_none() && tools.is_empty() {
        return None;
    }
    Some(Record { usage, tools })
}

/// Worker threads for a scan: the cores available, at most 8.
pub(crate) fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8)
}

/// Runs `f` over `files` and returns every result in file order. `init` makes
/// the per-thread state `S` that `f` threads through its files. With one
/// thread, or very few files, the calling thread does all the work. Otherwise
/// the sorted list is cut into contiguous slices of about equal size in bytes
/// (a few huge transcripts would otherwise leave most threads idle), each slice
/// runs on its own scoped thread with its own `RepoResolver` and state, and
/// the slices are joined in order.
pub(crate) fn map_files<T: Send, S>(
    files: &[PathBuf],
    threads: usize,
    init: impl Fn() -> S + Sync,
    f: impl Fn(&Path, &mut RepoResolver, &mut S) -> Vec<T> + Sync,
) -> Vec<T> {
    let threads = threads.min(files.len()).max(1);
    if threads == 1 {
        let mut repos = RepoResolver::default();
        let mut state = init();
        return files
            .iter()
            .flat_map(|file| f(file, &mut repos, &mut state))
            .collect();
    }
    let sizes: Vec<u64> = files
        .iter()
        .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0).max(1))
        .collect();
    let slices = byte_balanced_slices(&sizes, threads);
    let (f, init) = (&f, &init);
    std::thread::scope(|scope| {
        let handles: Vec<_> = slices
            .into_iter()
            .map(|range| {
                let chunk = &files[range];
                scope.spawn(move || {
                    let mut repos = RepoResolver::default();
                    let mut state = init();
                    chunk
                        .iter()
                        .flat_map(|file| f(file, &mut repos, &mut state))
                        .collect::<Vec<T>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

/// Cuts `0..sizes.len()` into at most `parts` contiguous ranges whose byte
/// totals are as even as a left to right pass makes them. Every index lands
/// in exactly one range and no range is empty.
fn byte_balanced_slices(sizes: &[u64], parts: usize) -> Vec<std::ops::Range<usize>> {
    let total: u64 = sizes.iter().sum();
    let target = total.div_ceil(parts as u64).max(1);
    let mut ranges = Vec::with_capacity(parts);
    let mut start = 0;
    let mut acc = 0u64;
    for (i, size) in sizes.iter().enumerate() {
        acc += size;
        let last = i + 1 == sizes.len();
        if (acc >= target && ranges.len() + 1 < parts) || last {
            ranges.push(start..i + 1);
            start = i + 1;
            acc = 0;
        }
    }
    ranges
}

/// A line longer than this is skipped, so one corrupt or huge line cannot
/// exhaust memory.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Calls `f` for each line of `path`, decoding every line on its own. A line
/// with invalid UTF-8 is decoded lossily and an oversized line is skipped, so
/// a bad byte costs at most that one line, never the whole transcript. A read
/// error ends the file.
pub(crate) fn read_lines(path: &Path, f: &mut dyn FnMut(&str)) {
    let Ok(file) = fs::File::open(path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let read = (&mut reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut buf);
        match read {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.len() > MAX_LINE_BYTES && !buf.ends_with(b"\n") {
            if skip_to_newline(&mut reader).is_err() {
                return;
            }
            continue;
        }
        f(String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r']));
    }
}

fn skip_to_newline(reader: &mut impl BufRead) -> std::io::Result<()> {
    loop {
        let (found, used) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(());
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(i) => (true, i + 1),
                None => (false, available.len()),
            }
        };
        reader.consume(used);
        if found {
            return Ok(());
        }
    }
}

pub(crate) fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `file_type` comes with the directory entry for free. Only a symlink
        // needs a `stat` to learn whether it points at a directory.
        let is_dir = match entry.file_type() {
            Ok(kind) if kind.is_symlink() => path.is_dir(),
            Ok(kind) => kind.is_dir(),
            Err(_) => path.is_dir(),
        };
        if is_dir {
            collect_jsonl(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

pub(crate) fn modified_before(path: &Path, watermark: i64) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    i64::try_from(since_epoch.as_secs()).is_ok_and(|secs| secs < watermark)
}

pub(crate) fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

pub fn token(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Tokens written to the one hour cache, from `usage.cache_creation`. Absent
/// means the whole write counts as the five minute cache.
pub fn one_hour_cache_tokens(usage: &Value) -> u64 {
    usage
        .get("cache_creation")
        .map(|tiers| token(tiers, "ephemeral_1h_input_tokens"))
        .unwrap_or(0)
}

fn usage_event(line: &Value, timestamp: i64, repos: &mut RepoResolver) -> Option<UsageEvent> {
    let message = line.get("message")?;
    let event_id = message.get("id")?.as_str()?.to_string();
    let usage = message.get("usage")?;
    let cache_write = token(usage, "cache_creation_input_tokens");
    let mut event = UsageEvent {
        event_id,
        timestamp,
        session_id: str_field(line, "sessionId").to_string(),
        request_id: str_field(line, "requestId").to_string(),
        model: str_field(message, "model").to_string(),
        effort: str_field(line, "effort").to_string(),
        repo: repos.resolve(str_field(line, "cwd")),
        branch: str_field(line, "gitBranch").to_string(),
        input_tokens: token(usage, "input_tokens"),
        output_tokens: token(usage, "output_tokens"),
        cache_creation_tokens: cache_write,
        cache_creation_1h_tokens: one_hour_cache_tokens(usage),
        cache_read_tokens: token(usage, "cache_read_input_tokens"),
        ..UsageEvent::default()
    };
    event.apply_pricing();
    Some(event)
}

fn tool_events(line: &Value, timestamp: i64) -> Vec<ToolInvocationEvent> {
    let Some(blocks) = line
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let session_id = str_field(line, "sessionId");
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|b| {
            let input = b.get("input")?;
            let (kind, name) = match b.get("name")?.as_str()? {
                "Skill" => (ToolKind::Skill, str_field(input, "skill")),
                "Agent" | "Task" => {
                    let subagent = str_field(input, "subagent_type");
                    let name = if subagent.is_empty() {
                        str_field(input, "description")
                    } else {
                        subagent
                    };
                    (ToolKind::Agent, name)
                }
                _ => return None,
            };
            Some(ToolInvocationEvent {
                event_id: b.get("id")?.as_str()?.to_string(),
                timestamp,
                session_id: session_id.to_string(),
                account: String::new(),
                kind,
                name: name.to_string(),
            })
        })
        .collect()
}

/// `2026-09-01T08:51:22.982Z` to epoch seconds (UTC). Fractional seconds are
/// dropped. No date crate exists in this binary, so this uses the standard
/// days-from-civil conversion.
pub(crate) fn parse_iso8601_utc(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let time = time.trim_end_matches('Z');
    let time = time.split('.').next()?;
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next()?.parse().ok()?;
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/usage")
            .join(name)
    }

    #[test]
    fn parses_timestamps_to_independently_computed_epochs() {
        assert_eq!(
            parse_iso8601_utc("2026-09-01T08:51:22.982Z"),
            Some(1788252682)
        );
        assert_eq!(
            parse_iso8601_utc("2026-09-02T00:00:01.000Z"),
            Some(1788307201)
        );
        assert_eq!(parse_iso8601_utc("not a time"), None);
    }

    #[test]
    fn usage_events_dedup_by_message_id_and_skip_malformed_lines() {
        let events = ClaudeCodeSource::new(root("usage"))
            .events_since(0)
            .unwrap();

        assert_eq!(
            events.usage.len(),
            3,
            "msg_A spans two lines but is one message"
        );
        let a = &events.usage[0];
        assert_eq!(a.event_id, "msg_A");
        assert_eq!(a.timestamp, 1788252682);
        assert_eq!(a.model, "claude-sonnet-5");
        assert_eq!(a.effort, "high");
        assert_eq!(a.repo, "proj-one");
        assert_eq!(a.branch, "main");
        assert_eq!(a.session_id, "s1");
        assert_eq!(
            (
                a.input_tokens,
                a.output_tokens,
                a.cache_creation_tokens,
                a.cache_read_tokens
            ),
            (2, 770, 29653, 22178)
        );
        // Sonnet 5 is 2/10, all five minute cache: 2*2 + 770*10 + 29653*2.5 + 22178*0.2.
        assert!((a.cost_usd - 0.0862721).abs() < 1e-9);

        let b = &events.usage[1];
        assert_eq!(
            (b.event_id.as_str(), b.model.as_str(), b.effort.as_str()),
            ("msg_B", "claude-opus-5", "xhigh")
        );
        assert_eq!((b.repo.as_str(), b.branch.as_str()), ("proj-two", "feat/x"));
        assert!((b.cost_usd - 0.00755).abs() < 1e-9);

        let c = &events.usage[2];
        assert_eq!((c.event_id.as_str(), c.effort.as_str()), ("msg_C", ""));
        assert!((c.cost_usd - 0.0035).abs() < 1e-9);
    }

    #[test]
    fn cache_tiers_repo_markers_and_unpriced_models_come_through() {
        let events = ClaudeCodeSource::new(root("tiers"))
            .events_since(0)
            .unwrap();
        let by_id = |id: &str| events.usage.iter().find(|e| e.event_id == id).unwrap();

        // Opus 5.5 is 4/20: 1000*4 + 500*20 + 2000*4*1.25 + 4000*4*2 + 10000*4*0.05.
        let t1 = by_id("msg_T1");
        assert_eq!(
            (t1.cache_creation_tokens, t1.cache_creation_1h_tokens),
            (6000, 4000)
        );
        assert!((t1.cost_usd - 0.058).abs() < 1e-9 && !t1.unpriced);
        assert_eq!(
            t1.repo, "playbook",
            "a .claude/worktrees path names its repo"
        );

        // Opus 4.9 is not in the price table, so it is flagged, not guessed.
        let t2 = by_id("msg_T2");
        assert_eq!((t2.cost_usd, t2.unpriced), (0.0, true));
        assert_eq!(
            t2.repo, "ward",
            "a .git/review-worktrees path names its repo"
        );

        // No tokens means nothing to price, even for an unknown model.
        let t3 = by_id("msg_T3");
        assert_eq!((t3.cost_usd, t3.unpriced), (0.0, false));

        // No tier breakdown: the whole write is the five minute cache.
        let t4 = by_id("msg_T4");
        assert_eq!(t4.cache_creation_1h_tokens, 0);
        assert!((t4.cost_usd - 0.0025).abs() < 1e-9);
    }

    #[test]
    fn watermark_is_inclusive_and_filters_older_events() {
        let source = ClaudeCodeSource::new(root("usage"));

        let at_boundary = source.events_since(1788253200).unwrap();
        let ids: Vec<&str> = at_boundary
            .usage
            .iter()
            .map(|e| e.event_id.as_str())
            .collect();
        assert_eq!(ids, vec!["msg_B", "msg_C"]);
    }

    #[test]
    fn tool_events_extract_skills_and_agents_with_and_without_subagent_type() {
        let events = ClaudeCodeSource::new(root("tools"))
            .events_since(0)
            .unwrap();

        let got: Vec<(&str, &str, &str)> = events
            .tools
            .iter()
            .map(|t| (t.event_id.as_str(), t.kind.as_str(), t.name.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("tu_1", "skill", "playbook:plan"),
                ("tu_2", "skill", "playbook:implement"),
                ("tu_3", "agent", "Explore"),
                ("tu_4", "agent", "Draft briefs"),
            ],
            "tu_4 appears on two lines but is one invocation"
        );
    }

    #[test]
    fn a_transcript_older_than_the_watermark_is_not_read_at_all() {
        let dir = crate::util::scratch_dir("old-transcript");
        let project = dir.join("p");
        fs::create_dir_all(&project).unwrap();
        let file = project.join("s.jsonl");
        fs::copy(root("usage").join("proj-one/s1.jsonl"), &file).unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let source = ClaudeCodeSource::new(dir.clone());

        let skipped = source.events_since(2000).unwrap();
        let read = source.events_since(0).unwrap();

        // The file's events are newer than 2000, but the file itself was
        // last modified at 1000, so it is skipped without being parsed.
        assert!(skipped.usage.is_empty());
        assert_eq!(read.usage.len(), 3);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_root_that_does_not_exist_yields_no_events() {
        let source = ClaudeCodeSource::new(root("no-such-dir"));

        let events = source.events_since(0).unwrap();

        assert!(events.usage.is_empty() && events.tools.is_empty());
    }

    fn message_line(id: &str, cwd: &str, content: &str) -> Vec<u8> {
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":\"2026-09-01T08:51:22.000Z\",\"cwd\":\"{cwd}\",\"gitBranch\":\"b\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-sonnet-5\",\"content\":{content},\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}}}}}}\n"
        )
        .into_bytes()
    }

    fn events_from(name: &str, bytes: &[u8]) -> Events {
        let dir = crate::util::scratch_dir(name);
        fs::create_dir_all(dir.join("p")).unwrap();
        fs::write(dir.join("p/s.jsonl"), bytes).unwrap();
        let events = ClaudeCodeSource::new(dir.clone()).events_since(0).unwrap();
        let _ = fs::remove_dir_all(dir);
        events
    }

    #[test]
    fn a_bad_utf8_byte_costs_only_its_own_line() {
        let mut bytes = message_line("msg_1", "/x/r", "[]");
        // Invalid UTF-8 inside a string value: the line still reads, lossily.
        let mut bad = message_line("msg_bad", "/x/PLACEHOLDER", "[]");
        let at = bad.windows(11).position(|w| w == b"PLACEHOLDER").unwrap();
        bad.splice(at..at + 11, [0xff, 0xfe]);
        bytes.extend(bad);
        // Invalid UTF-8 that breaks the JSON itself: only this line is lost.
        bytes.extend(b"{\"type\":\"assistant\",\xff\n");
        bytes.extend(message_line("msg_2", "/x/r", "[]"));

        let events = events_from("bad-utf8", &bytes);

        let ids: Vec<&str> = events.usage.iter().map(|e| e.event_id.as_str()).collect();
        assert_eq!(ids, vec!["msg_1", "msg_bad", "msg_2"]);
    }

    #[test]
    fn an_oversized_line_is_skipped_and_the_next_line_still_reads() {
        let mut bytes = b"{\"type\":\"assistant\",\"pad\":\"".to_vec();
        bytes.extend(vec![b'A'; MAX_LINE_BYTES + 10]);
        bytes.extend(b"\"}\n");
        bytes.extend(message_line("msg_after", "/x/r", "[]"));

        let events = events_from("huge-line", &bytes);

        let ids: Vec<&str> = events.usage.iter().map(|e| e.event_id.as_str()).collect();
        assert_eq!(ids, vec!["msg_after"]);
    }

    #[test]
    fn the_older_task_tool_name_counts_as_an_agent() {
        let content = r#"[{"type":"tool_use","id":"tu_old","name":"Task","input":{"subagent_type":"Explore","description":"look"}}]"#;

        let events = events_from("task-tool", &message_line("msg_t", "/x/r", content));

        let got: Vec<(&str, &str, &str)> = events
            .tools
            .iter()
            .map(|t| (t.event_id.as_str(), t.kind.as_str(), t.name.as_str()))
            .collect();
        assert_eq!(got, vec![("tu_old", "agent", "Explore")]);
    }

    /// A transcript tree of many files whose message and tool ids repeat across
    /// files, with growing and shrinking output counts and out of order
    /// timestamps, so the dedup rules decide the result.
    fn many_files(name: &str, files: usize) -> PathBuf {
        let dir = crate::util::scratch_dir(name);
        for f in 0..files {
            let sub = dir.join(format!("proj-{}", f % 5));
            fs::create_dir_all(&sub).unwrap();
            let mut text = String::new();
            for l in 0..6 {
                let id = format!("msg_{}", (f * 3 + l) % 11);
                let out = (f * 7 + l * 13) % 50 + 1;
                let sec = (f * 31 + l * 17) % 59;
                let min = (f + l) % 40;
                text.push_str(&format!(
                    "{{\"type\":\"assistant\",\"sessionId\":\"s{f}\",\"timestamp\":\"2026-09-01T08:{min:02}:{sec:02}.000Z\",\"cwd\":\"/x/repo-{}\",\"gitBranch\":\"b\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-sonnet-5\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"tu_{}\",\"name\":\"Skill\",\"input\":{{\"skill\":\"k{l}\"}}}}],\"usage\":{{\"input_tokens\":1,\"output_tokens\":{out}}}}}}}\n",
                    f % 4,
                    (f + l) % 9
                ));
                text.push_str("{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}\n");
            }
            text.push_str("not json at all\n");
            fs::write(sub.join(format!("s{f}.jsonl")), text).unwrap();
        }
        dir
    }

    /// The sequential scan as it was before threads and `Dedup`: parse every
    /// line, apply the dedup rules as lines arrive. The oracle for the tests.
    fn reference_events(root: &Path, watermark: i64) -> Events {
        let mut files = Vec::new();
        collect_jsonl(root, &mut files);
        files.sort();
        files.retain(|f| !modified_before(f, watermark));
        let mut usage: Vec<UsageEvent> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut tools: Vec<ToolInvocationEvent> = Vec::new();
        let mut tool_seen: HashMap<String, ()> = HashMap::new();
        let mut repos = RepoResolver::default();
        for file in files {
            read_lines(&file, &mut |line| {
                let Ok(value) = serde_json::from_str::<Value>(line) else {
                    return;
                };
                if value.get("type").and_then(Value::as_str) != Some("assistant") {
                    return;
                }
                let Some(ts) = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(parse_iso8601_utc)
                else {
                    return;
                };
                if ts < watermark {
                    return;
                }
                if let Some(event) = usage_event(&value, ts, &mut repos) {
                    match index.get(&event.event_id) {
                        Some(&i) if event.output_tokens > usage[i].output_tokens => {
                            let first = usage[i].timestamp.min(event.timestamp);
                            usage[i] = event;
                            usage[i].timestamp = first;
                        }
                        Some(_) => {}
                        None => {
                            index.insert(event.event_id.clone(), usage.len());
                            usage.push(event);
                        }
                    }
                }
                for tool in tool_events(&value, ts) {
                    if tool_seen.insert(tool.event_id.clone(), ()).is_none() {
                        tools.push(tool);
                    }
                }
            });
        }
        Events { usage, tools }
    }

    #[test]
    fn the_threaded_scan_matches_the_plain_sequential_scan() {
        let dir = many_files("usage-oracle", 60);
        let source = ClaudeCodeSource::new(dir.clone());
        for watermark in [0, 1788250500, 1788251200] {
            let expected = reference_events(&dir, watermark);
            assert!(expected.usage.len() > 5 && expected.tools.len() > 5);
            for threads in [1, 2, 3, 4, 7, 8, 100] {
                assert_eq!(
                    source.events_since_with(watermark, threads),
                    expected,
                    "watermark {watermark}, {threads} threads"
                );
            }
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn every_thread_count_gives_the_same_events_as_one_thread() {
        let dir = many_files("usage-threads", 40);
        let source = ClaudeCodeSource::new(dir.clone());
        for watermark in [0, 1788250500, 1788251200] {
            let one = source.events_since_with(watermark, 1);
            assert!(!one.usage.is_empty() && !one.tools.is_empty());
            for threads in [2, 3, 5, 8, 64] {
                assert_eq!(
                    source.events_since_with(watermark, threads),
                    one,
                    "watermark {watermark}, {threads} threads"
                );
            }
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn slices_cover_every_file_once_and_none_is_empty() {
        for sizes in [
            vec![5u64],
            vec![1, 1, 1, 1, 1, 1, 1],
            vec![1000, 1, 1, 1, 1],
            vec![1, 1, 1, 1, 1000],
            vec![3, 9, 2, 7, 4, 8, 1, 6],
        ] {
            for parts in 1..=6 {
                let ranges = byte_balanced_slices(&sizes, parts);
                assert!(ranges.len() <= parts, "{sizes:?} {parts}");
                let mut next = 0;
                for range in &ranges {
                    assert_eq!(range.start, next, "{sizes:?} {parts}");
                    assert!(range.end > range.start, "{sizes:?} {parts}");
                    next = range.end;
                }
                assert_eq!(next, sizes.len(), "{sizes:?} {parts}");
            }
        }
    }

    #[test]
    fn a_line_that_never_says_assistant_is_skipped_without_a_parse() {
        let mut repos = RepoResolver::default();
        let mut seen = Dedup::default();
        assert!(line_record(
            r#"{"type":"user","message":{"id":"m"}}"#,
            0,
            &mut repos,
            &mut seen
        )
        .is_none());
        // Spaced JSON still parses: the prefilter only needs the word.
        let spaced = r#"{ "type" : "assistant", "timestamp" : "2026-09-01T08:51:22.000Z", "message" : { "id" : "m", "usage" : { "output_tokens" : 4 } } }"#;
        assert!(line_record(spaced, 0, &mut repos, &mut seen).is_some());
    }
}
