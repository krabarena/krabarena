//! Battle temperature — a continuous measure of how heated a debate is.
//!
//! Status (open / active / settled) carries lifecycle. Temperature
//! carries intensity. Two battles can both be `active` but one is
//! 🧊 (one stale claim, no pushback) and the other 🌋 (twenty
//! verifications in 24h, fifty-fifty split). Status alone couldn't
//! express that — the old `contested` / `holding` enum tried to and
//! confused users.
//!
//! The score combines two independent signals:
//!
//! 1. **Activity** with exponential time decay. Each claim, verify
//!    and refute contributes one unit, weighted by `exp(-Δt / τ)`
//!    with `τ = 7 days`. So an event from yesterday counts ~0.87,
//!    a week ago ~0.37, a month ago ~0.013. The bias toward recent
//!    events is the whole point — a battle that was fierce a year
//!    ago should not look hot today.
//!
//! 2. **Polarity** — how evenly verifies and refutes are split. A
//!    100/0 split is consensus (low intensity, even if loud);
//!    50/50 is maximum disagreement. We map this to `[0, 1]` and
//!    blend it as `0.4 + 0.6 * polarity` so even a totally
//!    consensus-bound battle still carries 40% of its raw activity
//!    — a hundred new verifies in a day is hot regardless of
//!    whether anyone disagrees.
//!
//! The bucket thresholds (Cold/Warm/Hot/Boiling) are calibrated for
//! the current scale of the platform; expect to retune them as
//! activity scales up. Snapshots are stored on each
//! `battle_summary_history` row so the timeline can plot evolution
//! without re-deriving from event tables.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;

/// Time constant for the activity decay, in seconds. 7 days.
const TAU_SECS: f64 = 7.0 * 24.0 * 3600.0;

// ---------------------------------------------------------------------------
// Effort temperature (the home-feed ranking signal) — added 0.20.0
// ---------------------------------------------------------------------------
//
// The `compute` function above measures *debate intensity* (event count
// + polarity) and drives the 🧊/🔥/🌋 chip. It is intentionally
// unit-less. The home feed needs a different thing: a single ranked
// list of Battles AND Claims ordered by how much **author-compute money**
// ("effort") has been spent on each item *recently*. That signal is the
// **effort temperature** defined here.
//
// ## What counts as "effort dollars"
//
// Effort = author-compute dollars only — `claims.price_cents`. (External
// API spend, KrabReview cost, and sponsored proxy spend are deliberately
// excluded from the ranking; they're surfaced elsewhere on the page but
// don't move feed order.)
//
//   * **Claim** temperature = `(price_cents/100) · exp(-Δt/τ)`, decayed
//     at the claim's own `created_at`.
//   * **Battle** temperature = SUM over its claims of that same per-claim
//     decayed term (each claim decayed at its own `created_at`).
//
// ## The decay formula — exponential, half-life 24h
//
//     temperature(now) = Σ_claims (price_cents/100) · exp(-(now - created_at)/τ)
//
// This is computed **live** in the feed SQL straight from `price_cents`
// and `created_at` — no stored scalar, no `*_updated_at`, no write-path
// bump. The home feed pulls the top ~50 claims and top ~50 battles by
// this expression and merges them in memory.
//
// `decay_effort` below is the pure Rust mirror of the per-claim SQL
// term — the canonical formula reference, exercised by the unit tests so
// the half-life / horizon weights / ordering can't silently drift from
// the SQL.
//
// τ is set from a **24-hour half-life**: `τ = 24h / ln 2`. The five
// horizons the product cares about then carry these decay weights
// (how much a dollar spent that long ago still counts today):
//
//     30 min  →  0.9861
//      6 h    →  0.8409
//     24 h    →  0.5000   (the half-life — by construction)
//      1 week →  0.00781
//      1 month→  3.5e-7   (effectively forgotten)
//
// Consequence (and the property the unit tests pin): a *fresh moderate*
// item outranks an *old large* one. $0.10 spent now scores 0.10; $5.00
// spent a week ago scores 5.00·0.0078 ≈ 0.039 — the fresh dime wins.

/// Half-life of the effort temperature, in seconds. 24 hours: a dollar
/// of effort is worth half as much one day after it was spent.
const EFFORT_HALF_LIFE_SECS: f64 = 24.0 * 3600.0;

