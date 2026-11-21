//! Actual-cost billing for the Battle Proxy — the pure half.
//!
//! Usage-priced upstreams report what a call really cost in the
//! response (Monid `cost.value`, OpenRouter `usage.cost`, Exa
//! `costDollars.total`). A service whose proxy spec carries a
//! `billing` block (`models::proxy::BillingConfig`) is charged that
//! amount instead of the flat `cost_per_1k_cents / 1000`.
//!
//! This module only parses and decides; persistence (reservation,
//! settlement, dedupe via the `proxy_charges` unique index) lives in
//! `proxy_service`. All money here is integer **micro-dollars**
//! (1 USD = 1_000_000 µ$, 1¢ = 10_000 µ$) so sub-cent prices are exact.

use serde_json::Value;

use crate::models::proxy::{BillingConfig, BillingSource, CostUnit};

/// Micro-dollars per cent.
pub const MICRO_PER_CENT: i64 = 10_000;
/// Micro-dollars per dollar.
pub const MICRO_PER_USD: i64 = 1_000_000;
/// Sanity ceiling for one upstream-reported cost: $100. Anything above
/// it is treated as garbage (a misparsed field, an echoed caller value)
/// and billed at the flat fallback instead — without it, a huge number
/// with no `max_per_request_cents` saturated to `i64::MAX` and blew
/// every spend sum.
pub const MAX_REPORTED_MICRO: i64 = 100 * MICRO_PER_USD;

/// Flat per-request charge implied by `cost_per_1k_cents`, in µ$.
/// `cost_per_1k_cents / 1000` cents = `cost_per_1k_cents * 10` µ$ — exact.
pub fn flat_micro_per_request(cost_per_1k_cents: i32) -> i64 {
    i64::from(cost_per_1k_cents.max(0)) * 10
}

/// Cents (possibly fractional) → µ$, rounded to the nearest µ$.
pub fn cents_to_micro(cents: f64) -> i64 {
    (cents * MICRO_PER_CENT as f64).round() as i64
}

pub fn micro_to_cents(micro: i64) -> f64 {
    micro as f64 / MICRO_PER_CENT as f64
}

pub fn micro_to_usd(micro: i64) -> f64 {
    micro as f64 / MICRO_PER_USD as f64
}

/// Amount held against the budget while the upstream call is in
/// flight: the per-request cap when set (the most this call can be
/// charged), else the flat rate.
pub fn reserve_micro(billing: &BillingConfig, flat_micro: i64) -> i64 {
    match billing.max_per_request_cents {
        Some(cap) => cents_to_micro(cap),
        None => flat_micro,
    }
}

/// Resolve a dot-separated path (`usage.cost`, `choices.0.x`) in a
/// JSON value. Numeric segments index arrays.
pub fn json_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for seg in path.split('.') {
        cur = match cur {
            Value::Object(map) => map.get(seg)?,
            Value::Array(items) => items.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// True when the upstream `Content-Type` is `text/event-stream`.
pub fn is_event_stream(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|ct| ct.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
}

/// Parse a response body into the JSON documents to search, most
/// relevant first. A plain JSON body yields one document. A streamed
/// body yields every `data:` chunk that parses, **last first** —
/// OpenRouter puts `usage` in the final chunk — but only when the
/// upstream declared `text/event-stream` (`sse`): a plain-text body
/// that echoes caller input must not be able to smuggle in a
/// `data: {"cost": …}` line.
pub fn body_documents(body: &[u8], sse: bool) -> Vec<Value> {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        return vec![v];
    }
    if !sse {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return Vec::new();
    };
    let mut docs: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim)
        .filter(|d| !d.is_empty() && *d != "[DONE]")
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .collect();
    docs.reverse();
    docs
}

/// First value any of `paths` resolves to, searching documents in order.
fn first_hit<'a>(docs: &'a [Value], paths: &[&str]) -> Option<&'a Value> {
    docs.iter().find_map(|d| {
        paths
            .iter()
            .find_map(|p| json_path(d, p).filter(|v| !v.is_null()))
    })
}

/// A JSON number or numeric string → finite, non-negative f64.
fn as_amount(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s
            .trim()
            .trim_start_matches('$')
            .trim()
            .parse::<f64>()
            .ok()?,
        _ => return None,
    };
    (n.is_finite() && n >= 0.0).then_some(n)
}

