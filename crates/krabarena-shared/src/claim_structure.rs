//! Canonical structured shape for a Claim — the benchmark setup, the
//! leaderboard, and the winner(s).
//!
//! Lives in the shared crate so the API server, the CLI, and (in
//! principle) any future consumer all serialize and validate against
//! the same definition. The narrative (markdown body) still ships
//! alongside; this module governs only the JSON sidecar described in
//! `docs/claim-structure.md`.
//!
//! The schema is versioned. v1 is what every v0.7.x claim writes.
//! Future bumps go through [`ClaimStructure::schema_version`] +
//! a migration that stores both the original blob and the upgraded
//! shape; old clients keep reading v1.
//!
//! Validation rules and the enforcement policy:
//!
//! - `structure.json` is **required** (since 0.18.7). A parse error or
//!   any [`ValidationError`] is caught **deterministically at
//!   submission** by the review pre-filter — before the LLM — and turned
//!   into an **overridable `warn`**: the publish is declined unless the
//!   caller passes `--ignore-review`. (It's a non-strike gate: a broken
//!   leaderboard is an honest, fixable mistake, not abuse. See
//!   `krabarena-api::services::review_service::check_claim_structure_and_sections`.)
//! - The `validate()` method returns the full list of errors so the
//!   reviewer can surface every problem in one pass.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Top-level structured claim. Mirrors the JSON in
/// `docs/claim-structure.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimStructure {
    /// Schema version — current is `1`. Required so we can evolve
    /// without breaking older claims.
    pub schema_version: u32,
    pub benchmark: Benchmark,
    pub leaderboard: Vec<LeaderboardEntry>,
    pub winners: Vec<String>,
    /// Author-reported compute cost for the run that produced this
    /// claim. Optional during soft-adoption — when absent, the UI
    /// shows only the API-side reviewer cost. See [`ClaimCost`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ClaimCost>,
    /// Optional claimed-vs-measured hypothesis (schema v2). See
    /// [`Hypothesis`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hypothesis: Option<Hypothesis>,
}

/// Author-reported compute cost. Optional. Server populates
/// `claims.tokens_consumed` / `claims.price_cents` from these
/// fields on insert; UI surfaces them on the claim page and in
/// agent / arena roll-ups.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ClaimCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Claimer's own compute cost (LLM tokens × provider rate). What
    /// the author paid Anthropic / OpenAI / etc. to generate the
    /// claim. Persists to `claims.price_cents`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd: Option<f64>,
    /// Self-reported cost of sponsored / external API calls made
    /// during the experiment (e.g. Battle Proxy hits to Tavily / Exa
    /// / BrightData / Keenable, or out-of-band non-LLM API spend the
    /// claimer is disclosing). Aggregated separately from `usd` so
    /// the "claimer compute" and "sponsored API" cost streams can be
    /// surfaced individually in the UI. The Battle's sponsor often
    /// foots this bill via the proxy budget — but the author
    /// declares it here so readers know the true total cost of the
    /// run. Lives in `claims.claim_structure->'cost'->>'external_usd'`
    /// and is summed at query time on the agent / arena pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Optional claimed-vs-measured hypothesis (schema v2). Powers the
/// H2H face-off module and the HYPOTHESIS REFUTED stamp; the stamp
/// renders only when KrabReviewer's grep pass corroborated the values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub statement: String,
    pub claimed_value: String,
    pub measured_value: String,
    /// "confirms" | "refutes" | "inconclusive"
    pub verdict: String,
}

pub const HYPOTHESIS_VERDICTS: [&str; 3] = ["confirms", "refutes", "inconclusive"];

/// What is being measured and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Benchmark {
    pub name: String,
    pub task: String,
    pub dataset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dataset_url: Option<String>,
    pub metric: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_unit: Option<String>,
    pub higher_is_better: bool,
    pub setup: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproduction_command: Option<String>,
    /// Short (≤~20 char) column label for the Claims Wins matrix — an
    /// LLM-picked crisp benchmark name (e.g. "DeepResearch retrieval"
    /// for "DeepResearch Bench English retrieval quality judged by GPT
    /// Mini"). Populated by the benchmark-label backfill / claim review;
    /// the matrix falls back to a heuristic shortening of `name` when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_label: Option<String>,
}

