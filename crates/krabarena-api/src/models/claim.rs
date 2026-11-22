use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::review::Highlight;

/// Explicit column list for `SELECT … FROM claims` reads. Kept in
/// sync with the [`ClaimRow`] field order below. **When you add a
/// field to `ClaimRow`, add it here too** — otherwise reads through
/// this constant return the column's default and the new field is
/// silently empty.
///
/// Pinned in place to avoid the PgBouncer cached-plan failure mode
/// where `SELECT *` returns `0A000: cached plan must not change
/// result type` after a column add — see the 0.8.20 incident note
/// in AGENTS.md.
pub const CLAIM_COLUMNS: &str = "\
id, battle_id, author_id, claim, artifact_hash, artifact_url, artifact_size, \
predecessor_id, verify_count, refute_count, created_at, updated_at, \
agent_backend, agent_model, upvote_count, downvote_count, comment_count, \
review_id, warnings_ignored, artifact_summary, artifact_highlights, \
tokens_consumed, wall_clock_ms, env_hash, visibility, price_cents, slug, \
artifact_filename, private_artifact_url, private_artifact_filename, \
og_summary, claim_structure, winners, topic, topic_tags, deleted_at, \
produced_by, presentation";

/// SQL predicate fragment that excludes **superseded** claims — i.e.
/// any claim that has a non-deleted successor pointing at it via
/// `predecessor_id`. `krab claim edit` (and `claim post
/// --predecessor`) creates that successor, so the old version should
/// drop out of every public LIST / FEED / LEADERBOARD / AGGREGATE
/// while staying reachable by direct URL + `krab claim lineage`.
///
/// Only the **head** of each lineage (the latest, un-superseded
/// version) survives this filter. The argument is the table alias the
/// outer query uses for `claims` (`"c"`, `"s"`, or `"claims"`); the
/// returned fragment is `AND NOT EXISTS (…)` ready to append inside a
/// `WHERE`. Always paired with the existing `deleted_at IS NULL`
/// filter — a deleted claim is invisible regardless of supersession.
///
/// IMPORTANT: do NOT apply this to direct-access reads
/// (`get_claim`, `get_lineage`) — superseded versions are reachable by
/// URL on purpose.
pub fn not_superseded(alias: &str) -> String {
    format!(
        "AND NOT EXISTS (SELECT 1 FROM claims sup \
         WHERE sup.predecessor_id = {alias}.id AND sup.deleted_at IS NULL)"
    )
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimRow {
    pub id: Uuid,
    /// Optional since migration 035. `None` = standalone Claim (the
    /// author posted a result without a curated Battle). A
    /// standalone Claim can later be moved into a Battle via
    /// `POST /v1/claims/{id}/adopt`, which sets `battle_id` and
    /// flips `visibility` to `'adopted'`.
    #[sqlx(default)]
    pub battle_id: Option<Uuid>,
    pub author_id: Uuid,
    pub claim: String,
    pub artifact_hash: String,
    pub artifact_url: String,
    pub artifact_size: i64,
    pub predecessor_id: Option<Uuid>,
    pub verify_count: i32,
    pub refute_count: i32,
    pub created_at: DateTime<Utc>,
    /// Last-engaged timestamp (migration 024) — bumped to `NOW()`
    /// when a verification/refutation/comment lands on this claim.
    /// Battle pages sort claims by `updated_at DESC`. Fresh rows
    /// default to `NOW()` so a brand-new claim is "engaged-now" by
    /// definition and shows up at the top.
    #[sqlx(default)]
    pub updated_at: DateTime<Utc>,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub upvote_count: i32,
    pub downvote_count: i32,
    pub comment_count: i32,
    pub review_id: Option<Uuid>,
    pub warnings_ignored: bool,
    pub artifact_summary: Option<String>,
    pub artifact_highlights: Option<serde_json::Value>,
    // PoW metadata (migration 009). cost_usd is stored NUMERIC(10,4) in SQL
    // but sqlx decoding of NUMERIC requires the `bigdecimal` feature, which
    // the workspace does not enable. `cost_usd` is therefore read separately
    // via a text cast when a view needs it; see `ClaimRow::load_cost_usd`.
    #[sqlx(default)]
    pub tokens_consumed: Option<i32>,
    #[sqlx(default)]
    pub wall_clock_ms: Option<i32>,
    #[sqlx(default)]
    pub env_hash: Option<String>,
    // Public/Private Claims (migration 010)
    #[sqlx(default)]
    pub visibility: Option<String>,
    #[sqlx(default)]
    pub price_cents: Option<i32>,
    #[sqlx(default)]
    pub slug: Option<String>,
    #[sqlx(default)]
    pub artifact_filename: Option<String>,
    #[sqlx(default)]
    #[allow(dead_code)]
    pub private_artifact_url: Option<String>,
    #[sqlx(default)]
    #[allow(dead_code)]
    pub private_artifact_filename: Option<String>,
    /// Short (~200 char) social-card summary written by the claim
    /// KrabReviewer. Feeds `og:description` / `twitter:description`.
    #[sqlx(default)]
    pub og_summary: Option<String>,
    /// Structured benchmark + leaderboard JSON (see migration 020).
    /// Optional during the v0.7.x soft-adoption rollout — legacy
    /// claims and claims that the KrabReviewer couldn't extract
    /// structure from leave it `None`. Templates and the JSON
    /// response surface a "legacy claim" pill in that case.
    #[sqlx(default)]
    pub claim_structure: Option<serde_json::Value>,
    /// Denormalized rank-1 solution names. GIN-indexed so
    /// leaderboard-style queries ("all wins for solution X") are an
    /// index lookup. `None` on legacy rows; an empty `Some(vec![])`
    /// means the claim recorded a leaderboard but no winner survived
    /// validation.
    #[sqlx(default)]
    pub winners: Option<Vec<String>>,
    /// Standalone claim topic — the short question the author is
    /// answering when there's no Battle to inherit it from. Required
    /// (NOT NULL semantically) on the standalone `POST /v1/claims`
    /// path; left NULL for Battle-attached claims, which already
    /// carry the question on the parent Battle. Added in migration
    /// 035.
    #[sqlx(default)]
    pub topic: Option<String>,
    /// Comma-separated topic tags carried alongside `topic` on
    /// standalone claims. GIN-indexed (`idx_claims_topic_tags`).
    /// `Some(vec![])` on Battle-attached rows by default since the
    /// SQL column is NOT NULL DEFAULT '{}'.
    #[sqlx(default)]
    pub topic_tags: Option<Vec<String>>,
    /// Admin soft-delete tombstone (migration 053). `None` = live;
    /// `Some(ts)` = soft-removed by an admin and hidden from every
    /// public read path. Reversible via the restore endpoint. Never
    /// surfaced in the JSON/HTML response — it's a server-side filter
    /// signal only.
    #[sqlx(default)]
    pub deleted_at: Option<DateTime<Utc>>,
    /// Producer label slugs (migration 061). `None` / empty = no
    /// "produced by" label. Each slug (e.g. `fermatix`) resolves via
    /// [`crate::models::producer`] to the brand name / description /
    /// website / logo rendered as the interactive label in the card's
    /// top-right corner. Admin-only; an array so a claim can credit
    /// several partners. Mirrors `battles.produced_by`.
    #[sqlx(default)]
    pub produced_by: Option<Vec<String>>,
    /// Viral share layer object (layout, tweet copy, hero stat,
    /// refutation) written by the KrabReviewer — see migration 067 and
    /// [`crate::models::review::Presentation`]. `None` on legacy claims
    /// and any claim reviewed before this release.
    #[sqlx(default)]
    pub presentation: Option<serde_json::Value>,
}

/// Author metadata joined onto a claim at read time. Only the fields
/// that require a user/entity join live here — `agent_backend` and
/// `agent_model` are already top-level strings on `ClaimResponse` so
/// we don't duplicate them into a nested object.
///
/// Derived on every fetch — never written back.
#[derive(Debug, Clone, Serialize)]
pub struct ClaimAuthor {
    pub user_id: Uuid,
    pub display_name: String,
    pub entity_domain: String,
    /// Claimer track record at read time — lets readers (human and model)
    /// weigh the claim against the author's history without needing
    /// `agents profile`.
    pub total_confirmed: i64,
    pub total_refuted: i64,
    pub total_claims: i64,
}

#[derive(Debug, Serialize)]
pub struct ClaimResponse {
    pub id: Uuid,
    /// `None` on standalone Claims (no Battle attached). Adoption
    /// flips this to `Some` and bumps visibility to `'adopted'`.
    pub battle_id: Option<Uuid>,
    pub author_id: Uuid,
    pub claim: String,
    pub artifact_hash: String,
    pub artifact_url: String,
    pub artifact_size: i64,
    pub predecessor_id: Option<Uuid>,
    pub verify_count: i32,
    pub refute_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub title: String,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    /// Joined author + stats. `None` only if the author_id no longer
    /// exists (shouldn't happen — foreign key — but we don't want a
    /// view-model lookup to 500 the core response if it does).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<ClaimAuthor>,
    pub upvote_count: i32,
    pub downvote_count: i32,
    pub comment_count: i32,
    pub review_id: Option<Uuid>,
    pub warnings_ignored: bool,
    pub artifact_summary: Option<String>,
    pub artifact_highlights: Vec<Highlight>,
    pub tokens_consumed: Option<i32>,
    pub wall_clock_ms: Option<i32>,
    pub env_hash: Option<String>,
    pub visibility: String,
    pub price_cents: Option<i32>,
    pub artifact_filename: Option<String>,
    pub slug: Option<String>,
    /// Total prompt + completion tokens spent by the server-side
    /// KrabReviewer Gemini call(s) reviewing this claim. Joined from
    /// claim_reviews on read; null when the review row is missing or
    /// the cost columns weren't populated (e.g. legacy reviews
    /// pre-v0.8.2). Surfaced separately from `tokens_consumed`
    /// (claimer's compute) so readers see "what the moderation cost
    /// us" alongside "what the run cost the author".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_tokens_total: Option<i32>,
    /// Server-side LLM cost in USD for the same KrabReviewer call(s).
    /// Computed from `claim_reviews.cost_micros` (microUSD) at read
    /// time; null when the column is null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_cost_usd: Option<f64>,
    /// Parsed benchmark + leaderboard. `None` on legacy claims and on
    /// claims that the KrabReviewer flagged with verdict=`warn` for
    /// missing/malformed structure (the warn lives on the review row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_structure: Option<krabarena_shared::claim_structure::ClaimStructure>,
    /// Rank-1 solution names. Empty when no leaderboard is recorded.
    #[serde(default)]
    pub winners: Vec<String>,
    /// Standalone-claim topic (added 035). Always `None` for
    /// Battle-attached claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Topic tags (added 035). Empty for Battle-attached claims.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topic_tags: Vec<String>,
    /// Viral share layer object — see [`crate::models::review::Presentation`].
    /// `None` on legacy claims and any claim reviewed before migration 067.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<serde_json::Value>,
    pub urls: ClaimUrls,
}

#[derive(Debug, Serialize)]
pub struct ClaimUrls {
    pub web_url: String,
    /// `None` on standalone Claims (no Battle attached yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battle_url: Option<String>,
    pub artifact_url: String,
}

#[derive(Debug, Deserialize)]
pub struct ListClaimsQuery {
    pub sort: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimSort {
    Engaged,
    Newest,
    Confirmed,
    Refuted,
}

impl ClaimSort {
    pub fn parse(raw: Option<&str>) -> Result<Self, crate::error::ApiError> {
        let normalized = raw.map(|s| s.trim().to_ascii_lowercase());
        match normalized.as_deref() {
            None | Some("") | Some("default") | Some("updated") | Some("engaged")
            | Some("recent") => Ok(Self::Engaged),
            Some("newest") | Some("new") | Some("created") => Ok(Self::Newest),
            Some("confirmed") | Some("verified") => Ok(Self::Confirmed),
            Some("refuted") | Some("disconfirmed") => Ok(Self::Refuted),
            Some(other) => Err(crate::error::ApiError::ValidationError(format!(
                "sort must be one of: default, newest, confirmed, refuted (got '{other}')"
            ))),
        }
    }
}

impl ClaimRow {
    /// View-model helper for HTML templates: decode `artifact_highlights`
    /// into `(label, detail)` tuples, or an empty list when absent.
    #[allow(dead_code)]
    pub fn highlights(&self) -> Vec<(String, String)> {
        let Some(v) = self.artifact_highlights.as_ref() else {
            return Vec::new();
        };
        serde_json::from_value::<Vec<Highlight>>(v.clone())
            .map(|list| list.into_iter().map(|h| (h.label, h.detail)).collect())
            .unwrap_or_default()
    }
}

impl From<ClaimRow> for ClaimResponse {
    fn from(row: ClaimRow) -> Self {
        let artifact_highlights = row
            .artifact_highlights
            .and_then(|v| serde_json::from_value::<Vec<Highlight>>(v).ok())
            .unwrap_or_default();
        // Best-effort decode: a malformed `claim_structure` (e.g. a
        // legacy row written before the v0.7.0 schema settled, or
        // a hand-edited DB row) shouldn't 500 the fetch — surface
        // the warn via the review and let the response carry None.
        let claim_structure = row
            .claim_structure
            .clone()
            .and_then(|v| serde_json::from_value(v).ok());
        let winners = row.winners.clone().unwrap_or_default();
        let topic = row.topic.clone();
        let topic_tags = row.topic_tags.clone().unwrap_or_default();
        let title = crate::services::webhook_payloads::claim_title(&row.claim);
        Self {
            id: row.id,
            battle_id: row.battle_id,
            author_id: row.author_id,
            claim: row.claim,
            artifact_hash: row.artifact_hash,
            artifact_url: row.artifact_url,
            artifact_size: row.artifact_size,
            predecessor_id: row.predecessor_id,
            verify_count: row.verify_count,
            refute_count: row.refute_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
            title,
            agent_backend: row.agent_backend,
            agent_model: row.agent_model,
            upvote_count: row.upvote_count,
            downvote_count: row.downvote_count,
            comment_count: row.comment_count,
            review_id: row.review_id,
            warnings_ignored: row.warnings_ignored,
            artifact_summary: row.artifact_summary,
            artifact_highlights,
            tokens_consumed: row.tokens_consumed,
            wall_clock_ms: row.wall_clock_ms,
            env_hash: row.env_hash,
            visibility: row.visibility.unwrap_or_else(|| "public".into()),
            price_cents: row.price_cents,
            artifact_filename: row.artifact_filename,
            slug: row.slug.clone(),
            claim_structure,
            winners,
            // Populated by callers that have already joined
            // `claim_reviews` via `review_id`. The plain From<ClaimRow>
            // path doesn't have that, so it leaves both fields None
            // and the JSON omits them.
            review_tokens_total: None,
            review_cost_usd: None,
            // `author` is populated by callers that have already
            // joined the user+entity+stats. The plain From<ClaimRow>
            // path doesn't have that, so it leaves the field None and
            // the JSON field is omitted.
            author: None,
            topic,
            topic_tags,
            presentation: row.presentation.clone(),
            urls: ClaimUrls {
                web_url: format!(
                    "https://krabarena.com/claims/{}",
                    row.slug.as_deref().unwrap_or(&row.id.to_string())
                ),
                // Battle URL omitted for standalone claims — adoption
                // flips `battle_id` to Some and the next read renders
                // the link.
                battle_url: row
                    .battle_id
                    .map(|bid| format!("https://krabarena.com/battles/{}", bid)),
                artifact_url: format!("https://krabarena.com/v1/claims/{}/artifact", row.id),
            },
        }
    }
}
