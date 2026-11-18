// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The per-model price table, read from LiteLLM's public price list
//! (<https://github.com/BerriAI/litellm>, MIT licensed). A trimmed copy of that
//! list is checked in as `data/litellm-prices.json` so prices work offline, and
//! a newer copy can replace it at run time (see `price_refresh`). Both have the
//! LiteLLM shape: one object per model, keyed by the bare model id, with prices
//! in USD per token.
//!
//! Only the fields this crate prices with are kept, and only entries from the
//! providers that make the models coding agents run (`PROVIDERS`). Cloud
//! reseller entries (`bedrock/...`, `azure/...`) carry the same prices under
//! longer keys and are dropped, so a lookup always lands on the bare key.
//!
//! Regenerate the checked-in copy from a full download with
//! `LITELLM_FULL_JSON=<file> cargo test -p playbook-telemetry --lib regenerate_snapshot -- --ignored`.

use crate::pricing::Tokens;
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::BTreeMap;

/// The checked-in snapshot.
pub const SNAPSHOT: &str = include_str!("../data/litellm-prices.json");

/// Where the full list is published.
pub const SOURCE_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

const PROVIDERS: [&str; 2] = ["anthropic", "openai"];
const PROVIDER_FIELD: &str = "litellm_provider";
/// Top level number: when the list was taken, in epoch seconds. Not a model.
pub const EPOCH_FIELD: &str = "snapshot_epoch";

const INPUT: &str = "input_cost_per_token";
const OUTPUT: &str = "output_cost_per_token";
const CACHE_WRITE: &str = "cache_creation_input_token_cost";
const CACHE_WRITE_1H: &str = "cache_creation_input_token_cost_above_1hr";
const CACHE_READ: &str = "cache_read_input_token_cost";
const PRICE_FIELDS: [&str; 5] = [INPUT, OUTPUT, CACHE_WRITE, CACHE_WRITE_1H, CACHE_READ];

/// A long prompt tier is a field of the form `<price field>_above_<N>k_tokens`.
const TIER_INFIX: &str = "_above_";
const TIER_SUFFIX: &str = "k_tokens";
const TOKENS_PER_K: u64 = 1000;

/// What a cache write costs against the input price when the list gives none.
pub const CACHE_WRITE_5M_MULTIPLIER: f64 = 1.25;
pub const CACHE_WRITE_1H_MULTIPLIER: f64 = 2.0;

const DATE_SUFFIX_LEN: usize = 8;
const MIN_ENTRIES: usize = 10;

/// USD per token.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

impl Rates {
    /// Rates from per million token prices, with the cache write prices taken
    /// from the standard multipliers and cache reads as a fraction of input.
    pub fn from_per_million(input: f64, output: f64, read_fraction: f64) -> Rates {
        let per_token = |usd: f64| usd / 1_000_000.0;
        let input = per_token(input);
        Rates {
            input,
            output: per_token(output),
            cache_write_5m: input * CACHE_WRITE_5M_MULTIPLIER,
            cache_write_1h: input * CACHE_WRITE_1H_MULTIPLIER,
            cache_read: input * read_fraction,
        }
    }

    fn cost(&self, tokens: &Tokens) -> f64 {
        tokens.input as f64 * self.input
            + tokens.output as f64 * self.output
            + tokens.cache_write_5m as f64 * self.cache_write_5m
            + tokens.cache_write_1h as f64 * self.cache_write_1h
            + tokens.cache_read as f64 * self.cache_read
    }
}

/// One model's prices: the base rates and, when the list has one, the rates
/// that replace them once the prompt is longer than a token threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub base: Rates,
    pub long_prompt: Option<(u64, Rates)>,
}

impl Entry {
    pub fn flat(base: Rates) -> Entry {
        Entry {
            base,
            long_prompt: None,
        }
    }

