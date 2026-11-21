use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The verdict resolved server-side from the model's structured output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewVerdict {
    Pass,
    Warn,
    Block,
}

impl ReviewVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewVerdict::Pass => "pass",
            ReviewVerdict::Warn => "warn",
            ReviewVerdict::Block => "block",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pass" => Some(Self::Pass),
            "warn" => Some(Self::Warn),
            "block" => Some(Self::Block),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub id: String,
    pub severity: String, // "info" | "warn" | "block"
    pub category: String, // falsifiability | alignment | ... | engagement | moderation
    pub message: String,
    pub suggestion: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>, // "claim" | "artifact" | "both"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autofix_hint: Option<String>, // machine-readable fix hint
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationFlag {
    pub code: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>, // file path within artifact (for archives)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>, // line number
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autofix_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Highlight {
    pub label: String,
    pub detail: String,
}

/// Headline number for a claim's viral share card (e.g. "6.4x" faster).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeroStat {
    pub value: String,
    pub label: String,
}

/// Claimed-vs-measured contrast surfaced when a claim's headline number
/// didn't hold up under review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Refutation {
    pub claimed: String,
    pub measured: String,
}

/// Reviewer-generated "presentation" object driving the claim's viral
/// share layer (OG card layout + tweet copy). Persisted alongside
/// `og_summary` on both `claim_reviews` (review-of-record) and `claims`
/// (render-time reads) — see migration 067.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Presentation {
    pub layout: String,
    pub tweet_title: String,
    pub tweet_annotation: String,
    #[serde(default)]
    pub hero_stat: Option<HeroStat>,
    #[serde(default)]
    pub refutation: Option<Refutation>,
}

/// The structured output Gemini is instructed to return.  Mirrors the JSON
/// schema declared in `gemini_service::response_schema()`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReviewOutcome {
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub artifact_summary: Option<String>,
    #[serde(default)]
    pub artifact_highlights: Vec<Highlight>,
    #[serde(default)]
    pub suggestions: Vec<Suggestion>,
    #[serde(default)]
    pub moderation_flags: Vec<ModerationFlag>,
    // `battle_executive_summary` was removed from this struct in
    // 0.8.15 — the reviewer no longer produces summaries as a side
    // effect; battle_summarizer::regenerate_summary owns that work
    // now (called incrementally from the claim post handler).
    /// Short (~200 char) social-card summary for `og:description` /
    /// `twitter:description`. Written to `claims.og_summary` /
    /// `claim_reviews.og_summary` (migration 067).
    #[serde(default)]
    pub og_summary: Option<String>,
    /// Viral share layer object — see [`Presentation`]. Written to
    /// `claims.presentation` / `claim_reviews.presentation` (migration
    /// 067).
    #[serde(default)]
    pub presentation: Option<Presentation>,
}

/// DB row for `claim_reviews`.
///
/// `prompt_tokens` / `completion_tokens` / `cost_micros` are the
/// LLM-cost columns added in migration 022 (v0.9.0). They are NULL
/// for rows persisted without a Gemini call (prefilter blocks,
/// synthetic-pass dev mode) and for any row inserted before
/// migration 022 was applied.
///
/// `review_target` discriminates the four review surfaces sharing
/// this table: `claim` (default, the original use), `verify`,
/// `refute`, `battle_summary`. `verification_id` ties verify/refute
/// rows back to the verifications row they reviewed.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimReviewRow {
    pub id: Uuid,
    /// `None` for standalone-claim reviews (migration 035 — the
    /// column lost its NOT NULL when claims learned to live without
    /// a Battle).
    #[sqlx(default)]
    pub battle_id: Option<Uuid>,
    pub user_id: Uuid,
    pub claim: String,
    pub artifact_hash: String,
    pub artifact_size: i64,
    pub model: String,
    pub verdict: String,
    pub summary: Option<String>,
    pub artifact_summary: Option<String>,
    pub artifact_highlights: Option<serde_json::Value>,
    pub suggestions: serde_json::Value,
    pub moderation_flags: serde_json::Value,
    pub latency_ms: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub cost_micros: Option<i64>,
    pub review_target: String,
    pub verification_id: Option<Uuid>,
    /// Reviewer-generated social-card summary (migration 067). NULL
    /// for rows inserted before this release and for verify/refute
    /// reviews, which don't carry a presentation layer.
    #[sqlx(default)]
    pub og_summary: Option<String>,
    /// Raw JSONB [`Presentation`] payload (migration 067). Parsed
    /// into the typed struct at the `ReviewResponse` boundary.
    #[sqlx(default)]
    pub presentation: Option<serde_json::Value>,
}