/// Reported amount → µ$, or `None` above [`MAX_REPORTED_MICRO`].
fn to_micro(amount: f64, unit: CostUnit) -> Option<i64> {
    let micro = match unit {
        CostUnit::Usd => amount * MICRO_PER_USD as f64,
        CostUnit::Cents => amount * MICRO_PER_CENT as f64,
        CostUnit::MicroUsd => amount,
    }
    .round();
    (micro.is_finite() && micro <= MAX_REPORTED_MICRO as f64).then_some(micro as i64)
}

fn key_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// What the upstream response said about cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extraction {
    /// Reported cost in µ$, or `None` when missing / unparseable /
    /// in the wrong currency.
    pub cost_micro: Option<i64>,
    pub dedupe_key: Option<String>,
    /// Why `cost_micro` is `None` (for logs).
    pub miss_reason: Option<String>,
}

/// Pull the reported cost and dedupe key out of a response.
/// `header_value` is the value of `billing.header` (captured before
/// the response body was consumed); `content_type` is the upstream's
/// `Content-Type` (SSE parsing only for `text/event-stream`); `body` is
/// the full buffered body — parse BEFORE the 100 KB log truncation.
pub fn extract(
    billing: &BillingConfig,
    header_value: Option<&str>,
    content_type: Option<&str>,
    body: &[u8],
) -> Extraction {
    let needs_body = billing.source == BillingSource::ResponseJson
        || billing.dedupe_key_path.is_some()
        || billing.currency_path.is_some();
    let docs = if needs_body {
        body_documents(body, is_event_stream(content_type))
    } else {
        Vec::new()
    };

    let dedupe_key = billing
        .dedupe_key_path
        .as_ref()
        .and_then(|p| first_hit(&docs, &p.paths()))
        .and_then(key_string);

    let miss = |reason: String| Extraction {
        cost_micro: None,
        dedupe_key: dedupe_key.clone(),
        miss_reason: Some(reason),
    };

    let (amount, unit) = match billing.source {
        BillingSource::ResponseJson => {
            let entries = billing
                .cost_path
                .as_ref()
                .map(|p| p.entries())
                .unwrap_or_default();
            if docs.is_empty() {
                return miss("response body is not JSON".into());
            }
            // First document × entry (in priority order) that resolves
            // to a non-null value; its unit overrides the block's.
            let hit = docs.iter().find_map(|d| {
                entries.iter().find_map(|(p, u)| {
                    json_path(d, p)
                        .filter(|v| !v.is_null())
                        .map(|v| (v, u.unwrap_or(billing.unit)))
                })
            });
            match hit {
                None => {
                    let names: Vec<&str> = entries.iter().map(|(p, _)| *p).collect();
                    return miss(format!("no cost at {}", names.join(" | ")));
                }
                Some((v, unit)) => match as_amount(v) {
                    Some(a) => (a, unit),
                    None => return miss(format!("cost is not a non-negative number: {v}")),
                },
            }
        }
        BillingSource::ResponseHeader => match header_value {
            None => return miss("cost header absent".into()),
            Some(h) => match as_amount(&Value::String(h.to_string())) {
                Some(a) => (a, billing.unit),
                None => return miss(format!("cost header is not a number: {h:?}")),
            },
        },
    };

    if let Some(cp) = &billing.currency_path
        && let Some(Value::String(code)) = first_hit(&docs, &[cp.as_str()])
        && !code.eq_ignore_ascii_case(&billing.currency)
    {
        return miss(format!(
            "response currency {code} != configured {}",
            billing.currency
        ));
    }

    match to_micro(amount, unit) {
        Some(micro) => Extraction {
            cost_micro: Some(micro),
            dedupe_key,
            miss_reason: None,
        },
        None => miss(format!(
            "reported cost {amount} {unit:?} exceeds the $100/request sanity ceiling"
        )),
    }
}

/// How a settled charge was arrived at. Stored in `proxy_charges.source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeSource {
    Actual,
    Capped,
    Fallback,
    /// Dedupe key already billed for this battle+service — charged
    /// only the increase over what that key was already billed (0 for
    /// a poll repeating the same cost).
    Duplicate,
    /// Service without a `billing` block: the flat
    /// `cost_per_1k_cents / 1000` (migration 079).
    Flat,
    /// The request reached the upstream but never completed normally
    /// (client hung up / handler cancelled / upstream timeout / body
    /// over the size limit): charged the per-request cap, else the
    /// flat rate (migration 079).
    Abandoned,
}