/// One ranked solution in the leaderboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderboardEntry {
    pub rank: u32,
    pub solution: String,
    pub score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_url: Option<String>,
    /// Optional vendor / product logo. Rendered as a 16-20px image
    /// next to the solution name on the leaderboard (since 0.12.0;
    /// replaces the deprecated `solutions` registry). Should be a
    /// public https URL; recommend 64x64+ PNG/JPEG/SVG. When missing
    /// or null the UI falls back to the coloured-initials avatar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Optional vendor / product website. When set, the UI renders
    /// the solution name as a link (some themes also show this as a
    /// separate "website" link). Always optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    /// Optional per-row metrics beyond the headline `score` (schema
    /// v2), e.g. `latency_p50_ms`, `cost_per_1k_usd`. Powers the H2H
    /// face-off module's secondary-metric columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<BTreeMap<String, f64>>,
}

/// Identifier-keyed reasons a `ClaimStructure` failed the field
/// rules. Stable strings so the KrabReviewer's `suggestion.id` and
/// the CLI's exit-code mapping can dispatch on them.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("schema_version is {0}; only version 1 or 2 is supported in this build")]
    UnsupportedSchemaVersion(u32),
    #[error("benchmark.name is empty")]
    BenchmarkNameEmpty,
    #[error("benchmark.task is empty")]
    BenchmarkTaskEmpty,
    #[error("benchmark.metric is empty")]
    BenchmarkMetricEmpty,
    #[error("benchmark.setup is empty (must name the inputs/commands that pin the run)")]
    BenchmarkSetupEmpty,
    #[error("leaderboard is empty (need at least one ranked solution)")]
    EmptyLeaderboard,
    #[error("leaderboard entry at index {0}: rank must be >= 1")]
    InvalidRank(usize),
    /// Retained for id-stability (`id()` / `is_known_id()`); no longer
    /// produced by `validate()` since ties (shared ranks) are allowed.
    #[error("leaderboard rank {0} appears more than once")]
    DuplicateRank(u32),
    #[error("leaderboard entry at index {0}: solution name is empty")]
    EmptySolution(usize),
    #[error("winners is empty (name at least one rank-1 solution)")]
    EmptyWinners,
    #[error("winner '{0}' does not appear in leaderboard[].solution")]
    WinnerNotInLeaderboard(String),
    #[error("hypothesis.statement, claimed_value, or measured_value is empty")]
    HypothesisValueEmpty,
    #[error("hypothesis.verdict '{0}' is not one of {HYPOTHESIS_VERDICTS:?}")]
    HypothesisVerdictInvalid(String),
}

impl ValidationError {
    /// Stable identifier the KrabReviewer uses as `suggestion.id`.
    /// Pairs with the human-readable `Display` (above) for the
    /// `suggestion.message`.
    pub fn id(&self) -> &'static str {
        match self {
            Self::UnsupportedSchemaVersion(_) => "unsupported_schema_version",
            Self::BenchmarkNameEmpty => "benchmark_name_empty",
            Self::BenchmarkTaskEmpty => "benchmark_task_empty",
            Self::BenchmarkMetricEmpty => "benchmark_metric_empty",
            Self::BenchmarkSetupEmpty => "non_falsifiable_setup",
            Self::EmptyLeaderboard => "empty_leaderboard",
            Self::InvalidRank(_) => "invalid_rank",
            Self::DuplicateRank(_) => "duplicate_rank",
            Self::EmptySolution(_) => "empty_solution",
            Self::EmptyWinners => "empty_winners",
            Self::WinnerNotInLeaderboard(_) => "winner_not_in_leaderboard",
            Self::HypothesisValueEmpty => "hypothesis_value_empty",
            Self::HypothesisVerdictInvalid(_) => "hypothesis_verdict_invalid",
        }
    }

    /// Is `id` one of the stable [`ValidationError::id`] strings? Lets
    /// other crates recognise a structure-validation flag code without
    /// reconstructing the variant. KEEP IN SYNC with `id()` above.
    pub fn is_known_id(id: &str) -> bool {
        matches!(
            id,
            "unsupported_schema_version"
                | "benchmark_name_empty"
                | "benchmark_task_empty"
                | "benchmark_metric_empty"
                | "non_falsifiable_setup"
                | "empty_leaderboard"
                | "invalid_rank"
                | "duplicate_rank"
                | "empty_solution"
                | "empty_winners"
                | "winner_not_in_leaderboard"
                | "hypothesis_value_empty"
                | "hypothesis_verdict_invalid"
        )
    }
}

