//! Moderation policy: the pure, testable rules that translate KrabReviewer
//! block verdicts into account-level strikes, rate limits, and suspensions.
//!
//! This module has no I/O — it operates on a slice of historical strikes and
//! returns the enforcement decision. `review_service` wires it to the DB.
//!
//! Severity mapping and escalation thresholds are intentionally named
//! constants so ops can tune without touching control flow.

use chrono::{DateTime, Duration, Utc};

/// Moderation flag codes emitted by the KrabReviewer (model + deterministic
/// pre-filter).  Kept in lock-step with the JSON schema returned by Gemini.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagCode {
    SecretDetected,
    Pii,
    UnsafeAction,
    OffTopic,
    LowEffort,
    Spam,
    Duplicate,
    Flooding,
    OpinionPiece,
    PolicyViolation,
    /// Claim prose is not written in English (M10). Treated like OffTopic/
    /// LowEffort: an honest-mistake content issue, not abuse — Minor.
    NonEnglish,
    /// Artifact bundle is missing `RUN.md` and/or `README.md`. These
    /// two files are the canonical structure every artifact must
    /// carry — `RUN.md` holds the exact reproduction steps (commands,
    /// pins, expected runtime), `README.md` is the extended report.
    /// Missing either blocks the claim at pre-filter time, before
    /// Gemini is even called, with a concrete autofix hint.
    MissingArtifactFile,
}

impl FlagCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            FlagCode::SecretDetected => "secret_detected",
            FlagCode::Pii => "pii",
            FlagCode::UnsafeAction => "unsafe_action",
            FlagCode::OffTopic => "off_topic",
            FlagCode::LowEffort => "low_effort",
            FlagCode::Spam => "spam",
            FlagCode::Duplicate => "duplicate",
            FlagCode::Flooding => "flooding",
            FlagCode::OpinionPiece => "opinion_piece",
            FlagCode::PolicyViolation => "policy_violation",
            FlagCode::NonEnglish => "non_english",
            FlagCode::MissingArtifactFile => "missing_artifact_file",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "secret_detected" => Some(Self::SecretDetected),
            "pii" => Some(Self::Pii),
            "unsafe_action" => Some(Self::UnsafeAction),
            "off_topic" => Some(Self::OffTopic),
            "low_effort" => Some(Self::LowEffort),
            "spam" => Some(Self::Spam),
            "duplicate" => Some(Self::Duplicate),
            "flooding" => Some(Self::Flooding),
            "opinion_piece" => Some(Self::OpinionPiece),
            "policy_violation" => Some(Self::PolicyViolation),
            "non_english" => Some(Self::NonEnglish),
            "missing_artifact_file" => Some(Self::MissingArtifactFile),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Minor,
    Major,
    Severe,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Minor => "minor",
            Severity::Major => "major",
            Severity::Severe => "severe",
        }
    }
}

/// Classify a moderation flag into strike severity.
///
/// SecretDetected is Major (not Severe): forgotten keys in a benchmark
/// artifact are an honest mistake, not abuse. The artifact is blocked,
/// but the account is not immediately suspended. Repeated secret leaks
/// escalate through the Major ladder (3 in 24h → 1h suspension, etc.).
pub fn severity_for(code: FlagCode) -> Severity {
    match code {
        FlagCode::UnsafeAction | FlagCode::PolicyViolation | FlagCode::Pii => Severity::Severe,
        FlagCode::SecretDetected
        | FlagCode::Spam
        | FlagCode::Flooding
        | FlagCode::OpinionPiece
        | FlagCode::Duplicate => Severity::Major,
        FlagCode::LowEffort
        | FlagCode::OffTopic
        | FlagCode::MissingArtifactFile
        | FlagCode::NonEnglish => Severity::Minor,
    }
}

/// A past strike relevant to the escalation decision.
#[derive(Debug, Clone)]
pub struct PastStrike {
    pub severity: Severity,
    pub created_at: DateTime<Utc>,
}

/// The enforcement action to apply after recording a new strike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enforcement {
    /// Block was recorded as a strike but no account action yet.
    None,
    /// Publishing rate-limited (one Claim per 4h) until the given time.
    RateLimit {
        until: DateTime<Utc>,
        reason: String,
    },
    /// Account suspended (no Claim/Response/Comment writes) until the given time.
    Suspend {
        until: DateTime<Utc>,
        reason: String,
    },
    /// Permanent suspension (`until` = far future).
    PermanentSuspend { reason: String },
}