    /// Dollar cost of one message. The prompt is everything sent in: input and
    /// both cache write kinds and cache reads.
    pub fn cost(&self, tokens: &Tokens) -> f64 {
        let prompt =
            tokens.input + tokens.cache_write_5m + tokens.cache_write_1h + tokens.cache_read;
        match self.long_prompt {
            Some((threshold, long)) if prompt > threshold => long.cost(tokens),
            _ => self.base.cost(tokens),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PriceTable {
    entries: BTreeMap<String, Entry>,
    /// When the list was taken, epoch seconds; 0 when unknown.
    pub snapshot_epoch: i64,
}

fn number(map: &Map<String, Value>, field: &str) -> Option<f64> {
    map.get(field)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
}

/// The token threshold of `field` when it is `base` with a long prompt tier.
fn tier_threshold(field: &str, base: &str) -> Option<u64> {
    let kilo = field
        .strip_prefix(base)?
        .strip_prefix(TIER_INFIX)?
        .strip_suffix(TIER_SUFFIX)?;
    kilo.parse::<u64>().ok()?.checked_mul(TOKENS_PER_K)
}

fn is_kept_field(field: &str) -> bool {
    field == PROVIDER_FIELD
        || PRICE_FIELDS
            .iter()
            .any(|base| field == *base || tier_threshold(field, base).is_some())
}

/// The rates for one tier: `suffix` is empty for the base prices. A price the
/// list omits is derived from the input price. `read_fraction` is the share of
/// the input price a cache read costs when the list gives no read price.
fn rates(map: &Map<String, Value>, suffix: &str, read_fraction: f64) -> Option<Rates> {
    let price = |base: &str| number(map, &format!("{base}{suffix}"));
    let input = price(INPUT)?;
    Some(Rates {
        input,
        output: price(OUTPUT)?,
        cache_write_5m: price(CACHE_WRITE).unwrap_or(input * CACHE_WRITE_5M_MULTIPLIER),
        cache_write_1h: price(CACHE_WRITE_1H).unwrap_or(input * CACHE_WRITE_1H_MULTIPLIER),
        cache_read: price(CACHE_READ).unwrap_or(input * read_fraction),
    })
}

fn entry(map: &Map<String, Value>) -> Option<Entry> {
    let base = rates(map, "", 1.0)?;
    let threshold = map
        .keys()
        .filter_map(|field| tier_threshold(field, INPUT))
        .min();
    let long_prompt = threshold.and_then(|n| {
        let suffix = format!("{TIER_INFIX}{}{TIER_SUFFIX}", n / TOKENS_PER_K);
        let fraction = if base.input > 0.0 {
            base.cache_read / base.input
        } else {
            1.0
        };
        rates(map, &suffix, fraction).map(|r| (n, r))
    });
    Some(Entry { base, long_prompt })
}

fn accepted(key: &str, map: &Map<String, Value>) -> bool {
    !key.contains('/')
        && map
            .get(PROVIDER_FIELD)
            .and_then(Value::as_str)
            .is_some_and(|p| PROVIDERS.contains(&p))
}

/// Drops a trailing `-YYYYMMDD` or `-YYYY-MM-DD`.
fn strip_date(name: &str) -> &str {
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    if let Some((head, tail)) = name.rsplit_once('-') {
        if digits(tail, DATE_SUFFIX_LEN) {
            return head;
        }
    }
    let mut parts = name.rsplitn(4, '-');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(day), Some(month), Some(year), Some(head))
            if digits(day, 2) && digits(month, 2) && digits(year, 4) =>
        {
            head
        }
        _ => name,
    }
}

/// The lower case model id without a `[1m]` or `@date` suffix. A cloud
/// provider's own id (`us.anthropic.claude-...-v1:0`) is not a bare key and
/// stays unpriced, as it always has. A plain lower case id, the usual case
/// when every stored message is priced, is not copied.
fn bare(model: &str) -> Cow<'_, str> {
    let end = model.find(['[', '@', ':']).unwrap_or(model.len());
    let name = &model[..end];
    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(name.to_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

impl PriceTable {
    /// Reads a LiteLLM shaped list: the full download, a trimmed copy or a
    /// cached one. Entries from other providers and entries without an input
    /// and an output price are skipped. An error means the text is not a list.
    pub fn parse(text: &str) -> Result<PriceTable, String> {
        let root: Value =
            serde_json::from_str(text).map_err(|e| format!("price list is not JSON: {e}"))?;
        let object = root.as_object().ok_or("price list is not an object")?;
        let entries = object
            .iter()
            .filter_map(|(key, value)| {
                let map = value.as_object().filter(|m| accepted(key, m))?;
                Some((key.to_lowercase(), entry(map)?))
            })
            .collect();
        Ok(PriceTable {
            entries,
            snapshot_epoch: object.get(EPOCH_FIELD).and_then(Value::as_i64).unwrap_or(0),
        })
    }

    pub fn embedded() -> PriceTable {
        PriceTable::parse(SNAPSHOT).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A list worth replacing the snapshot with: it has many models, so a
    /// truncated or wrong download is not taken for one.
    pub fn is_plausible(&self) -> bool {
        self.len() >= MIN_ENTRIES
    }

    /// The entry for `model`: the exact id first, then without its date.
    pub fn lookup(&self, model: &str) -> Option<&Entry> {
        let name = bare(model);
        self.entries
            .get(name.as_ref())
            .or_else(|| self.entries.get(strip_date(&name)))
    }

    pub fn cost_usd(&self, model: &str, tokens: &Tokens) -> Option<f64> {
        self.lookup(model).map(|e| e.cost(tokens))
    }
}

/// The text of a trimmed list for the full download `text`, stamped with
/// `epoch`: one model per line, sorted, so a refreshed snapshot diffs cleanly.
pub fn trim(text: &str, epoch: i64) -> Result<String, String> {
    let root: Value =
        serde_json::from_str(text).map_err(|e| format!("price list is not JSON: {e}"))?;
    let object = root.as_object().ok_or("price list is not an object")?;
    let kept: BTreeMap<&str, Map<String, Value>> = object
        .iter()
        .filter_map(|(key, value)| {
            let map = value.as_object().filter(|m| accepted(key, m))?;
            entry(map)?;
            let slim = map
                .iter()
                .filter(|(field, _)| is_kept_field(field))
                .map(|(field, v)| (field.clone(), v.clone()))
                .collect();
            Some((key.as_str(), slim))
        })
        .collect();
    let mut lines = vec![format!("\"{EPOCH_FIELD}\":{epoch}")];
    for (key, map) in &kept {
        let body = serde_json::to_string(map).map_err(|e| e.to_string())?;
        lines.push(format!("{}:{body}", Value::from(*key)));
    }
    Ok(format!("{{\n{}\n}}\n", lines.join(",\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn per_million(model: &str, tokens: Tokens) -> f64 {
        PriceTable::embedded().cost_usd(model, &tokens).unwrap()
    }

    const SAMPLE: &str = r#"{
        "sample_spec": {"input_cost_per_token": 1},
        "claude-demo-1": {"litellm_provider": "anthropic", "input_cost_per_token": 1e-6,
            "output_cost_per_token": 5e-6, "cache_read_input_token_cost": 1e-7,
            "supports_vision": true, "input_cost_per_token_batches": 5e-7},
        "claude-demo-1-20250101": {"litellm_provider": "anthropic", "input_cost_per_token": 9e-6,
            "output_cost_per_token": 9e-6},
        "bedrock/claude-demo-1": {"litellm_provider": "anthropic", "input_cost_per_token": 7e-6,
            "output_cost_per_token": 7e-6},
        "us.anthropic.claude-demo-1-v1:0": {"litellm_provider": "bedrock",
            "input_cost_per_token": 7e-6, "output_cost_per_token": 7e-6},
        "no-output": {"litellm_provider": "openai", "input_cost_per_token": 1e-6},
        "gpt-demo": {"litellm_provider": "openai", "input_cost_per_token": 2e-6,
            "output_cost_per_token": 1e-5, "input_cost_per_token_above_272k_tokens": 4e-6,
            "output_cost_per_token_above_272k_tokens": 1.5e-5,
            "cache_read_input_token_cost": 2e-7, "cache_read_input_token_cost_above_272k_tokens": 4e-7}
    }"#;

    fn sample() -> PriceTable {
        PriceTable::parse(SAMPLE).unwrap()
    }

    #[test]
    fn the_embedded_snapshot_parses_and_prices_the_models_we_rely_on() {
        let table = PriceTable::embedded();

        assert!(table.is_plausible(), "{} entries", table.len());
        assert!(table.snapshot_epoch > 0);
        let input = Tokens {
            input: 1_000_000,
            ..Tokens::default()
        };
        let output = Tokens {
            output: 1_000_000,
            ..Tokens::default()
        };
        for (model, in_price, out_price) in [
            ("claude-opus-5-5", 4.0, 20.0),
            ("claude-sonnet-5-5", 2.0, 10.0),
            ("claude-opus-4-8", 5.0, 25.0),
            ("claude-sonnet-4-6", 3.0, 15.0),
        ] {
            assert!(close(per_million(model, input), in_price), "{model}");
            assert!(close(per_million(model, output), out_price), "{model}");
        }
    }

    #[test]
    fn the_snapshot_prices_models_the_hand_table_never_had() {
        for model in [
            "claude-fable-5-2",
            "gpt-6-luna",
            "gpt-6.1-sol",
            "claude-haiku-5-5",
        ] {
            let tokens = Tokens {
                input: 1000,
                output: 1000,
                ..Tokens::default()
            };
            let cost = PriceTable::embedded().cost_usd(model, &tokens);
            assert!(cost.is_some_and(|c| c > 0.0), "{model}");
        }
    }

    #[test]
    fn a_model_with_no_entry_is_unpriced_and_neighbours_do_not_lend_prices() {
        let table = PriceTable::embedded();
        let t = Tokens {
            input: 1,
            ..Tokens::default()
        };

        assert_eq!(table.cost_usd("codex-auto-review", &t), None);
        assert_eq!(table.cost_usd("<synthetic>", &t), None);
        assert_eq!(table.cost_usd("claude-opus-4-99", &t), None);
    }

    #[test]
    fn only_the_wanted_providers_and_bare_keys_are_read() {
        let table = sample();

        assert!(table.lookup("claude-demo-1").is_some());
        assert!(table.lookup("gpt-demo").is_some());
        assert!(table.lookup("no-output").is_none());
        assert!(table.lookup("sample_spec").is_none());
        let t = Tokens {
            input: 1_000_000,
            ..Tokens::default()
        };
        // The bare key wins over the provider prefixed one.
        assert!(close(table.cost_usd("claude-demo-1", &t).unwrap(), 1.0));
    }

    #[test]
    fn a_lookup_ignores_case_suffixes_and_dates() {
        let table = sample();
        let t = Tokens {
            input: 1_000_000,
            ..Tokens::default()
        };
        for model in [
            "CLAUDE-DEMO-1",
            "claude-demo-1[1m]",
            "claude-demo-1@20250101",
        ] {
            assert!(close(table.cost_usd(model, &t).unwrap(), 1.0), "{model}");
        }
        // A dated id that has its own key uses that key's price.
        assert!(close(
            table.cost_usd("claude-demo-1-20250101", &t).unwrap(),
            9.0
        ));
        assert!(table.lookup("claude-demo-1-20260101").is_some());
        assert!(table.lookup("us.anthropic.claude-demo-1-v1:0").is_none());
        assert_eq!(strip_date("gpt-5-2025-08-07"), "gpt-5");
        assert_eq!(strip_date("claude-opus-4-1"), "claude-opus-4-1");
    }

    #[test]
    fn cache_prices_fall_back_to_the_standard_multipliers() {
        let table = sample();
        let five = Tokens {
            cache_write_5m: 1_000_000,
            ..Tokens::default()
        };
        let hour = Tokens {
            cache_write_1h: 1_000_000,
            ..Tokens::default()
        };
        let read = Tokens {
            cache_read: 1_000_000,
            ..Tokens::default()
        };

        assert!(close(table.cost_usd("claude-demo-1", &five).unwrap(), 1.25));
        assert!(close(table.cost_usd("claude-demo-1", &hour).unwrap(), 2.0));
        assert!(close(table.cost_usd("claude-demo-1", &read).unwrap(), 0.1));
    }

    #[test]
    fn a_long_prompt_tier_replaces_the_base_rates_above_its_threshold() {
        let table = sample();
        let short = Tokens {
            input: 272_000,
            output: 1000,
            ..Tokens::default()
        };
        let long = Tokens {
            input: 272_000,
            cache_read: 1,
            output: 1000,
            ..Tokens::default()
        };

        assert!(close(
            table.cost_usd("gpt-demo", &short).unwrap(),
            272_000.0 * 2e-6 + 1000.0 * 1e-5
        ));
        assert!(close(
            table.cost_usd("gpt-demo", &long).unwrap(),
            272_000.0 * 4e-6 + 1000.0 * 1.5e-5 + 4e-7
        ));
    }

    #[test]
    fn the_snapshot_keeps_haiku_five_five_long_prompt_tier() {
        let long = Tokens {
            input: 1_000_000,
            output: 1_000_000,
            ..Tokens::default()
        };
        let small = Tokens {
            input: 100_000,
            output: 1_000_000,
            ..Tokens::default()
        };

        assert!(close(per_million("claude-haiku-5-5", small), 0.01 + 0.50));
        assert!(close(per_million("claude-haiku-5-5", long), 0.50 + 2.50));
    }

    #[test]
    fn trimming_keeps_price_fields_and_drops_the_rest() {
        let text = trim(SAMPLE, 42).unwrap();
        let table = PriceTable::parse(&text).unwrap();

        assert_eq!(table.snapshot_epoch, 42);
        assert_eq!(table.len(), 3);
        assert!(!text.contains("supports_vision"));
        assert!(!text.contains("_batches"));
        assert!(text.contains("input_cost_per_token_above_272k_tokens"));
        assert_eq!(table.lookup("gpt-demo"), sample().lookup("gpt-demo"));
        assert_eq!(trim(&text, 42).unwrap(), text, "trimming is idempotent");
    }

    #[test]
    fn text_that_is_not_a_price_list_is_an_error() {
        assert!(PriceTable::parse("[]").is_err());
        assert!(PriceTable::parse("nope").is_err());
        assert!(trim("[]", 0).is_err());
        assert!(!PriceTable::parse("{}").unwrap().is_plausible());
    }

    #[test]
    #[ignore = "regenerates data/litellm-prices.json from the file named by LITELLM_FULL_JSON"]
    fn regenerate_snapshot() {
        let from = std::env::var("LITELLM_FULL_JSON").expect("set LITELLM_FULL_JSON");
        let full = std::fs::read_to_string(from).unwrap();
        let now = crate::util::time::now_secs();
        let out = trim(&full, now).unwrap();
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/litellm-prices.json");
        std::fs::write(path, out).unwrap();
    }
}