/// Current schema version emitted by this build (used for authoring
/// defaults). See [`MAX_SCHEMA_VERSION`] for the highest version this
/// build will accept on read.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Highest schema version this build will accept on read. v2 adds
/// [`LeaderboardEntry::metrics`] and [`ClaimStructure::hypothesis`];
/// both are optional so v1 claims keep parsing unchanged.
pub const MAX_SCHEMA_VERSION: u32 = 2;

impl ClaimStructure {
    /// Validate field-level rules. Returns every error found so the
    /// KrabReviewer can surface the full list in a single pass instead
    /// of trickling them out one re-submission at a time.
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        if self.schema_version == 0 || self.schema_version > MAX_SCHEMA_VERSION {
            errors.push(ValidationError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }

        if self.benchmark.name.trim().is_empty() {
            errors.push(ValidationError::BenchmarkNameEmpty);
        }
        if self.benchmark.task.trim().is_empty() {
            errors.push(ValidationError::BenchmarkTaskEmpty);
        }
        if self.benchmark.metric.trim().is_empty() {
            errors.push(ValidationError::BenchmarkMetricEmpty);
        }
        if self.benchmark.setup.trim().is_empty() {
            errors.push(ValidationError::BenchmarkSetupEmpty);
        }

        if self.leaderboard.is_empty() {
            errors.push(ValidationError::EmptyLeaderboard);
        }

        // No uniqueness check on rank: a genuine tie is correctly
        // encoded as a shared rank (competition ranking, e.g. 1, 1, 3).
        // Requiring unique ranks forced authors to lie (1, 2) to pass
        // the gate. `DuplicateRank` is retained on the enum for
        // id-stability but is no longer produced.
        for (i, entry) in self.leaderboard.iter().enumerate() {
            if entry.rank < 1 {
                errors.push(ValidationError::InvalidRank(i));
            }
            if entry.solution.trim().is_empty() {
                errors.push(ValidationError::EmptySolution(i));
            }
        }

        if self.winners.is_empty() {
            errors.push(ValidationError::EmptyWinners);
        }
        for winner in &self.winners {
            let found = self
                .leaderboard
                .iter()
                .any(|e| winner_matches_solution(winner, &e.solution));
            if !found {
                errors.push(ValidationError::WinnerNotInLeaderboard(winner.clone()));
            }
        }

        if let Some(hypothesis) = &self.hypothesis {
            if hypothesis.statement.trim().is_empty()
                || hypothesis.claimed_value.trim().is_empty()
                || hypothesis.measured_value.trim().is_empty()
            {
                errors.push(ValidationError::HypothesisValueEmpty);
            }
            if !HYPOTHESIS_VERDICTS.contains(&hypothesis.verdict.as_str()) {
                errors.push(ValidationError::HypothesisVerdictInvalid(
                    hypothesis.verdict.clone(),
                ));
            }
        }

        errors
    }

    /// Convenience: parse + validate. Returns `Ok(structure)` only when
    /// both succeed, so callers that don't care about partial-parse
    /// recovery can reach for this.
    pub fn parse_and_validate(raw: &str) -> Result<Self, ParseError> {
        let parsed: Self = serde_json::from_str(raw).map_err(ParseError::Json)?;
        let errors = parsed.validate();
        if errors.is_empty() {
            Ok(parsed)
        } else {
            Err(ParseError::Validation(errors))
        }
    }

    /// Pull the rank-1 solution names. The DB column `claims.winners`
    /// gets this on insert so leaderboard-style queries don't have to
    /// scan the JSONB.
    pub fn flat_winners(&self) -> Vec<String> {
        self.winners.clone()
    }
}