// -- Thresholds -------------------------------------------------------------
//
// Pre-launch posture (2026-04): relaxed from the original strict defaults
// because the platform is small, honest authors iterating on claim
// structure can trip off_topic / low_effort multiple times in a session,
// and punishing real users is currently worse than tolerating a little
// noise. Severe-tier limits (M3 unsafe_action, M9 policy_violation, PII)
// are UNCHANGED — those exist for real safety, not dev friction.
//
// **Original strict values** (restore these post-launch / at first sign
// of actual spam):
//
//   MAJOR_WINDOW_SHORT_LIMIT = 3        (now 10)
//   MAJOR_WINDOW_MID_LIMIT   = 5        (now 20)
//   MAJOR_MID_SUSPENSION_HOURS = 24     (now 6)
//   MAJOR_WINDOW_LONG_LIMIT  = 10       (now 50)
//   MAJOR_LONG_SUSPENSION_DAYS = 7      (now 2)
//   MINOR_WINDOW_SHORT_LIMIT = 5        (now 20)
//   MINOR_RATE_LIMIT_HOURS   = 24       (now 4)
//   MINOR_WINDOW_LONG_LIMIT  = 20       (now 100)
//   MINOR_LONG_SUSPENSION_HOURS = 24    (now 12)

const SEVERE_FIRST_SUSPENSION_DAYS: i64 = 7;
const SEVERE_SECOND_SUSPENSION_DAYS: i64 = 30;

const MAJOR_WINDOW_SHORT_HOURS: i64 = 24;
const MAJOR_WINDOW_SHORT_LIMIT: usize = 10;
const MAJOR_SHORT_SUSPENSION_HOURS: i64 = 1;

const MAJOR_WINDOW_MID_DAYS: i64 = 7;
const MAJOR_WINDOW_MID_LIMIT: usize = 20;
const MAJOR_MID_SUSPENSION_HOURS: i64 = 6;

const MAJOR_WINDOW_LONG_DAYS: i64 = 30;
const MAJOR_WINDOW_LONG_LIMIT: usize = 50;
const MAJOR_LONG_SUSPENSION_DAYS: i64 = 2;

const MINOR_WINDOW_SHORT_HOURS: i64 = 24;
const MINOR_WINDOW_SHORT_LIMIT: usize = 20;
const MINOR_RATE_LIMIT_HOURS: i64 = 4;

const MINOR_WINDOW_LONG_DAYS: i64 = 30;
const MINOR_WINDOW_LONG_LIMIT: usize = 100;
const MINOR_LONG_SUSPENSION_HOURS: i64 = 12;

/// Compute the enforcement outcome given a new strike plus recent history.
///
/// `now` is taken as a parameter so tests can be deterministic.
pub fn evaluate(new_severity: Severity, now: DateTime<Utc>, history: &[PastStrike]) -> Enforcement {
    match new_severity {
        Severity::Severe => severe_escalation(now, history),
        Severity::Major => major_escalation(now, history),
        Severity::Minor => minor_escalation(now, history),
    }
}

fn severe_escalation(now: DateTime<Utc>, history: &[PastStrike]) -> Enforcement {
    // Count prior severe strikes within the 30-day cooldown window.
    let cutoff = now - Duration::days(SEVERE_SECOND_SUSPENSION_DAYS);
    let prior_severe = history
        .iter()
        .filter(|s| s.severity == Severity::Severe && s.created_at >= cutoff)
        .count();

    // The new strike is not yet in history, so:
    //   prior == 0 → this is the 1st → 7-day
    //   prior == 1 → this is the 2nd → 30-day
    //   prior >= 2 → this is the 3rd+ → permanent
    match prior_severe {
        0 => Enforcement::Suspend {
            until: now + Duration::days(SEVERE_FIRST_SUSPENSION_DAYS),
            reason: "severe moderation strike".into(),
        },
        1 => Enforcement::Suspend {
            until: now + Duration::days(SEVERE_SECOND_SUSPENSION_DAYS),
            reason: "second severe moderation strike within 30 days".into(),
        },
        _ => Enforcement::PermanentSuspend {
            reason: "repeated severe moderation strikes".into(),
        },
    }
}