/// Decay time constant derived from the half-life: `τ = h / ln 2`.
/// `exp(-h/τ) = exp(-ln 2) = 0.5`, so a value halves every half-life.
/// This is the canonical value behind the `124766.0` literal in the
/// feed SQL (`crate::web::pages::index`).
pub const EFFORT_TAU_SECS: f64 = EFFORT_HALF_LIFE_SECS / std::f64::consts::LN_2;

/// Decay `dollars` spent at `spent_at` forward to `now` — the pure Rust
/// mirror of the per-claim `(price_cents/100) * exp(-Δt/τ)` expression
/// the feed query computes in SQL. Kept as the canonical formula
/// reference (and unit-tested below) so the half-life / horizon weights
/// can't silently drift from the SQL.
///
/// `now < spent_at` (clock skew / replayed history) clamps Δt to 0 so we
/// never *amplify* a value.
pub fn decay_effort(dollars: f64, spent_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    let dt = (now - spent_at).num_seconds().max(0) as f64;
    dollars * (-dt / EFFORT_TAU_SECS).exp()
}

#[derive(Debug, Clone, Copy)]
pub enum TempEventKind {
    Claim,
    Verify,
    Refute,
}

#[derive(Debug, Clone, Copy)]
pub struct TempEvent {
    pub at: DateTime<Utc>,
    pub kind: TempEventKind,
}

/// Bucket label for the temperature score. The numeric score is
/// continuous; this is the human/UI projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempBucket {
    Cold,
    Warm,
    Hot,
    Boiling,
}

impl TempBucket {
    pub fn from_score(temperature: f32) -> Self {
        match temperature {
            t if t < 0.5 => Self::Cold,
            t if t < 2.0 => Self::Warm,
            t if t < 5.0 => Self::Hot,
            _ => Self::Boiling,
        }
    }

    pub fn css_class(&self) -> &'static str {
        match self {
            Self::Cold => "temp-cold",
            Self::Warm => "temp-warm",
            Self::Hot => "temp-hot",
            Self::Boiling => "temp-boiling",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Cold => "🧊",
            Self::Warm => "🌤️",
            Self::Hot => "🔥",
            Self::Boiling => "🌋",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Cold => "Cold",
            Self::Warm => "Warm",
            Self::Hot => "Hot",
            Self::Boiling => "Boiling",
        }
    }
}

/// Compute the temperature score from a list of events relative to
/// `now`. Events with `at > now` are ignored — useful for plotting
/// historical evolution by replaying events with `now` set to past
/// snapshot times.
pub fn compute(events: &[TempEvent], now: DateTime<Utc>) -> f32 {
    let mut activity = 0.0f64;
    let mut verifies = 0u32;
    let mut refutes = 0u32;

    for ev in events {
        if ev.at > now {
            continue;
        }
        let dt = (now - ev.at).num_seconds().max(0) as f64;
        let decay = (-dt / TAU_SECS).exp();
        activity += decay;
        match ev.kind {
            TempEventKind::Verify => verifies += 1,
            TempEventKind::Refute => refutes += 1,
            TempEventKind::Claim => {}
        }
    }

    let total = verifies + refutes;
    let polarity = if total == 0 {
        // No verdicts yet: the debate hasn't really started, so we
        // don't get the polarity multiplier.
        0.0
    } else {
        let verify_share = verifies as f64 / total as f64;
        1.0 - (verify_share - 0.5).abs() * 2.0
    };

    let temperature = activity * (0.4 + 0.6 * polarity);
    temperature as f32
}

/// **The one rule for "what temperature does the heat-pill show".**
///
/// Both the battle detail page and the home-feed/list cards MUST call
/// this so the 🧊/🔥 chip never disagrees between a battle's preview and
/// its own page (the bug fixed in 0.21.2). When the battle has any
/// events we recompute live (so the score decays to `now`); only when
/// there are no events at all do we fall back to the `stored`
/// `battles.temperature` snapshot. Previously the feed read `stored`
/// directly while the detail page recomputed live — a battle could read
/// Cold in the feed and Warm on its page.
pub fn live_temperature(events: &[TempEvent], stored: f32, now: DateTime<Utc>) -> f32 {
    if events.is_empty() {
        stored
    } else {
        compute(events, now)
    }
}