/// JSON shape returned from the review endpoints.
#[derive(Debug, Serialize)]
pub struct ReviewResponse {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battle_id: Option<Uuid>,
    pub user_id: Uuid,
    pub claim: String,
    pub artifact_hash: String,
    pub artifact_size: i64,
    pub model: String,
    pub verdict: String,
    pub summary: Option<String>,
    pub artifact_summary: Option<String>,
    pub artifact_highlights: Vec<Highlight>,
    pub suggestions: Vec<Suggestion>,
    pub moderation_flags: Vec<ModerationFlag>,
    pub latency_ms: Option<i32>,
    pub created_at: DateTime<Utc>,
    /// LLM cost telemetry (NULL for prefilter / synthetic-pass rows
    /// and for any historical row inserted before migration 022).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<i64>,
    pub review_target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_id: Option<Uuid>,
    /// Reviewer-generated social-card summary (migration 067).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub og_summary: Option<String>,
    /// Viral share layer object (migration 067). `None` when the
    /// reviewer didn't emit one, when grounding sanitization dropped
    /// it, or for reviews from before this release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<Presentation>,
    /// CLI-friendly hints for how to proceed.
    pub next_actions: NextActions,
    /// Permanent URLs for the review and related resources.
    pub urls: ReviewUrls,
    // `battle_executive_summary` was removed in 0.8.15 — the
    // reviewer no longer produces summaries; the claim post
    // handler spawns an incremental
    // `battle_summarizer::regenerate_summary(.., Some(claim_id))`
    // after every successful publish.
}

#[derive(Debug, Serialize)]
pub struct ReviewUrls {
    pub review_url: String,
    /// `None` for standalone-claim reviews (no Battle attached).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battle_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NextActions {
    /// True if the caller can publish this Claim (pass, or warn that they can override).
    pub can_publish: bool,
    /// True if the Claim is blocked and no client flag overrides it.
    pub blocked: bool,
    /// True if the caller must pass --ignore-review or revise before publishing.
    pub needs_revision_or_override: bool,
}

impl From<ClaimReviewRow> for ReviewResponse {
    fn from(row: ClaimReviewRow) -> Self {
        let verdict = ReviewVerdict::parse(&row.verdict).unwrap_or(ReviewVerdict::Pass);
        let suggestions: Vec<Suggestion> =
            serde_json::from_value(row.suggestions).unwrap_or_default();
        let moderation_flags: Vec<ModerationFlag> =
            serde_json::from_value(row.moderation_flags).unwrap_or_default();
        let artifact_highlights: Vec<Highlight> = row
            .artifact_highlights
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let presentation: Option<Presentation> = row
            .presentation
            .and_then(|v| serde_json::from_value(v).ok());

        let blocked = verdict == ReviewVerdict::Block;
        let needs_revision_or_override = verdict == ReviewVerdict::Warn;

        Self {
            id: row.id,
            battle_id: row.battle_id,
            user_id: row.user_id,
            claim: row.claim,
            artifact_hash: row.artifact_hash,
            artifact_size: row.artifact_size,
            model: row.model,
            verdict: verdict.as_str().to_string(),
            summary: row.summary,
            artifact_summary: row.artifact_summary,
            artifact_highlights,
            suggestions,
            moderation_flags,
            latency_ms: row.latency_ms,
            created_at: row.created_at,
            prompt_tokens: row.prompt_tokens,
            completion_tokens: row.completion_tokens,
            cost_micros: row.cost_micros,
            review_target: row.review_target,
            verification_id: row.verification_id,
            og_summary: row.og_summary,
            presentation,
            next_actions: NextActions {
                can_publish: !blocked,
                blocked,
                needs_revision_or_override,
            },
            urls: ReviewUrls {
                review_url: format!("https://krabarena.com/reviews/{}", row.id),
                battle_url: row
                    .battle_id
                    .map(|bid| format!("https://krabarena.com/battles/{}", bid)),
            },
        }
    }
}

/// DB row for `moderation_strikes`.
#[derive(Debug, Clone, sqlx::FromRow)]
#[allow(dead_code)] // `user_id` is used in queries but not surfaced externally
pub struct ModerationStrikeRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub review_id: Uuid,
    pub code: String,
    pub severity: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ModerationStatus {
    pub strikes: Vec<ModerationStrike>,
    pub active_suspension: Option<SuspensionInfo>,
    pub active_rate_limit: Option<RateLimitInfo>,
}

#[derive(Debug, Serialize)]
pub struct ModerationStrike {
    pub id: Uuid,
    pub review_id: Uuid,
    pub code: String,
    pub severity: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<ModerationStrikeRow> for ModerationStrike {
    fn from(row: ModerationStrikeRow) -> Self {
        Self {
            id: row.id,
            review_id: row.review_id,
            code: row.code,
            severity: row.severity,
            reason: row.reason,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SuspensionInfo {
    pub until: DateTime<Utc>,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct RateLimitInfo {
    pub until: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_roundtrips_and_tolerates_missing_optionals() {
        let raw = r#"{"layout":"h2h","tweet_title":"t","tweet_annotation":"a",
                  "refutation":{"claimed":"6.4x","measured":"1.09x"}}"#;
        let p: Presentation = serde_json::from_str(raw).unwrap();
        assert_eq!(p.layout, "h2h");
        assert!(p.hero_stat.is_none());
        assert_eq!(p.refutation.unwrap().measured, "1.09x");
    }
}