impl ChargeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ChargeSource::Actual => "actual",
            ChargeSource::Capped => "capped",
            ChargeSource::Fallback => "fallback",
            ChargeSource::Duplicate => "duplicate",
            ChargeSource::Flat => "flat",
            ChargeSource::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChargeDecision {
    pub amount_micro: i64,
    pub source: ChargeSource,
    /// Claimed only when a positive cost was reported: a run that is
    /// still PENDING (no cost yet) must not block its later COMPLETED
    /// poll from being billed, and a $0 response has nothing to dedupe.
    pub dedupe_key: Option<String>,
    /// Uncapped reported cost when `source == Capped`.
    pub reported_micro: Option<i64>,
}

/// Charge for a request that was sent upstream but never completed
/// normally: the most it could have cost — `max_per_request_cents`
/// when the service has one, else the flat rate. Never 0 unless the
/// service itself is free.
pub fn abandoned_micro(billing: Option<&BillingConfig>, flat_micro: i64) -> i64 {
    match billing {
        Some(b) => reserve_micro(b, flat_micro),
        None => flat_micro,
    }
}

/// Settlement for a flat-billed service (no `billing` block).
pub fn flat_decision(flat_micro: i64) -> ChargeDecision {
    ChargeDecision {
        amount_micro: flat_micro,
        source: ChargeSource::Flat,
        dedupe_key: None,
        reported_micro: None,
    }
}

/// Settlement for an abandoned request (see [`ChargeSource::Abandoned`]).
pub fn abandoned_decision(amount_micro: i64) -> ChargeDecision {
    ChargeDecision {
        amount_micro,
        source: ChargeSource::Abandoned,
        dedupe_key: None,
        reported_micro: None,
    }
}

/// What a request whose dedupe key was already billed `billed_micro`
/// owes: the increase only. A Monid run polled while still cheap and
/// again at its larger final cost is billed the max seen, not the
/// first positive value.
pub fn dedupe_delta(amount_micro: i64, billed_micro: i64) -> i64 {
    amount_micro.saturating_sub(billed_micro).max(0)
}