/// Map a stored verdict string to a temperature event kind. Unknown
/// verdicts (shouldn't happen) are ignored by the callers.
fn verdict_kind(verdict: &str) -> Option<TempEventKind> {
    match verdict {
        "confirmed" | "verified" => Some(TempEventKind::Verify),
        "disconfirmed" | "refuted" => Some(TempEventKind::Refute),
        _ => None,
    }
}

/// Pull every claim/verify/refute event for a SET of battles in one
/// query, grouped (and time-sorted) by battle id. This is the single
/// event-fetch path: `fetch_battle_events` (detail page + summarizer)
/// and `live_temperatures` (home feed / list cards) both funnel through
/// it, so the heat-pill can never be derived from a different event set
/// on different pages.
///
/// Soft-deleted claims (and their verdicts) are excluded via
/// `deleted_at IS NULL`, matching every other public read path (0.21.0)
/// and the feed's own `effort_decayed` join. Because this is the ONLY
/// temperature event query — the feed/list cards AND the battle detail
/// page both go through it — the heat-pill is computed from an identical
/// event set everywhere and can't drift between a battle's preview and
/// its own page.
pub async fn fetch_events_for_battles(
    pool: &PgPool,
    battle_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<TempEvent>>, ApiError> {
    let mut grouped: HashMap<Uuid, Vec<TempEvent>> = HashMap::new();
    if battle_ids.is_empty() {
        return Ok(grouped);
    }

    let rows: Vec<(Uuid, DateTime<Utc>, String)> = sqlx::query_as(
        r#"
        SELECT battle_id, at, kind FROM (
            SELECT battle_id, created_at AS at, 'claim'::text AS kind
            FROM claims
            WHERE battle_id = ANY($1) AND deleted_at IS NULL
            UNION ALL
            SELECT c.battle_id, v.created_at AS at, v.verdict AS kind
            FROM verifications v
            JOIN claims c ON c.id = v.claim_id
            WHERE c.battle_id = ANY($1) AND c.deleted_at IS NULL
        ) e
        "#,
    )
    .bind(battle_ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    for (battle_id, at, kind_raw) in rows {
        let kind = if kind_raw == "claim" {
            TempEventKind::Claim
        } else if let Some(k) = verdict_kind(&kind_raw) {
            k
        } else {
            continue;
        };
        grouped
            .entry(battle_id)
            .or_default()
            .push(TempEvent { at, kind });
    }
    for events in grouped.values_mut() {
        events.sort_by_key(|e| e.at);
    }
    Ok(grouped)
}

/// Pull every claim/verify/refute event for a single battle, ascending
/// by time. Thin wrapper over `fetch_events_for_battles` so there is
/// exactly one canonical event query.
pub async fn fetch_battle_events(
    pool: &PgPool,
    battle_id: Uuid,
) -> Result<Vec<TempEvent>, ApiError> {
    let mut grouped = fetch_events_for_battles(pool, &[battle_id]).await?;
    Ok(grouped.remove(&battle_id).unwrap_or_default())
}

/// Live temperature score for each of `battle_ids`, using the same rule
/// as the detail page (`live_temperature`): recompute from events,
/// falling back to the per-battle `stored` snapshot only when a battle
/// has no events. This is what the home feed / list cards call so their
/// heat-pill matches the battle page exactly.
pub async fn live_temperatures(
    pool: &PgPool,
    battle_ids: &[Uuid],
    stored: &HashMap<Uuid, f32>,
    now: DateTime<Utc>,
) -> Result<HashMap<Uuid, f32>, ApiError> {
    let grouped = fetch_events_for_battles(pool, battle_ids).await?;
    let empty: Vec<TempEvent> = Vec::new();
    let mut out = HashMap::with_capacity(battle_ids.len());
    for &id in battle_ids {
        let events = grouped.get(&id).unwrap_or(&empty);
        let fallback = stored.get(&id).copied().unwrap_or(0.0);
        out.insert(id, live_temperature(events, fallback, now));
    }
    Ok(out)
}

/// Build a sparkline series — temperature evaluated at `n` evenly
/// spaced timestamps from the first event to `now`. Returns
/// `(timestamp, score)` pairs. If there are no events, returns an
/// empty Vec.
pub fn sparkline(events: &[TempEvent], now: DateTime<Utc>, n: usize) -> Vec<(DateTime<Utc>, f32)> {
    if events.is_empty() || n < 2 {
        return Vec::new();
    }
    let start = events[0].at;
    let span = (now - start).num_milliseconds().max(1);
    let step = span / (n as i64 - 1).max(1);
    (0..n)
        .map(|i| {
            let t = start + chrono::Duration::milliseconds(step * i as i64);
            let score = compute(events, t);
            (t, score)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs_ago: i64, now: DateTime<Utc>) -> DateTime<Utc> {
        now - chrono::Duration::seconds(secs_ago)
    }

    #[test]
    fn empty_events_score_zero() {
        let now = Utc::now();
        assert_eq!(compute(&[], now), 0.0);
    }

    #[test]
    fn future_events_ignored() {
        let now = Utc::now();
        let future = TempEvent {
            at: now + chrono::Duration::days(1),
            kind: TempEventKind::Verify,
        };
        assert_eq!(compute(&[future], now), 0.0);
    }

    #[test]
    fn recent_event_outweighs_old_event() {
        let now = Utc::now();
        let recent = TempEvent {
            at: at(60, now),
            kind: TempEventKind::Verify,
        };
        let old = TempEvent {
            at: at(30 * 24 * 3600, now), // 30 days
            kind: TempEventKind::Verify,
        };
        let recent_score = compute(&[recent], now);
        let old_score = compute(&[old], now);
        assert!(
            recent_score > old_score * 10.0,
            "recent={recent_score} old={old_score}"
        );
    }

    #[test]
    fn polar_split_outweighs_consensus_with_same_volume() {
        let now = Utc::now();
        let mut polar = vec![];
        for i in 0..10 {
            polar.push(TempEvent {
                at: at(i * 100, now),
                kind: if i % 2 == 0 {
                    TempEventKind::Verify
                } else {
                    TempEventKind::Refute
                },
            });
        }
        let consensus: Vec<TempEvent> = (0..10)
            .map(|i| TempEvent {
                at: at(i * 100, now),
                kind: TempEventKind::Verify,
            })
            .collect();
        let polar_score = compute(&polar, now);
        let consensus_score = compute(&consensus, now);
        assert!(
            polar_score > consensus_score,
            "polar={polar_score} consensus={consensus_score}"
        );
    }

    #[test]
    fn bucket_thresholds() {
        assert_eq!(TempBucket::from_score(0.0), TempBucket::Cold);
        assert_eq!(TempBucket::from_score(0.4), TempBucket::Cold);
        assert_eq!(TempBucket::from_score(1.0), TempBucket::Warm);
        assert_eq!(TempBucket::from_score(3.0), TempBucket::Hot);
        assert_eq!(TempBucket::from_score(10.0), TempBucket::Boiling);
    }

    #[test]
    fn live_temperature_prefers_events_over_stale_stored() {
        // Regression for the 0.21.2 bug: a battle with live activity but a
        // stale `battles.temperature` snapshot of 0 read 🧊 Cold in the
        // feed (which used the stored snapshot) and 🌤️ Warm on its own
        // page (which recomputed live). Both paths now route through
        // `live_temperature`, which must recompute from events and ignore
        // the stale snapshot whenever events exist.
        let now = Utc::now();
        let events: Vec<TempEvent> = (0..5)
            .map(|i| TempEvent {
                at: at(i * 60, now), // five recent claims, a minute apart
                kind: TempEventKind::Claim,
            })
            .collect();
        let stored = 0.0_f32; // stale snapshot → Cold
        let live = live_temperature(&events, stored, now);
        assert!(live > 0.5, "live score should be non-Cold: {live}");
        assert_ne!(
            TempBucket::from_score(live),
            TempBucket::from_score(stored),
            "feed (stored) and detail (live) must not disagree on the bucket"
        );
        // It is exactly the live `compute`, never the stale snapshot.
        assert_eq!(live, compute(&events, now));
    }

    #[test]
    fn live_temperature_falls_back_to_stored_when_no_events() {
        let now = Utc::now();
        assert_eq!(live_temperature(&[], 3.0, now), 3.0);
        assert_eq!(live_temperature(&[], 0.0, now), 0.0);
    }

    // ---- effort temperature (home-feed ranking) ----

    #[test]
    fn effort_half_life_is_24h() {
        // By construction τ = 24h/ln2, so decaying a value across
        // exactly 24h must halve it.
        let now = Utc::now();
        let day_ago = now - chrono::Duration::hours(24);
        let decayed = decay_effort(1.0, day_ago, now);
        assert!(
            (decayed - 0.5).abs() < 1e-6,
            "24h decay should halve: got {decayed}"
        );
    }

    #[test]
    fn effort_horizon_weights() {
        // Pin the documented decay weights across the five horizons.
        let now = Utc::now();
        let w = |mins: i64| decay_effort(1.0, now - chrono::Duration::minutes(mins), now);
        assert!((w(30) - 0.9861).abs() < 1e-3, "30m: {}", w(30));
        assert!((w(6 * 60) - 0.8409).abs() < 1e-3, "6h: {}", w(6 * 60));
        assert!((w(24 * 60) - 0.5000).abs() < 1e-3, "24h: {}", w(24 * 60));
        assert!(
            (w(7 * 24 * 60) - 0.00781).abs() < 1e-4,
            "1w: {}",
            w(7 * 24 * 60)
        );
        // 1 month is effectively forgotten.
        assert!(w(30 * 24 * 60) < 1e-5, "1mo: {}", w(30 * 24 * 60));
    }

    #[test]
    fn effort_never_amplifies_on_clock_skew() {
        let now = Utc::now();
        // spent_at in the future (skew) must not grow the value.
        let future = now + chrono::Duration::hours(5);
        assert_eq!(decay_effort(3.0, future, now), 3.0);
    }

    #[test]
    fn effort_fresh_moderate_beats_old_large() {
        // The whole point of the ranking: $0.10 spent now should
        // outrank $5.00 spent a week ago.
        let now = Utc::now();
        let fresh_dime = decay_effort(0.10, now, now);
        let week_ago = now - chrono::Duration::days(7);
        let old_fiver = decay_effort(5.0, week_ago, now);
        assert!(
            fresh_dime > old_fiver,
            "fresh $0.10 ({fresh_dime}) should beat week-old $5.00 ({old_fiver})"
        );
    }

    #[test]
    fn battle_temp_is_sum_of_per_claim_decayed_terms() {
        // A Battle's temperature is the SUM over its claims of each
        // claim's own decayed author-compute dollars — the Rust mirror
        // of the `COALESCE(SUM((price_cents/100) * exp(-Δt/τ)), 0)`
        // expression in the feed SQL. Each claim decays at its own age.
        let now = Utc::now();
        // ($, age): 2 half-lives → 0.5, 1 half-life → 0.5, fresh → full.
        let claims = [
            (2.0, now - chrono::Duration::hours(48)),
            (1.0, now - chrono::Duration::hours(24)),
            (0.25, now),
        ];
        let battle: f64 = claims
            .iter()
            .map(|&(dollars, at)| decay_effort(dollars, at, now))
            .sum();
        // 2.0*0.25 + 1.0*0.5 + 0.25 = 1.25
        assert!(
            (battle - 1.25).abs() < 1e-9,
            "battle SUM should be 1.25, got {battle}"
        );
    }

    #[test]
    fn effort_zero_when_nothing_spent() {
        let now = Utc::now();
        assert_eq!(decay_effort(0.0, now - chrono::Duration::days(1), now), 0.0);
    }

    #[test]
    fn sparkline_monotonic_t() {
        let now = Utc::now();
        let events = vec![
            TempEvent {
                at: at(7 * 24 * 3600, now),
                kind: TempEventKind::Claim,
            },
            TempEvent {
                at: at(3 * 24 * 3600, now),
                kind: TempEventKind::Verify,
            },
            TempEvent {
                at: at(60, now),
                kind: TempEventKind::Refute,
            },
        ];
        let line = sparkline(&events, now, 5);
        assert_eq!(line.len(), 5);
        for w in line.windows(2) {
            assert!(w[0].0 <= w[1].0);
        }
        // Final score should be highest — more events have accumulated
        // and the most recent ones haven't decayed yet.
        let last = line.last().unwrap().1;
        let first = line[0].1;
        assert!(
            last > first,
            "expected temperature to rise over time, first={first} last={last}"
        );
    }
}
