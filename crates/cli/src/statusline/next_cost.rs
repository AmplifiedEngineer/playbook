// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! What the next message will cost, and whether the prompt cache is still
//! warm. Pure: the caller reads the transcript tail and passes it in.
//!
//! The figure is a lower bound. It prices only the input side of the current
//! context: at the cache read price while the cache is warm, at the cache
//! write price once it has gone cold. The next message also writes the
//! previous reply and the new prompt to the cache and pays for its output,
//! none of which is known here.
//!
//! Cache state, in order of checks: no last turn is `Unknown`; a model switch
//! is `Cold` (a new model starts a new cache); no cache tier found is
//! `Unknown`; otherwise the age of the last turn against the tier's TTL. A
//! model switch is the one case that prices the cold figure at the 5 minute
//! write even when the last tier was one hour, because the new model's tier
//! is not known.

// The estimate is wired into the status line in the next change.
#![allow(dead_code)]

use crate::common::transcript_tail::{CacheTier, LastTurn};
use crate::usage::pricing::{cost_usd, same_model, Tokens};

/// Lifetime of a five minute cache write.
const FIVE_MINUTE_TTL_SECS: i64 = 300;
/// Lifetime of a one hour cache write.
const ONE_HOUR_TTL_SECS: i64 = 3600;
/// Below this the figure is shown as `<$0.01`.
const MIN_SHOWN_USD: f64 = 0.01;
/// Absorbs float error when flooring a price to whole cents.
const CENT_EPSILON: f64 = 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheState {
    Warm { left_secs: i64 },
    Cold,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct NextCost {
    pub warm_usd: f64,
    pub cold_usd: f64,
    pub state: CacheState,
}

/// Estimate for the next message, or `None` for an unpriced model or an empty
/// context. An empty `model_id` falls back to the last turn's model.
pub(super) fn estimate(
    model_id: &str,
    context_tokens: u64,
    last: Option<&LastTurn>,
    now: i64,
) -> Option<NextCost> {
    if context_tokens == 0 {
        return None;
    }
    let model = if model_id.is_empty() {
        last.map_or("", |l| l.model.as_str())
    } else {
        model_id
    };
    // Price first: an unpriced model returns before any state check.
    let warm_usd = cost_usd(
        model,
        &Tokens {
            cache_read: context_tokens,
            ..Tokens::default()
        },
    )?;
    let write_1h = cost_usd(
        model,
        &Tokens {
            cache_write_1h: context_tokens,
            ..Tokens::default()
        },
    )?;
    let cold_5m = cost_usd(
        model,
        &Tokens {
            cache_write_5m: context_tokens,
            ..Tokens::default()
        },
    )?;
    let next = |cold_usd, state| {
        Some(NextCost {
            warm_usd,
            cold_usd,
            state,
        })
    };

    let Some(last) = last else {
        return next(cold_5m, CacheState::Unknown);
    };
    if !same_model(model, &last.model) {
        return next(cold_5m, CacheState::Cold);
    }
    let Some(tier) = last.tier else {
        return next(cold_5m, CacheState::Unknown);
    };
    let (ttl, cold_usd) = match tier {
        CacheTier::FiveMinutes => (FIVE_MINUTE_TTL_SECS, cold_5m),
        CacheTier::OneHour => (ONE_HOUR_TTL_SECS, write_1h),
    };
    let age = (now - last.at).max(0);
    let state = if age >= ttl {
        CacheState::Cold
    } else {
        CacheState::Warm {
            left_secs: ttl - age,
        }
    };
    next(cold_usd, state)
}

/// `<$0.01` below one cent, else `≥$` and the price floored to whole cents.
/// The figure is a lower bound, so it never rounds up.
pub(super) fn fmt_next(usd: f64) -> String {
    if usd < MIN_SHOWN_USD {
        return "<$0.01".to_string();
    }
    let cents = (usd * 100.0 + CENT_EPSILON).floor();
    format!("≥${:.2}", cents / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const OPUS: &str = "claude-opus-4-8";
    const CTX: u64 = 200_000;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn last(model: &str, tier: Option<CacheTier>, age: i64) -> LastTurn {
        LastTurn {
            at: NOW - age,
            model: model.to_string(),
            tier,
        }
    }

    struct Row {
        name: &'static str,
        model_id: &'static str,
        tokens: u64,
        last: Option<LastTurn>,
        state: CacheState,
        warm: f64,
        cold: f64,
    }

    fn warm(left_secs: i64) -> CacheState {
        CacheState::Warm { left_secs }
    }

    #[test]
    fn state_and_prices_by_case() {
        use CacheTier::{FiveMinutes as M5, OneHour as H1};
        let five = Some(M5);
        let rows = [
            // Opus 4.8 is $5 input, reads at 0.1x: 200,000 x $5 x 0.1 / 1M = $0.10.
            // Cold at the 5m write: x 1.25 = $1.25. At the 1h write: x 2 = $2.00.
            Row {
                name: "age zero",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last(OPUS, five, 0)),
                state: warm(300),
                warm: 0.10,
                cold: 1.25,
            },
            Row {
                name: "age 299 is warm",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last(OPUS, five, 299)),
                state: warm(1),
                warm: 0.10,
                cold: 1.25,
            },
            Row {
                name: "age 300 is cold",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last(OPUS, five, 300)),
                state: CacheState::Cold,
                warm: 0.10,
                cold: 1.25,
            },
            Row {
                name: "1h age 3599 is warm",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last(OPUS, Some(H1), 3599)),
                state: warm(1),
                warm: 0.10,
                cold: 2.00,
            },
            Row {
                name: "1h age 3600 is cold",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last(OPUS, Some(H1), 3600)),
                state: CacheState::Cold,
                warm: 0.10,
                cold: 2.00,
            },
            Row {
                name: "no tier, age 10",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last(OPUS, None, 10)),
                state: CacheState::Unknown,
                warm: 0.10,
                cold: 1.25,
            },
            Row {
                name: "no tier, age 4000",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last(OPUS, None, 4000)),
                state: CacheState::Unknown,
                warm: 0.10,
                cold: 1.25,
            },
            Row {
                name: "no last turn",
                model_id: OPUS,
                tokens: CTX,
                last: None,
                state: CacheState::Unknown,
                warm: 0.10,
                cold: 1.25,
            },
            Row {
                name: "timestamp ahead of the clock",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last(OPUS, five, -60)),
                state: warm(300),
                warm: 0.10,
                cold: 1.25,
            },
            // Opus 5.5 is $4 input, reads at 0.05x: 200,000 x $4 x 0.05 / 1M = $0.04; write 200,000 x $4 x 1.25 / 1M = $1.00.
            Row {
                name: "opus 5.5",
                model_id: "claude-opus-5-5",
                tokens: CTX,
                last: Some(last("claude-opus-5-5", five, 10)),
                state: warm(290),
                warm: 0.04,
                cold: 1.00,
            },
            Row {
                name: "1m suffix is ignored",
                model_id: "claude-opus-4-8[1m]",
                tokens: CTX,
                last: Some(last(OPUS, five, 10)),
                state: warm(290),
                warm: 0.10,
                cold: 1.25,
            },
            // Opus 4.1 is $15 input: 200,000 x $15 x 0.1 / 1M = $0.30; write x 1.25 = $3.75.
            Row {
                name: "date suffix is ignored",
                model_id: "claude-opus-4-1",
                tokens: CTX,
                last: Some(last("claude-opus-4-1-20250805", five, 10)),
                state: warm(290),
                warm: 0.30,
                cold: 3.75,
            },
            // Sonnet 5 is $2 input: warm 200,000 x $2 x 0.1 / 1M = $0.04; cold at the 5m write x 1.25 = $0.50.
            Row {
                name: "model switch with no tier",
                model_id: "claude-sonnet-5",
                tokens: CTX,
                last: Some(last(OPUS, None, 10)),
                state: CacheState::Cold,
                warm: 0.04,
                cold: 0.50,
            },
            Row {
                name: "model switch from a 1h tier",
                model_id: "claude-sonnet-5",
                tokens: CTX,
                last: Some(last(OPUS, Some(H1), 10)),
                state: CacheState::Cold,
                warm: 0.04,
                cold: 0.50,
            },
            Row {
                name: "bedrock prefix on the transcript side",
                model_id: OPUS,
                tokens: CTX,
                last: Some(last("us.anthropic.claude-opus-4-8-v1:0", five, 10)),
                state: warm(290),
                warm: 0.10,
                cold: 1.25,
            },
            Row {
                name: "stdin id empty falls back to the transcript model",
                model_id: "",
                tokens: CTX,
                last: Some(last(OPUS, five, 10)),
                state: warm(290),
                warm: 0.10,
                cold: 1.25,
            },
            // Haiku 5.5 over 100,000 tokens pays 5x: 150,000 x $0.10 x 5 x 0.1 / 1M = $0.0075; write x 1.25 = $0.09375.
            Row {
                name: "haiku long prompt factor",
                model_id: "claude-haiku-5-5",
                tokens: 150_000,
                last: None,
                state: CacheState::Unknown,
                warm: 0.0075,
                cold: 0.09375,
            },
        ];
        for row in rows {
            let got = estimate(row.model_id, row.tokens, row.last.as_ref(), NOW)
                .unwrap_or_else(|| panic!("{}: no estimate", row.name));
            assert_eq!(got.state, row.state, "{}", row.name);
            assert!(
                close(got.warm_usd, row.warm),
                "{}: warm {}",
                row.name,
                got.warm_usd
            );
            assert!(
                close(got.cold_usd, row.cold),
                "{}: cold {}",
                row.name,
                got.cold_usd
            );
        }
    }

    #[test]
    fn an_unpriced_model_or_zero_tokens_gives_none() {
        let l = last("claude-opus-4-9", Some(CacheTier::FiveMinutes), 10);
        assert_eq!(estimate("claude-opus-4-9", CTX, Some(&l), NOW), None);
        assert_eq!(estimate("", CTX, None, NOW), None);
        let l = last(OPUS, Some(CacheTier::FiveMinutes), 10);
        assert_eq!(estimate(OPUS, 0, Some(&l), NOW), None);
        // The price table matches plain names only, so a prefixed stdin id is
        // unpriced and shows no estimate.
        assert_eq!(
            estimate("us.anthropic.claude-opus-4-8-v1:0", CTX, Some(&l), NOW),
            None
        );
    }

    #[test]
    fn an_unpriced_model_returns_none_before_any_switch_check() {
        let l = last(OPUS, Some(CacheTier::FiveMinutes), 10);
        assert_eq!(estimate("claude-opus-4-9", CTX, Some(&l), NOW), None);
    }

    #[test]
    fn fmt_next_floors_to_the_cent() {
        for (usd, want) in [
            (0.0099, "<$0.01"),
            (0.0, "<$0.01"),
            (0.01, "≥$0.01"),
            (0.0999, "≥$0.09"),
            (1.254, "≥$1.25"),
            (1.259, "≥$1.25"),
            (0.29, "≥$0.29"),
            (1.25, "≥$1.25"),
        ] {
            assert_eq!(fmt_next(usd), want, "{usd}");
        }
    }
}