fn major_escalation(now: DateTime<Utc>, history: &[PastStrike]) -> Enforcement {
    // +1 accounts for the strike we are about to add but that is not yet in history.
    let in_short = count_since(
        history,
        Severity::Major,
        now - Duration::hours(MAJOR_WINDOW_SHORT_HOURS),
    ) + 1;
    let in_mid = count_since(
        history,
        Severity::Major,
        now - Duration::days(MAJOR_WINDOW_MID_DAYS),
    ) + 1;
    let in_long = count_since(
        history,
        Severity::Major,
        now - Duration::days(MAJOR_WINDOW_LONG_DAYS),
    ) + 1;

    if in_long >= MAJOR_WINDOW_LONG_LIMIT {
        return Enforcement::Suspend {
            until: now + Duration::days(MAJOR_LONG_SUSPENSION_DAYS),
            reason: format!("{} major strikes in {}d", in_long, MAJOR_WINDOW_LONG_DAYS),
        };
    }
    if in_mid >= MAJOR_WINDOW_MID_LIMIT {
        return Enforcement::Suspend {
            until: now + Duration::hours(MAJOR_MID_SUSPENSION_HOURS),
            reason: format!("{} major strikes in {}d", in_mid, MAJOR_WINDOW_MID_DAYS),
        };
    }
    if in_short >= MAJOR_WINDOW_SHORT_LIMIT {
        return Enforcement::Suspend {
            until: now + Duration::hours(MAJOR_SHORT_SUSPENSION_HOURS),
            reason: format!(
                "{} major strikes in {}h",
                in_short, MAJOR_WINDOW_SHORT_HOURS
            ),
        };
    }
    Enforcement::None
}

fn minor_escalation(now: DateTime<Utc>, history: &[PastStrike]) -> Enforcement {
    let in_long = count_since(
        history,
        Severity::Minor,
        now - Duration::days(MINOR_WINDOW_LONG_DAYS),
    ) + 1;
    let in_short = count_since(
        history,
        Severity::Minor,
        now - Duration::hours(MINOR_WINDOW_SHORT_HOURS),
    ) + 1;

    if in_long >= MINOR_WINDOW_LONG_LIMIT {
        return Enforcement::Suspend {
            until: now + Duration::hours(MINOR_LONG_SUSPENSION_HOURS),
            reason: format!("{} minor strikes in {}d", in_long, MINOR_WINDOW_LONG_DAYS),
        };
    }
    if in_short >= MINOR_WINDOW_SHORT_LIMIT {
        return Enforcement::RateLimit {
            until: now + Duration::hours(MINOR_RATE_LIMIT_HOURS),
            reason: format!(
                "{} minor strikes in {}h — publish rate limited",
                in_short, MINOR_WINDOW_SHORT_HOURS
            ),
        };
    }
    Enforcement::None
}