/// Turn an extraction into the amount to charge: actual cost clamped
/// to `max_per_request_cents`, or the flat rate when no usable cost
/// was reported.
pub fn decide(billing: &BillingConfig, flat_micro: i64, ex: &Extraction) -> ChargeDecision {
    match ex.cost_micro {
        None => ChargeDecision {
            amount_micro: flat_micro,
            source: ChargeSource::Fallback,
            dedupe_key: None,
            reported_micro: None,
        },
        Some(cost) => {
            let dedupe_key = if cost > 0 {
                ex.dedupe_key.clone()
            } else {
                None
            };
            match billing.max_per_request_cents.map(cents_to_micro) {
                Some(cap) if cost > cap => ChargeDecision {
                    amount_micro: cap,
                    source: ChargeSource::Capped,
                    dedupe_key,
                    reported_micro: Some(cost),
                },
                _ => ChargeDecision {
                    amount_micro: cost,
                    source: ChargeSource::Actual,
                    dedupe_key,
                    reported_micro: None,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn billing(raw: Value) -> BillingConfig {
        serde_json::from_value(raw).expect("billing block")
    }

    fn monid() -> BillingConfig {
        billing(json!({
            "type": "response_json",
            "cost_path": "cost.value",
            "currency_path": "cost.currency",
            "dedupe_key_path": ["runId", "id"],
            "max_per_request_cents": 10
        }))
    }

    #[test]
    fn json_path_walks_objects_and_arrays() {
        let v = json!({"usage": {"cost": 0.5}, "choices": [{"x": 1}, {"x": 2}]});
        assert_eq!(json_path(&v, "usage.cost"), Some(&json!(0.5)));
        assert_eq!(json_path(&v, "choices.1.x"), Some(&json!(2)));
        assert_eq!(json_path(&v, "choices.9.x"), None);
        assert_eq!(json_path(&v, "usage.cost.deeper"), None);
        assert_eq!(json_path(&v, "missing"), None);
    }

    #[test]
    fn monid_run_cost_is_exact_micro_dollars() {
        let body =
            br#"{"runId":"run_1","status":"COMPLETED","cost":{"value":0.004,"currency":"USD"}}"#;
        let ex = extract(&monid(), None, None, body);
        assert_eq!(ex.cost_micro, Some(4_000)); // $0.004 = 0.4¢ — not rounded to 0¢
        assert_eq!(ex.dedupe_key.as_deref(), Some("run_1"));
        let d = decide(&monid(), 0, &ex);
        assert_eq!(d.amount_micro, 4_000);
        assert_eq!(d.source, ChargeSource::Actual);
        assert_eq!(d.dedupe_key.as_deref(), Some("run_1"));
    }

    /// Regression (found on the first live Monid call through prod,
    /// 0.43.4): an inline-COMPLETED `POST /v1/run` reports its price as
    /// `billing.reportedCost` in micro-dollars and has NO `cost` field;
    /// only the `GET /v1/runs/{id}` poll carries `cost.value` in USD. With
    /// `cost_path: "cost.value"` alone the run fell back to the flat $0
    /// and was billed only if the claimer happened to poll it.
    #[test]
    fn monid_inline_run_micro_dollars_and_poll_usd_bill_once() {
        let b = billing(json!({
            "type": "response_json",
            "cost_path": [
                "cost.value",
                {"path": "billing.reportedCost.value", "unit": "micro_usd"}
            ],
            "currency_path": "cost.currency",
            "dedupe_key_path": ["runId", "id"],
            "max_per_request_cents": 10
        }));
        // Shapes copied from the live responses (results trimmed).
        let run = br#"{"runId":"01M5V1P7JT3VE8ARWN4TV06HPY","provider":"keenable",
            "endpoint":"/v1/search","status":"COMPLETED","output":{"results":[]},
            "price":{"type":"PER_CALL","amount":{"value":0.004,"currency":"USD"}},
            "billing":{"reportedCost":{"currency":"USD","value":4000,"unit":"MICRO_DOLLAR"}},
            "resultCount":1,"billedUnits":1}"#;
        let poll = br#"{"runId":"01M5V1P7JT3VE8ARWN4TV06HPY","status":"COMPLETED",
            "price":{"type":"PER_CALL","amount":{"value":0.004,"currency":"USD"}},
            "cost":{"value":0.004,"currency":"USD"},"resultCount":1}"#;
        let r = decide(&b, 0, &extract(&b, None, None, run));
        let p = decide(&b, 0, &extract(&b, None, None, poll));
        assert_eq!((r.amount_micro, r.source), (4_000, ChargeSource::Actual));
        assert_eq!((p.amount_micro, p.source), (4_000, ChargeSource::Actual));
        // Same dedupe key → the DB unique index bills only the first.
        assert_eq!(r.dedupe_key, p.dedupe_key);
        assert_eq!(r.dedupe_key.as_deref(), Some("01M5V1P7JT3VE8ARWN4TV06HPY"));
        // The list price (`price.amount.value`) is never read as a cost.
        let inspect =
            br#"{"provider":"keenable","price":{"amount":{"value":0.004,"currency":"USD"}}}"#;
        assert_eq!(extract(&b, None, None, inspect).cost_micro, None);
    }

    #[test]
    fn dedupe_key_falls_back_to_alternative_path() {
        // GET /v1/runs/{id} may name the id differently than POST /v1/run.
        let body = br#"{"id":"run_1","cost":{"value":"0.004","currency":"usd"}}"#;
        let ex = extract(&monid(), None, None, body);
        assert_eq!(ex.cost_micro, Some(4_000));
        assert_eq!(ex.dedupe_key.as_deref(), Some("run_1"));
    }

    #[test]
    fn pending_run_without_cost_falls_back_and_does_not_claim_key() {
        let body = br#"{"runId":"run_1","status":"RUNNING","cost":null}"#;
        let ex = extract(&monid(), None, None, body);
        assert_eq!(ex.cost_micro, None);
        let d = decide(&monid(), 0, &ex);
        assert_eq!(d.source, ChargeSource::Fallback);
        assert_eq!(d.amount_micro, 0);
        assert_eq!(
            d.dedupe_key, None,
            "the COMPLETED poll must still be billable"
        );
    }

    #[test]
    fn zero_cost_does_not_claim_dedupe_key() {
        let body = br#"{"runId":"run_free","cost":{"value":0,"currency":"USD"}}"#;
        let d = decide(&monid(), 0, &extract(&monid(), None, None, body));
        assert_eq!(d.source, ChargeSource::Actual);
        assert_eq!(d.amount_micro, 0);
        assert_eq!(d.dedupe_key, None);
    }

    #[test]
    fn currency_mismatch_falls_back_to_flat() {
        let body = br#"{"runId":"r","cost":{"value":0.004,"currency":"EUR"}}"#;
        let ex = extract(&monid(), None, None, body);
        assert_eq!(ex.cost_micro, None);
        assert!(ex.miss_reason.as_deref().unwrap().contains("EUR"));
        let d = decide(&monid(), 70, &ex);
        assert_eq!((d.amount_micro, d.source), (70, ChargeSource::Fallback));
    }

    #[test]
    fn cost_above_cap_is_clamped() {
        let body = br#"{"runId":"r","cost":{"value":2.5,"currency":"USD"}}"#;
        let d = decide(&monid(), 0, &extract(&monid(), None, None, body));
        assert_eq!(d.source, ChargeSource::Capped);
        assert_eq!(d.amount_micro, 100_000); // 10¢
        assert_eq!(d.reported_micro, Some(2_500_000));
    }

    #[test]
    fn missing_or_garbage_cost_falls_back_to_flat_rate() {
        let exa = billing(json!({"type": "response_json", "cost_path": "costDollars.total"}));
        let flat = flat_micro_per_request(700); // $7/1K = 0.7¢ = 7000µ$
        assert_eq!(flat, 7_000);
        for body in [
            &br#"{"error":"bad request"}"#[..],
            br#"{"costDollars":{"total":"n/a"}}"#,
            br#"{"costDollars":{"total":-1}}"#,
            b"<html>502 Bad Gateway</html>",
            b"",
        ] {
            let d = decide(&exa, flat, &extract(&exa, None, None, body));
            assert_eq!((d.amount_micro, d.source), (flat, ChargeSource::Fallback));
        }
        let d = decide(
            &exa,
            flat,
            &extract(
                &exa,
                None,
                None,
                br#"{"costDollars":{"total":0.005,"search":{"neural":0.005}}}"#,
            ),
        );
        assert_eq!((d.amount_micro, d.source), (5_000, ChargeSource::Actual));
    }

    #[test]
    fn openrouter_usage_cost_with_tiny_token_price() {
        let or = billing(json!({
            "type": "response_json",
            "cost_path": ["usage.cost", "usage.total_cost"]
        }));
        let body = br#"{"id":"gen-1","usage":{"prompt_tokens":12,"completion_tokens":3,"cost":0.0000275}}"#;
        let ex = extract(&or, None, None, body);
        // 27.5µ$ rounds to 28µ$; the flat model would have booked 1000µ$.
        assert_eq!(ex.cost_micro, Some(28));
        let legacy = br#"{"usage":{"total_cost":0.0012}}"#;
        assert_eq!(extract(&or, None, None, legacy).cost_micro, Some(1_200));
    }

    #[test]
    fn openrouter_streamed_sse_reads_usage_from_last_chunk() {
        let or = billing(json!({"type": "response_json", "cost_path": "usage.cost"}));
        let body = b": OPENROUTER PROCESSING\n\n\
data: {\"id\":\"gen-1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n\
data: {\"id\":\"gen-1\",\"choices\":[],\"usage\":{\"cost\":0.00042}}\n\n\
data: [DONE]\n\n";
        assert_eq!(
            extract(&or, None, Some("text/event-stream; charset=utf-8"), body).cost_micro,
            Some(420)
        );
    }

    // Regression (proxy audit 2026-11-19): every non-JSON body was
    // parsed as SSE, so a text/plain or HTML response echoing caller
    // input could set the billed cost with a `data: {"cost":…}` line.
    #[test]
    fn sse_is_parsed_only_for_event_stream_content_type() {
        let or = billing(json!({"type": "response_json", "cost_path": "usage.cost"}));
        let body = b"you searched for:\ndata: {\"usage\":{\"cost\":0.000001}}\n";
        for ct in [None, Some("text/plain"), Some("text/html; charset=utf-8")] {
            let ex = extract(&or, None, ct, body);
            assert_eq!(ex.cost_micro, None, "{ct:?}");
            assert_eq!(
                decide(&or, 1_000, &ex).source,
                ChargeSource::Fallback,
                "{ct:?}"
            );
        }
        assert_eq!(
            extract(&or, None, Some("TEXT/EVENT-STREAM"), body).cost_micro,
            Some(1)
        );
        assert!(is_event_stream(Some("text/event-stream")));
        assert!(!is_event_stream(Some("application/json")));
    }

    // Regression (proxy audit 2026-11-19): a huge reported cost with no
    // `max_per_request_cents` saturated to i64::MAX.
    #[test]
    fn absurd_reported_costs_fall_back_to_flat() {
        let exa = billing(json!({"type": "response_json", "cost_path": "costDollars.total"}));
        for body in [
            &br#"{"costDollars":{"total":1e300}}"#[..],
            br#"{"costDollars":{"total":100.01}}"#,
            br#"{"costDollars":{"total":"99999999999999999999"}}"#,
        ] {
            let ex = extract(&exa, None, None, body);
            assert_eq!(ex.cost_micro, None);
            assert!(ex.miss_reason.as_deref().unwrap().contains("ceiling"));
            let d = decide(&exa, 7_000, &ex);
            assert_eq!((d.amount_micro, d.source), (7_000, ChargeSource::Fallback));
        }
        // Exactly $100 is still accepted.
        assert_eq!(
            extract(&exa, None, None, br#"{"costDollars":{"total":100}}"#).cost_micro,
            Some(MAX_REPORTED_MICRO)
        );
    }

    // Regression (proxy audit 2026-11-19): the dedupe key was claimed by
    // the first positive cost, so a later larger final cost for the same
    // run billed 0.
    #[test]
    fn dedupe_bills_the_increase_only() {
        assert_eq!(dedupe_delta(4_000, 1_000), 3_000);
        assert_eq!(dedupe_delta(4_000, 4_000), 0);
        assert_eq!(dedupe_delta(1_000, 4_000), 0);
        assert_eq!(dedupe_delta(i64::MAX, -1), i64::MAX);
    }

    #[test]
    fn abandoned_requests_charge_cap_else_flat() {
        assert_eq!(abandoned_micro(Some(&monid()), 0), 100_000);
        let exa = billing(json!({"type": "response_json", "cost_path": "costDollars.total"}));
        assert_eq!(abandoned_micro(Some(&exa), 7_000), 7_000);
        assert_eq!(abandoned_micro(None, 1_000), 1_000);
        let d = abandoned_decision(1_000);
        assert_eq!((d.amount_micro, d.source.as_str()), (1_000, "abandoned"));
        assert_eq!(flat_decision(1_000).source.as_str(), "flat");
    }

    #[test]
    fn header_source_and_units() {
        let h =
            billing(json!({"type": "response_header", "header": "x-cost-cents", "unit": "cents"}));
        assert_eq!(
            extract(&h, Some("1.25"), None, b"").cost_micro,
            Some(12_500)
        );
        assert_eq!(extract(&h, None, None, b"").cost_micro, None);
        assert_eq!(extract(&h, Some("abc"), None, b"").cost_micro, None);
        let micro =
            billing(json!({"type": "response_json", "cost_path": "c", "unit": "micro_usd"}));
        assert_eq!(
            extract(&micro, None, None, br#"{"c": 42}"#).cost_micro,
            Some(42)
        );
    }

    #[test]
    fn reserve_uses_cap_else_flat() {
        assert_eq!(reserve_micro(&monid(), 0), 100_000);
        let exa = billing(json!({"type": "response_json", "cost_path": "costDollars.total"}));
        assert_eq!(reserve_micro(&exa, 7_000), 7_000);
    }

    #[test]
    fn unit_conversions() {
        assert_eq!(cents_to_micro(0.4), 4_000);
        assert_eq!(micro_to_cents(4_000), 0.4);
        assert_eq!(micro_to_usd(4_000), 0.004);
        assert_eq!(flat_micro_per_request(100), 1_000); // $1/1K = $0.001
        assert_eq!(flat_micro_per_request(-5), 0);
    }
}