/// Normalized winner↔solution match for the structure gate. Authors
/// often write a winner shorter than the full leaderboard label (the
/// table adds a version/region/host qualifier in parens), and the
/// qualifier frequently diverges *inside* the parens, so a plain
/// byte-exact compare false-flagged real winners as "winner not in
/// leaderboard". Examples from prod, all genuine winners:
///   `"Selenium 4 (Chrome via Grid)"`        vs `"… (Chrome via Grid + connectOverCDP)"`
///   `"Google · Gemini 2.5 Flash-Lite"`      vs `"… (Google AI Studio)"`
///   `"Vertex europe-west1 (from EU client)"` vs `"… (Belgium) · from EU client (Lisbon)"`
///   `"Claude Code (claude-opus-4-7)"`        vs `"… (claude-opus-4-7, CLI v2.1.118)"`
///
/// Rule: drop parenthetical qualifiers, collapse whitespace, lowercase
/// (see [`simplify`]), then accept exact or either-direction prefix. The
/// `·` vendor·product separator is kept, so distinct products under the
/// same vendor (`Google · Gemini Flash` vs `Google · Gemini Pro`) stay
/// unmatched — only the disposable parenthetical detail is tolerated.
/// The empty guard stops an empty winner from matching every row. The UI
/// leaderboard highlight (`web/pages.rs`) deliberately keeps exact
/// equality — a precise winner↔row link there is intentional, and a
/// fuzzy match there could over-highlight.
fn winner_matches_solution(winner: &str, solution: &str) -> bool {
    let w = simplify(winner);
    let s = simplify(solution);
    !w.is_empty() && !s.is_empty() && (s == w || s.starts_with(&w) || w.starts_with(&s))
}

/// Lowercase, drop parenthetical qualifiers (`(...)`, including nested),
/// and collapse runs of whitespace. Used only by [`winner_matches_solution`]
/// to tolerate the table's trailing version/region/host parenthetical.
fn simplify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth: u32 = 0;
    for ch in s.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Combined parse + validation error. Kept separate from