fn count_since(history: &[PastStrike], severity: Severity, since: DateTime<Utc>) -> usize {
    history
        .iter()
        .filter(|s| s.severity == severity && s.created_at >= since)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(sev: Severity, minutes_ago: i64, now: DateTime<Utc>) -> PastStrike {
        PastStrike {
            severity: sev,
            created_at: now - Duration::minutes(minutes_ago),
        }
    }

    #[test]
    fn severity_mapping_matches_plan() {
        assert_eq!(severity_for(FlagCode::SecretDetected), Severity::Major);
        assert_eq!(severity_for(FlagCode::UnsafeAction), Severity::Severe);
        assert_eq!(severity_for(FlagCode::PolicyViolation), Severity::Severe);
        assert_eq!(severity_for(FlagCode::Pii), Severity::Severe);
        assert_eq!(severity_for(FlagCode::Spam), Severity::Major);
        assert_eq!(severity_for(FlagCode::Flooding), Severity::Major);
        assert_eq!(severity_for(FlagCode::Duplicate), Severity::Major);
        assert_eq!(severity_for(FlagCode::OpinionPiece), Severity::Major);
        assert_eq!(severity_for(FlagCode::LowEffort), Severity::Minor);
        assert_eq!(severity_for(FlagCode::OffTopic), Severity::Minor);
        assert_eq!(severity_for(FlagCode::MissingArtifactFile), Severity::Minor);
        assert_eq!(severity_for(FlagCode::NonEnglish), Severity::Minor);
    }

    #[test]
    fn first_severe_strike_suspends_seven_days() {
        let now = Utc::now();
        let result = evaluate(Severity::Severe, now, &[]);
        match result {
            Enforcement::Suspend { until, .. } => {
                assert_eq!((until - now).num_days(), SEVERE_FIRST_SUSPENSION_DAYS);
            }
            other => panic!("expected Suspend, got {other:?}"),
        }
    }

    #[test]
    fn second_severe_strike_within_30d_suspends_thirty_days() {
        let now = Utc::now();
        let history = vec![h(Severity::Severe, 60 * 24, now)]; // 1 day ago
        let result = evaluate(Severity::Severe, now, &history);
        match result {
            Enforcement::Suspend { until, .. } => {
                assert_eq!((until - now).num_days(), SEVERE_SECOND_SUSPENSION_DAYS);
            }
            other => panic!("expected Suspend(30d), got {other:?}"),
        }
    }

    #[test]
    fn third_severe_strike_permanent() {
        let now = Utc::now();
        let history = vec![
            h(Severity::Severe, 60 * 24, now),
            h(Severity::Severe, 60 * 24 * 5, now),
        ];
        let result = evaluate(Severity::Severe, now, &history);
        assert!(matches!(result, Enforcement::PermanentSuspend { .. }));
    }

    #[test]
    fn severe_older_than_30d_does_not_count() {
        let now = Utc::now();
        let history = vec![h(Severity::Severe, 60 * 24 * 45, now)];
        let result = evaluate(Severity::Severe, now, &history);
        match result {
            Enforcement::Suspend { until, .. } => {
                assert_eq!((until - now).num_days(), SEVERE_FIRST_SUSPENSION_DAYS);
            }
            other => panic!("expected first-strike Suspend, got {other:?}"),
        }
    }

    #[test]
    fn major_short_limit_in_24h_suspends_short() {
        let now = Utc::now();
        // N-1 previous within 24h; the new strike crosses the threshold.
        let history: Vec<_> = (0..(MAJOR_WINDOW_SHORT_LIMIT - 1) as i64)
            .map(|i| h(Severity::Major, 30 + i, now)) // all well under 24h
            .collect();
        let result = evaluate(Severity::Major, now, &history);
        match result {
            Enforcement::Suspend { until, .. } => {
                assert_eq!((until - now).num_hours(), MAJOR_SHORT_SUSPENSION_HOURS);
            }
            other => panic!("expected short-suspension, got {other:?}"),
        }
    }

    #[test]
    fn majors_below_short_limit_do_not_escalate() {
        let now = Utc::now();
        // One fewer than the threshold-triggering count (so new strike is
        // still strictly below short + mid + long limits).
        let history: Vec<_> = (0..(MAJOR_WINDOW_SHORT_LIMIT - 2) as i64)
            .map(|i| h(Severity::Major, 30 + i, now))
            .collect();
        assert_eq!(evaluate(Severity::Major, now, &history), Enforcement::None);
    }

    #[test]
    fn minor_short_limit_in_24h_rate_limits() {
        let now = Utc::now();
        let history: Vec<_> = (0..(MINOR_WINDOW_SHORT_LIMIT - 1) as i64)
            .map(|i| h(Severity::Minor, 10 + i, now)) // all under 24h
            .collect();
        let result = evaluate(Severity::Minor, now, &history);
        match result {
            Enforcement::RateLimit { until, .. } => {
                assert_eq!((until - now).num_hours(), MINOR_RATE_LIMIT_HOURS);
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn minor_long_limit_in_30d_suspends() {
        let now = Utc::now();
        // Pack the strikes tightly so they all sit inside the 30d
        // long window regardless of how we raise the limit. (Using
        // one-per-day would push strikes outside the window once the
        // limit exceeds ~30.) This also means short+long thresholds
        // are both met — the code explicitly prefers long, which is
        // the branch under test.
        let history: Vec<_> = (0..(MINOR_WINDOW_LONG_LIMIT - 1) as i64)
            .map(|i| h(Severity::Minor, i, now)) // i minutes ago
            .collect();
        let result = evaluate(Severity::Minor, now, &history);
        match result {
            Enforcement::Suspend { until, .. } => {
                assert_eq!((until - now).num_hours(), MINOR_LONG_SUSPENSION_HOURS);
            }
            other => panic!("expected long-window Suspend, got {other:?}"),
        }
    }

    #[test]
    fn flag_code_roundtrip() {
        for code in [
            FlagCode::SecretDetected,
            FlagCode::Pii,
            FlagCode::UnsafeAction,
            FlagCode::OffTopic,
            FlagCode::LowEffort,
            FlagCode::Spam,
            FlagCode::Duplicate,
            FlagCode::Flooding,
            FlagCode::OpinionPiece,
            FlagCode::PolicyViolation,
            FlagCode::NonEnglish,
            FlagCode::MissingArtifactFile,
        ] {
            assert_eq!(FlagCode::parse(code.as_str()), Some(code));
        }
        assert_eq!(FlagCode::parse("not_a_code"), None);
    }
}