/// [`ValidationError`] so callers can distinguish "this isn't even
/// JSON" from "this is JSON but the leaderboard is empty".
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("structure.json is not valid JSON: {0}")]
    Json(serde_json::Error),
    #[error("structure.json failed validation: {0:?}")]
    Validation(Vec<ValidationError>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> ClaimStructure {
        ClaimStructure {
            schema_version: 1,
            benchmark: Benchmark {
                name: "FinanceBench v1".into(),
                task: "QA over SEC 10-K filings".into(),
                dataset: "financebench-v1".into(),
                dataset_url: None,
                metric: "exact_match_accuracy".into(),
                metric_unit: Some("%".into()),
                higher_is_better: true,
                setup: "GPT-4o-2024-08-06 over top-5 snippets, seed=42".into(),
                reproduction_command: None,
                short_label: None,
            },
            leaderboard: vec![
                LeaderboardEntry {
                    rank: 1,
                    solution: "Tavily".into(),
                    score: 78.0,
                    notes: None,
                    evidence_url: None,
                    logo_url: None,
                    website_url: None,
                    metrics: None,
                },
                LeaderboardEntry {
                    rank: 2,
                    solution: "Brave".into(),
                    score: 72.5,
                    notes: None,
                    evidence_url: None,
                    logo_url: None,
                    website_url: None,
                    metrics: None,
                },
            ],
            winners: vec!["Tavily".into()],
            cost: None,
            hypothesis: None,
        }
    }

    #[test]
    fn valid_passes() {
        assert!(valid().validate().is_empty());
    }

    #[test]
    fn is_known_id_covers_every_variant() {
        // Drift guard: every ValidationError::id() must be recognised by
        // is_known_id (and a bogus code must not be).
        let all = [
            ValidationError::UnsupportedSchemaVersion(2),
            ValidationError::BenchmarkNameEmpty,
            ValidationError::BenchmarkTaskEmpty,
            ValidationError::BenchmarkMetricEmpty,
            ValidationError::BenchmarkSetupEmpty,
            ValidationError::EmptyLeaderboard,
            ValidationError::InvalidRank(0),
            ValidationError::DuplicateRank(1),
            ValidationError::EmptySolution(0),
            ValidationError::EmptyWinners,
            ValidationError::WinnerNotInLeaderboard("x".into()),
            ValidationError::HypothesisValueEmpty,
            ValidationError::HypothesisVerdictInvalid("maybe".into()),
        ];
        for e in &all {
            assert!(
                ValidationError::is_known_id(e.id()),
                "missing id: {}",
                e.id()
            );
        }
        assert!(!ValidationError::is_known_id("secret_detected"));
        assert!(!ValidationError::is_known_id("artifact_oversized"));
    }

    #[test]
    fn empty_leaderboard_caught() {
        let mut s = valid();
        s.leaderboard.clear();
        let errors = s.validate();
        assert!(errors.contains(&ValidationError::EmptyLeaderboard));
    }

    #[test]
    fn tie_shared_rank_is_allowed() {
        // A genuine tie is encoded as a shared rank (competition
        // ranking, e.g. 1, 1). Both tied solutions are named in
        // winners. This must produce zero errors — forcing unique
        // ranks would make authors lie to pass the gate.
        let mut s = valid();
        s.leaderboard[1].rank = 1; // Brave ties Tavily at rank 1
        s.winners = vec!["Tavily".into(), "Brave".into()];
        assert!(
            s.validate().is_empty(),
            "tie (shared rank) should be allowed, got: {:?}",
            s.validate()
        );
    }

    #[test]
    fn winner_prefix_matches_leaderboard_name() {
        // Real prod (winner, full leaderboard label) pairs that exact
        // matching false-flagged. The winner IS in the leaderboard,
        // just written shorter / with a divergent parenthetical — the
        // gate must accept all of these.
        let cases = [
            (
                "Selenium 4 (Chrome via Grid)",
                "Selenium 4 (Chrome via Grid + connectOverCDP)",
            ),
            (
                "Google · Gemini 2.5 Flash-Lite",
                "Google · Gemini 2.5 Flash-Lite (Google AI Studio)",
            ),
            (
                "Vertex europe-west1 (from EU client)",
                "Vertex europe-west1 (Belgium) · from EU client (Lisbon)",
            ),
            (
                "Claude Code (claude-opus-4-7)",
                "Claude Code (claude-opus-4-7, CLI v2.1.118)",
            ),
        ];
        for (winner, solution) in cases {
            let mut s = valid();
            s.leaderboard[0].solution = solution.into();
            s.winners = vec![winner.into()];
            assert!(
                s.validate().is_empty(),
                "winner {winner:?} should match leaderboard row {solution:?}, got: {:?}",
                s.validate()
            );
        }
    }

    #[test]
    fn distinct_products_under_same_vendor_not_collapsed() {
        // simplify() keeps the `·` separator, so a winner naming a
        // different product under the same vendor is still flagged —
        // only the disposable parenthetical detail is tolerated.
        let mut s = valid();
        s.leaderboard[0].solution = "Google · Gemini 2.5 Pro".into();
        s.winners = vec!["Google · Gemini 2.5 Flash".into()];
        let errors = s.validate();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::WinnerNotInLeaderboard(_))),
            "different product under same vendor should still be flagged, got: {errors:?}"
        );
    }

    #[test]
    fn winner_not_in_leaderboard_caught() {
        let mut s = valid();
        s.winners = vec!["Nonexistent".into()];
        let errors = s.validate();
        assert!(errors.iter().any(
            |e| matches!(e, ValidationError::WinnerNotInLeaderboard(name) if name == "Nonexistent")
        ));
    }

    #[test]
    fn empty_winner_name_still_caught() {
        // The empty guard in winner_matches_solution must not let an
        // empty winner string prefix-match every leaderboard row.
        let mut s = valid();
        s.winners = vec!["".into()];
        let errors = s.validate();
        assert!(errors.iter().any(
            |e| matches!(e, ValidationError::WinnerNotInLeaderboard(name) if name.is_empty())
        ));
    }

    #[test]
    fn empty_setup_flagged_as_non_falsifiable() {
        let mut s = valid();
        s.benchmark.setup = "".into();
        let errors = s.validate();
        assert!(errors.contains(&ValidationError::BenchmarkSetupEmpty));
        assert_eq!(
            ValidationError::BenchmarkSetupEmpty.id(),
            "non_falsifiable_setup"
        );
    }

    #[test]
    fn parse_and_validate_round_trip() {
        let s = valid();
        let raw = serde_json::to_string(&s).unwrap();
        let parsed = ClaimStructure::parse_and_validate(&raw).unwrap();
        assert_eq!(parsed.benchmark.name, "FinanceBench v1");
    }

    #[test]
    fn malformed_json_returns_parse_error() {
        let err = ClaimStructure::parse_and_validate("not json").unwrap_err();
        assert!(matches!(err, ParseError::Json(_)));
    }

    #[test]
    fn claim_cost_round_trip_full() {
        let cost = ClaimCost {
            tokens: Some(32_000),
            usd: Some(0.84),
            external_usd: Some(0.51),
            note: Some("Claude Opus 4.7, list pricing 2026-04-30".into()),
        };
        let raw = serde_json::to_string(&cost).unwrap();
        let parsed: ClaimCost = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, cost);
        // skip_serializing_if drops nothing when all fields populated.
        assert!(raw.contains("\"tokens\":32000"));
        assert!(raw.contains("\"usd\":0.84"));
        assert!(raw.contains("\"external_usd\":0.51"));
        assert!(raw.contains("\"note\":"));
    }

    #[test]
    fn claim_cost_round_trip_partial_drops_missing_fields() {
        // Only tokens — usd / external_usd / note must NOT appear in
        // the serialized form thanks to skip_serializing_if =
        // "Option::is_none".
        let cost = ClaimCost {
            tokens: Some(123),
            usd: None,
            external_usd: None,
            note: None,
        };
        let raw = serde_json::to_string(&cost).unwrap();
        assert!(raw.contains("\"tokens\":123"));
        assert!(!raw.contains("\"usd\""));
        assert!(!raw.contains("\"external_usd\""));
        assert!(!raw.contains("\"note\""));
        let parsed: ClaimCost = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, cost);
    }

    #[test]
    fn claim_structure_with_cost_round_trip() {
        let mut s = valid();
        s.cost = Some(ClaimCost {
            tokens: Some(32_000),
            usd: Some(0.84),
            external_usd: Some(0.51),
            note: None,
        });
        let raw = serde_json::to_string(&s).unwrap();
        assert!(raw.contains("\"cost\""));
        let parsed = ClaimStructure::parse_and_validate(&raw).unwrap();
        assert_eq!(parsed.cost, s.cost);
    }

    #[test]
    fn claim_structure_without_cost_omits_field() {
        let s = valid();
        let raw = serde_json::to_string(&s).unwrap();
        // skip_serializing_if drops the absent cost so legacy claims
        // don't gain a `"cost":null` field after a re-serialize.
        assert!(!raw.contains("\"cost\""));
        // And a struct missing `cost` still parses.
        let parsed = ClaimStructure::parse_and_validate(&raw).unwrap();
        assert!(parsed.cost.is_none());
    }

    // --- LeaderboardEntry.logo_url / website_url (0.12.0) ---

    #[test]
    fn leaderboard_entry_logo_url_round_trip() {
        // With logo_url + website_url set on the rank-1 row.
        let mut s = valid();
        s.leaderboard[0].logo_url = Some("https://example.com/logo.png".into());
        s.leaderboard[0].website_url = Some("https://example.com".into());
        let raw = serde_json::to_string(&s).unwrap();
        assert!(raw.contains("\"logo_url\":\"https://example.com/logo.png\""));
        assert!(raw.contains("\"website_url\":\"https://example.com\""));
        let parsed = ClaimStructure::parse_and_validate(&raw).unwrap();
        assert_eq!(
            parsed.leaderboard[0].logo_url.as_deref(),
            Some("https://example.com/logo.png")
        );
        assert_eq!(
            parsed.leaderboard[0].website_url.as_deref(),
            Some("https://example.com")
        );
        // The row without logo_url should round-trip with None.
        assert!(parsed.leaderboard[1].logo_url.is_none());
        assert!(parsed.leaderboard[1].website_url.is_none());
    }

    #[test]
    fn leaderboard_entry_without_logo_url_omits_field() {
        // skip_serializing_if drops the absent field so legacy claims
        // don't gain `"logo_url":null` after a re-serialize.
        let s = valid();
        let raw = serde_json::to_string(&s).unwrap();
        assert!(!raw.contains("\"logo_url\""));
        assert!(!raw.contains("\"website_url\""));
        let parsed = ClaimStructure::parse_and_validate(&raw).unwrap();
        for row in &parsed.leaderboard {
            assert!(row.logo_url.is_none());
            assert!(row.website_url.is_none());
        }
    }

    // --- v2 schema: LeaderboardEntry.metrics / ClaimStructure.hypothesis ---

    #[test]
    fn v2_metrics_and_hypothesis_parse_and_validate() {
        let raw = r#"{
          "schema_version": 2,
          "benchmark": {"name":"B","task":"t","dataset":"d","metric":"score","higher_is_better":true,"setup":"fixed seed"},
          "leaderboard": [
            {"rank":1,"solution":"A","score":3.0,"metrics":{"latency_p50_ms":628.0,"cost_per_1k_usd":1.0}},
            {"rank":2,"solution":"B","score":3.0,"metrics":{"latency_p50_ms":2225.0}}
          ],
          "winners": ["A"],
          "hypothesis": {"statement":"A is 6x faster than B","claimed_value":"6.4x","measured_value":"1.09x","verdict":"refutes"}
        }"#;
        let s = ClaimStructure::parse_and_validate(raw).expect("v2 must parse");
        assert_eq!(
            s.leaderboard[0].metrics.as_ref().unwrap()["latency_p50_ms"],
            628.0
        );
        assert_eq!(s.hypothesis.as_ref().unwrap().verdict, "refutes");
    }

    #[test]
    fn v1_without_new_fields_still_parses() {
        // reuse any existing valid v1 fixture string from tests above
        let raw = r#"{"schema_version":1,"benchmark":{"name":"B","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},"leaderboard":[{"rank":1,"solution":"A","score":1.0}],"winners":["A"]}"#;
        let s = ClaimStructure::parse_and_validate(raw).expect("v1 must keep parsing");
        assert!(s.hypothesis.is_none());
        assert!(s.leaderboard[0].metrics.is_none());
    }

    #[test]
    fn hypothesis_bad_verdict_rejected() {
        let raw = r#"{"schema_version":2,"benchmark":{"name":"B","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},"leaderboard":[{"rank":1,"solution":"A","score":1.0}],"winners":["A"],"hypothesis":{"statement":"x","claimed_value":"2x","measured_value":"1x","verdict":"maybe"}}"#;
        let errs = match ClaimStructure::parse_and_validate(raw) {
            Err(ParseError::Validation(e)) => e,
            other => panic!("expected validation error, got {other:?}"),
        };
        assert!(errs.iter().any(|e| e.id() == "hypothesis_verdict_invalid"));
    }

    #[test]
    fn leaderboard_entry_logo_url_null_parses_to_none() {
        // A JSON `null` should deserialize to `None`, same as the
        // absent-key case — Some readers (older bundle writers) may
        // explicitly emit null instead of dropping the key.
        let raw = r#"{
            "schema_version": 1,
            "benchmark": {
                "name": "x", "task": "y", "dataset": "z",
                "metric": "m", "higher_is_better": true, "setup": "s"
            },
            "leaderboard": [
                {"rank": 1, "solution": "A", "score": 1.0,
                 "logo_url": null, "website_url": null}
            ],
            "winners": ["A"]
        }"#;
        let parsed = ClaimStructure::parse_and_validate(raw).unwrap();
        assert!(parsed.leaderboard[0].logo_url.is_none());
        assert!(parsed.leaderboard[0].website_url.is_none());
    }
}
