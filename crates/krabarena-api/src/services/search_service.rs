//! Hybrid search across claims, battles, and users.
//!
//! Two retrieval signals feed a single ranking:
//!
//!   · **Semantic** — cosine similarity between the query embedding
//!     (Gemini `gemini-embedding-001`, 768-dim) and each row's
//!     `search_embedding`, via pgvector's HNSW index. Only used when
//!     the caller passes an embedding (AI mode).
//!   · **Lexical** — pg_trgm similarity + `ILIKE` substring match on
//!     the human-readable fields (battle title, claim topic/body, user
//!     display name). Always runs. This is what makes exact lookups
//!     (agent names, slugs, version strings) work — semantics alone
//!     smears them into a fuzzy neighbourhood.
//!
//! When the caller passes `query_embedding = None` (AI toggle off, or
//! graceful degrade after an embedding failure) only the lexical
//! signal contributes — cosine is pinned to 0 for every candidate.
//!
//! Both per-kind candidate pools are merged by id, then mixed with a
//! freshness signal (decays with row age) and a hotness signal
//! (verification activity + claim count, log-scaled). The side signals
//! are min-max normalised within each kind's candidate window so a
//! quiet corner of the platform isn't dwarfed by one hot Battle.
//!
//! Final score formula (constants live in [`Weights`] so the test
//! suite can twiddle them without grepping the codebase):
//!
//! ```text
//! score = w_cosine    * cosine
//!       + w_lexical   * lexical
//!       + w_hotness   * normalised_hotness
//!       + w_freshness * normalised_freshness
//! ```

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::services::embedding_service::EMBEDDING_DIM;

/// The kinds of entity the search index covers. `All` is a
/// convenience for the top-level merged ranking — individual kinds
/// can still be requested for kind-scoped results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    Battles,
    Claims,
    Users,
    All,
}

impl SearchKind {
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("all").trim().to_ascii_lowercase().as_str() {
            "battle" | "battles" => Self::Battles,
            "claim" | "claims" => Self::Claims,
            "user" | "users" | "agent" | "agents" => Self::Users,
            _ => Self::All,
        }
    }

    /// Canonical lowercase label used in URLs, JSON, and templates.
    /// Single source of truth so the JSON route and the HTML page
    /// don't each re-spell the match.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Battles => "battles",
            Self::Claims => "claims",
            Self::Users => "users",
            Self::All => "all",
        }
    }

    fn wants(self, other: SearchKind) -> bool {
        self == SearchKind::All || self == other
    }
}

/// Tunable score weights — kept as a struct (not constants) so tests
/// can override them per-call without locking. The default matches the
/// spec; if we ever want to expose them in an admin surface this is the
/// place to plumb through.
#[derive(Debug, Clone, Copy)]
pub struct Weights {
    pub cosine: f32,
    pub lexical: f32,
    pub hotness: f32,
    pub freshness: f32,
}

impl Default for Weights {
    fn default() -> Self {
        // Cosine-dominant for semantic relevance, with a substantial
        // lexical term so that an *exact* name/slug/version match
        // reliably floats to the top (the old pure-cosine ranking
        // smeared those into oblivion). Hotness/freshness stay a light
        // tiebreaker for near-identical rows. Weights sum to 1.0 so a
        // perfect-everything row scores 1.0.
        //
        // History: a GEPA weight sweep on the 20-query grounded eval
        // measured pure cosine (1.0/0.0) at MRR 0.900 vs the old
        // 0.6/0.4 popularity-heavy split at 0.802. We keep cosine
        // dominant and add lexical on top rather than trading it away.
        Self {
            cosine: 0.72,
            lexical: 0.20,
            hotness: 0.02,
            freshness: 0.06,
        }
    }
}

/// One result row returned to JSON / template consumers.
///
/// `kind` doubles as the discriminator so a `kind=all` query can be
/// rendered without checking `Option<UUID>` fields. `slug` is `None`
/// for users (no slug table) and for legacy battles/claims that
/// pre-date slug introduction.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub kind: &'static str,
    pub id: Uuid,
    pub slug: Option<String>,
    pub title: String,
    pub snippet: String,
    pub score: f32,
    pub score_breakdown: ScoreBreakdown,
    /// Public-facing URL. Pre-rendered from the caller's base URL so
    /// consumers (CLI, web template) don't each reinvent slug-vs-uuid
    /// routing.
    pub url: String,
    /// Age in days at query time. Surfaced so a UI can render "3d ago"
    /// without a second timestamp parse.
    pub age_days: f32,
    /// `true` when the row was surfaced by the semantic (vector) signal
    /// alone — a cosine match with no literal/lexical overlap. The UI
    /// uses this to label "why this matched": lexical hits get their
    /// terms highlighted, semantic-only hits get a "semantic" tag since
    /// there's no literal text to highlight.
    pub semantic: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScoreBreakdown {
    pub cosine: f32,
    pub lexical: f32,
    pub hotness: f32,
    pub freshness: f32,
}

/// Top-N count we pull from each kind (per signal) before mixing +
/// truncating. Slightly oversampled vs the final `limit` so the
/// freshness/hotness normalisation has enough range to be meaningful
/// and the post-ANN `deleted_at`/superseded filters don't starve the
/// result set.
const PER_KIND_CANDIDATES: i64 = 40;

/// Hard ceiling on `limit` to keep response sizes bounded. Also the
/// ceiling for the "show more" incremental loader on the HTML page —
/// hybrid ANN + re-rank can't meaningfully paginate past its candidate
/// window, so we cap rather than fake deep pagination.
pub const HARD_LIMIT: i64 = 100;

/// Default result count when the caller doesn't specify one. Shared by
/// the JSON route and the HTML page so the two surfaces agree.
pub const DEFAULT_LIMIT: i64 = 20;

/// Hotness mix knob: refutes weighed more than verifies because a
/// successful refutation is a stronger engagement signal (someone
/// actually ran the artifact and disagreed) than a routine verify.
const REFUTE_WEIGHT: f64 = 5.0;

/// Minimum pg_trgm similarity for a row to enter the lexical candidate
/// pool. Low + permissive — `ILIKE` substring matches are admitted
/// regardless, this only governs the fuzzy (typo-tolerant) tail.
const TRGM_THRESHOLD: f64 = 0.1;

/// A merged candidate before final ranking. Carries the raw signals
/// from both retrieval paths; `cosine`/`lexical` are 0 when that path
/// didn't surface the row.
struct Candidate {
    kind: &'static str,
    /// URL path segment (`battles` / `claims` / `agents`).
    url_path: &'static str,
    id: Uuid,
    slug: Option<String>,
    title: String,
    snippet_src: String,
    created_at: DateTime<Utc>,
    cosine: f32,
    lexical: f32,
    hotness_raw: f64,
}

/// Run the hybrid search and return up to `limit` results across the
/// requested kinds.
///
/// `query_embedding` is `Some` in AI mode (semantic + lexical) and
/// `None` in lexical-only mode (AI toggle off, or graceful degrade
/// after an embedding failure). `q` is the raw user text, used for the
/// lexical (pg_trgm) candidate queries.
pub async fn search(
    pool: &PgPool,
    base_url: &str,
    q: &str,
    query_embedding: Option<&[f32]>,
    kind: SearchKind,
    limit: i64,
) -> Result<Vec<SearchHit>, ApiError> {
    if let Some(emb) = query_embedding {
        if emb.len() != EMBEDDING_DIM {
            // Shouldn't happen — the embedding service pins the dim —
            // but treat this as a 400 rather than a 500 so tests that
            // pass bogus vectors get a clear signal.
            return Err(ApiError::ValidationError(format!(
                "query embedding has wrong dimensionality: expected {EMBEDDING_DIM}, got {}",
                emb.len()
            )));
        }
    }
    let limit = limit.clamp(1, HARD_LIMIT);
    let vec = query_embedding.map(|e| pgvector::Vector::from(e.to_vec()));
    let vref = vec.as_ref();
    let now = Utc::now();
    let weights = Weights::default();

    // Gather candidate pools for the requested kinds concurrently.
    // Each future resolves to an empty Vec when its kind isn't wanted,
    // so `try_join!` stays a single straight-line await.
    let (battles, claims, users) = tokio::try_join!(
        async {
            if kind.wants(SearchKind::Battles) {
                battle_candidates(pool, vref, q).await
            } else {
                Ok(Vec::new())
            }
        },
        async {
            if kind.wants(SearchKind::Claims) {
                claim_candidates(pool, vref, q).await
            } else {
                Ok(Vec::new())
            }
        },
        async {
            if kind.wants(SearchKind::Users) {
                user_candidates(pool, vref, q).await
            } else {
                Ok(Vec::new())
            }
        },
    )?;

    // Rank per kind (hotness/freshness normalisation is kind-local —
    // a battle's hotness scale differs from a user's), then merge.
    let mut hits: Vec<SearchHit> = Vec::new();
    hits.extend(rank_candidates(battles, base_url, now, &weights));
    hits.extend(rank_candidates(claims, base_url, now, &weights));
    hits.extend(rank_candidates(users, base_url, now, &weights));

    // Stable order by score desc, tiebreak by recency (newer first)
    // so flicker between equal-scoring rows is at least predictable.
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.age_days
                    .partial_cmp(&b.age_days)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    hits.truncate(limit as usize);
    Ok(hits)
}

// ---------------------------------------------------------------------------
// Per-kind candidate gathering. Each fn runs the vector query (when an
// embedding is supplied) and the trigram query, then merges the two
// pools by id into `Candidate`s.
// ---------------------------------------------------------------------------

/// (id, title, description, slug, created_at, similarity, verify_total,
/// refute_total, claim_count).
type BattleVecRow = (
    Uuid,
    String,
    String,
    Option<String>,
    DateTime<Utc>,
    f64,
    i64,
    i64,
    i64,
);

/// Battle trigram row — same shape plus an `exact` flag (ILIKE hit).
type BattleTrgmRow = (
    Uuid,
    String,
    String,
    Option<String>,
    DateTime<Utc>,
    f64,
    bool,
    i64,
    i64,
    i64,
);

async fn battle_candidates(
    pool: &PgPool,
    query: Option<&pgvector::Vector>,
    q: &str,
) -> Result<Vec<Candidate>, ApiError> {
    // Hotness aggregates verify + refute counts and the claim count
    // across every live claim attached to the battle.
    let hotness = |verify: i64, refute: i64, claims: i64| {
        ((verify as f64) + REFUTE_WEIGHT * (refute as f64) + (claims as f64) + 1.0).log10()
    };
    let mut map: HashMap<Uuid, Candidate> = HashMap::new();

    if let Some(v) = query {
        let rows: Vec<BattleVecRow> = sqlx::query_as(
            "SELECT b.id, b.title, b.description, b.slug, b.created_at, \
                    1 - (b.search_embedding <=> $1) AS similarity, \
                    COALESCE(SUM(c.verify_count), 0)::bigint AS verify_total, \
                    COALESCE(SUM(c.refute_count), 0)::bigint AS refute_total, \
                    COUNT(c.id)::bigint AS claim_count \
             FROM battles b \
             LEFT JOIN claims c ON c.battle_id = b.id AND c.deleted_at IS NULL \
             WHERE b.search_embedding IS NOT NULL AND b.deleted_at IS NULL \
             GROUP BY b.id \
             ORDER BY b.search_embedding <=> $1 ASC \
             LIMIT $2",
        )
        .bind(v)
        .bind(PER_KIND_CANDIDATES)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
        for (id, title, description, slug, created_at, sim, vt, rt, cc) in rows {
            map.insert(
                id,
                Candidate {
                    kind: "battle",
                    url_path: "battles",
                    id,
                    slug,
                    title,
                    snippet_src: description,
                    created_at,
                    cosine: (sim.max(0.0)) as f32,
                    lexical: 0.0,
                    hotness_raw: hotness(vt, rt, cc),
                },
            );
        }
    }

    let rows: Vec<BattleTrgmRow> = sqlx::query_as(
        "SELECT b.id, b.title, b.description, b.slug, b.created_at, \
                similarity(b.title, $1)::float8 AS sim, \
                (b.title ILIKE '%' || $1 || '%') AS exact, \
                COALESCE(SUM(c.verify_count), 0)::bigint AS verify_total, \
                COALESCE(SUM(c.refute_count), 0)::bigint AS refute_total, \
                COUNT(c.id)::bigint AS claim_count \
         FROM battles b \
         LEFT JOIN claims c ON c.battle_id = b.id AND c.deleted_at IS NULL \
         WHERE b.deleted_at IS NULL \
           AND (b.title ILIKE '%' || $1 || '%' OR similarity(b.title, $1) > $3) \
         GROUP BY b.id \
         ORDER BY similarity(b.title, $1) DESC \
         LIMIT $2",
    )
    .bind(q)
    .bind(PER_KIND_CANDIDATES)
    .bind(TRGM_THRESHOLD)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    for (id, title, description, slug, created_at, sim, exact, vt, rt, cc) in rows {
        let lex = lexical_score(sim, exact);
        map.entry(id)
            .and_modify(|c| {
                if lex > c.lexical {
                    c.lexical = lex;
                }
            })
            .or_insert_with(|| Candidate {
                kind: "battle",
                url_path: "battles",
                id,
                slug,
                title,
                snippet_src: description,
                created_at,
                cosine: 0.0,
                lexical: lex,
                hotness_raw: hotness(vt, rt, cc),
            });
    }

    Ok(map.into_values().collect())
}

/// (id, claim_body, topic, slug, created_at, similarity, verify, refute).
type ClaimVecRow = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    DateTime<Utc>,
    f64,
    i32,
    i32,
);

/// Claim trigram row — same shape plus an `exact` flag.
type ClaimTrgmRow = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    DateTime<Utc>,
    f64,
    bool,
    i32,
    i32,
);

async fn claim_candidates(
    pool: &PgPool,
    query: Option<&pgvector::Vector>,
    q: &str,
) -> Result<Vec<Candidate>, ApiError> {
    // Superseded versions drop out of search — only the lineage HEAD is
    // a candidate. See `models::claim::not_superseded`.
    // Private claims never surface in search (see `claim_visibility`).
    let superseded = format!(
        "{} {}",
        crate::models::claim::not_superseded("c"),
        crate::services::claim_visibility::public_only_sql("c")
    );
    let hotness = |verify: i32, refute: i32| {
        ((verify as f64) + REFUTE_WEIGHT * (refute as f64) + 1.0).log10()
    };
    let mut map: HashMap<Uuid, Candidate> = HashMap::new();

    if let Some(v) = query {
        let rows: Vec<ClaimVecRow> = sqlx::query_as(&format!(
            "SELECT c.id, c.claim, c.topic, c.slug, c.created_at, \
                    1 - (c.search_embedding <=> $1) AS similarity, \
                    c.verify_count, c.refute_count \
             FROM claims c \
             WHERE c.search_embedding IS NOT NULL AND c.deleted_at IS NULL \
               {superseded} \
             ORDER BY c.search_embedding <=> $1 ASC \
             LIMIT $2"
        ))
        .bind(v)
        .bind(PER_KIND_CANDIDATES)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
        for (id, body, topic, slug, created_at, sim, vt, rt) in rows {
            let (title, snippet_src) = extract_title_and_summary(&body, topic.as_deref());
            map.insert(
                id,
                Candidate {
                    kind: "claim",
                    url_path: "claims",
                    id,
                    slug,
                    title,
                    snippet_src,
                    created_at,
                    cosine: (sim.max(0.0)) as f32,
                    lexical: 0.0,
                    hotness_raw: hotness(vt, rt),
                },
            );
        }
    }

    let rows: Vec<ClaimTrgmRow> = sqlx::query_as(&format!(
        "SELECT c.id, c.claim, c.topic, c.slug, c.created_at, \
                GREATEST(similarity(COALESCE(c.topic, ''), $1), similarity(LEFT(c.claim, 160), $1))::float8 AS sim, \
                (c.topic ILIKE '%' || $1 || '%' OR c.claim ILIKE '%' || $1 || '%') AS exact, \
                c.verify_count, c.refute_count \
         FROM claims c \
         WHERE c.deleted_at IS NULL \
           {superseded} \
           AND (c.topic ILIKE '%' || $1 || '%' OR c.claim ILIKE '%' || $1 || '%' \
                OR similarity(COALESCE(c.topic, ''), $1) > $3) \
         ORDER BY sim DESC \
         LIMIT $2"
    ))
    .bind(q)
    .bind(PER_KIND_CANDIDATES)
    .bind(TRGM_THRESHOLD)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    for (id, body, topic, slug, created_at, sim, exact, vt, rt) in rows {
        let lex = lexical_score(sim, exact);
        map.entry(id)
            .and_modify(|c| {
                if lex > c.lexical {
                    c.lexical = lex;
                }
            })
            .or_insert_with(|| {
                let (title, snippet_src) = extract_title_and_summary(&body, topic.as_deref());
                Candidate {
                    kind: "claim",
                    url_path: "claims",
                    id,
                    slug,
                    title,
                    snippet_src,
                    created_at,
                    cosine: 0.0,
                    lexical: lex,
                    hotness_raw: hotness(vt, rt),
                }
            });
    }

    Ok(map.into_values().collect())
}

/// (id, display_name, email, created_at, similarity, claims_count).
type UserVecRow = (Uuid, String, Option<String>, DateTime<Utc>, f64, i64);

/// User trigram row — same shape plus an `exact` flag.
type UserTrgmRow = (Uuid, String, Option<String>, DateTime<Utc>, f64, bool, i64);

async fn user_candidates(
    pool: &PgPool,
    query: Option<&pgvector::Vector>,
    q: &str,
) -> Result<Vec<Candidate>, ApiError> {
    // Hotness is the number of (live, HEAD) claims this user authored.
    let superseded = crate::models::claim::not_superseded("c");
    let hotness = |claims: i64| ((claims as f64) + 1.0).log10();
    let mut map: HashMap<Uuid, Candidate> = HashMap::new();

    if let Some(v) = query {
        let rows: Vec<UserVecRow> = sqlx::query_as(&format!(
            "SELECT u.id, u.display_name, u.email, u.created_at, \
                    1 - (u.search_embedding <=> $1) AS similarity, \
                    (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded})::bigint AS claims_count \
             FROM users u \
             WHERE u.search_embedding IS NOT NULL \
             ORDER BY u.search_embedding <=> $1 ASC \
             LIMIT $2"
        ))
        .bind(v)
        .bind(PER_KIND_CANDIDATES)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
        for (id, display_name, email, created_at, sim, cc) in rows {
            map.insert(
                id,
                Candidate {
                    kind: "user",
                    url_path: "agents",
                    id,
                    slug: None,
                    title: display_name,
                    snippet_src: email.unwrap_or_else(|| "—".into()),
                    created_at,
                    cosine: (sim.max(0.0)) as f32,
                    lexical: 0.0,
                    hotness_raw: hotness(cc),
                },
            );
        }
    }

    let rows: Vec<UserTrgmRow> = sqlx::query_as(&format!(
        "SELECT u.id, u.display_name, u.email, u.created_at, \
                similarity(u.display_name, $1)::float8 AS sim, \
                (u.display_name ILIKE '%' || $1 || '%') AS exact, \
                (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded})::bigint AS claims_count \
         FROM users u \
         WHERE u.display_name ILIKE '%' || $1 || '%' OR similarity(u.display_name, $1) > $3 \
         ORDER BY sim DESC \
         LIMIT $2"
    ))
    .bind(q)
    .bind(PER_KIND_CANDIDATES)
    .bind(TRGM_THRESHOLD)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    for (id, display_name, email, created_at, sim, exact, cc) in rows {
        let lex = lexical_score(sim, exact);
        map.entry(id)
            .and_modify(|c| {
                if lex > c.lexical {
                    c.lexical = lex;
                }
            })
            .or_insert_with(|| Candidate {
                kind: "user",
                url_path: "agents",
                id,
                slug: None,
                title: display_name,
                snippet_src: email.unwrap_or_else(|| "—".into()),
                created_at,
                cosine: 0.0,
                lexical: lex,
                hotness_raw: hotness(cc),
            });
    }

    Ok(map.into_values().collect())
}

// ---------------------------------------------------------------------------
// Ranking + helpers
// ---------------------------------------------------------------------------

/// Normalise the side signals within this kind's candidate set, mix in
/// the weights, and build the public [`SearchHit`]s. Shared by all
/// three kinds so the normalise → zip → mix → build boilerplate lives
/// in exactly one place.
fn rank_candidates(
    cands: Vec<Candidate>,
    base_url: &str,
    now: DateTime<Utc>,
    weights: &Weights,
) -> Vec<SearchHit> {
    let hotness_raw: Vec<f64> = cands.iter().map(|c| c.hotness_raw).collect();
    let freshness_raw: Vec<f64> = cands.iter().map(|c| freshness(c.created_at, now)).collect();
    let hot_n = normalise(&hotness_raw);
    let fresh_n = normalise(&freshness_raw);

    cands
        .into_iter()
        .zip(hot_n.into_iter().zip(fresh_n))
        .map(|(c, (h, f))| {
            let h = h as f32;
            let f = f as f32;
            let score = mix(c.cosine, c.lexical, h, f, weights);
            let url = build_url(base_url, c.url_path, c.slug.as_deref(), c.id);
            let semantic = c.cosine > 0.0 && c.lexical <= 0.0;
            SearchHit {
                kind: c.kind,
                id: c.id,
                slug: c.slug,
                title: c.title,
                snippet: snippet(&c.snippet_src, 240),
                score,
                score_breakdown: ScoreBreakdown {
                    cosine: c.cosine,
                    lexical: c.lexical,
                    hotness: h,
                    freshness: f,
                },
                url,
                age_days: age_days(c.created_at, now) as f32,
                semantic,
            }
        })
        .collect()
}

/// Build a public URL. Prefers the slug; falls back to the UUID when
/// the row pre-dates slugs or has none (e.g. users).
fn build_url(base_url: &str, path: &str, slug: Option<&str>, id: Uuid) -> String {
    let base = base_url.trim_end_matches('/');
    match slug {
        Some(s) if !s.is_empty() => format!("{base}/{path}/{s}"),
        _ => format!("{base}/{path}/{id}"),
    }
}

/// Map a raw pg_trgm similarity + exact-match flag into a `[0, 1]`
/// lexical score. An exact (ILIKE substring) hit is floored high so a
/// literal name/slug match beats a merely-fuzzy one even when the raw
/// trigram similarity of a short query against long text is low.
fn lexical_score(sim: f64, exact: bool) -> f32 {
    let s = sim.clamp(0.0, 1.0) as f32;
    if exact { s.max(0.85) } else { s }
}

fn mix(cosine: f32, lexical: f32, hotness: f32, freshness: f32, w: &Weights) -> f32 {
    w.cosine * cosine + w.lexical * lexical + w.hotness * hotness + w.freshness * freshness
}

fn age_days(created_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    let dur = now.signed_duration_since(created_at);
    (dur.num_seconds() as f64 / 86_400.0).max(0.0)
}

fn freshness(created_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    // `exp(-age/30)` — half-life ~21 days. Matches the spec.
    (-age_days(created_at, now) / 30.0).exp()
}

/// Min-max normalise into `[0, 1]`. If every value is identical (e.g.
/// all freshness=1 because everything was just posted) we collapse the
/// column to 0.5 so it contributes neutrally rather than dominating.
fn normalise(raw: &[f64]) -> Vec<f64> {
    if raw.is_empty() {
        return Vec::new();
    }
    let min = raw.iter().copied().fold(f64::INFINITY, f64::min);
    let max = raw.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if (max - min).abs() < f64::EPSILON {
        return vec![0.5; raw.len()];
    }
    raw.iter().map(|v| (v - min) / (max - min)).collect()
}

/// Trim/clamp helper for snippet rendering. Strips markdown noise
/// (heading hashes, emphasis, code fences, link syntax) so the snippet
/// reads as prose rather than raw source, then caps to `max_chars`.
/// Char-aware so we don't slice mid-codepoint.
fn snippet(src: &str, max_chars: usize) -> String {
    let cleaned = strip_markdown(src);
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Lightweight markdown → plain-text reducer for snippets. Not a full
/// parser — just enough to stop `## Summary`, `**bold**`, `` `code` ``,
/// `> quote`, and `[text](url)` from leaking into the result preview.
/// Collapses runs of whitespace (incl. newlines) into single spaces.
fn strip_markdown(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let mut s = line.trim_start();
        // Drop leading block markers: heading #, blockquote >, list
        // bullets, and leading emphasis/space.
        s = s.trim_start_matches(['#', '>', ' ']);
        if let Some(rest) = s.strip_prefix("- ") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix("* ") {
            s = rest;
        }
        for ch in s.chars() {
            // Inline emphasis / code markers — drop the marker, keep the
            // text. Link syntax (`[text](url)`) is left as-is: it's rare
            // in the lead of a claim body and not worth a real parser.
            if matches!(ch, '*' | '_' | '`' | '#') {
                continue;
            }
            out.push(ch);
        }
        out.push(' ');
    }
    // Collapse whitespace runs.
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_ws = false;
    for ch in out.chars() {
        let ws = ch.is_whitespace();
        if ws {
            if !prev_ws {
                collapsed.push(' ');
            }
        } else {
            collapsed.push(ch);
        }
        prev_ws = ws;
    }
    collapsed.trim().to_string()
}

/// Pull the first `# Title` heading from a claim's markdown body and
/// return `(title, body_without_title)`. Falls back to `topic` for
/// standalone claims without a title heading, and finally to the raw
/// body's first 80 chars if neither is available.
fn extract_title_and_summary<'a>(body: &'a str, topic: Option<&'a str>) -> (String, String) {
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim().to_string();
            return (title, body.to_string());
        }
    }
    if let Some(t) = topic {
        return (t.trim().to_string(), body.to_string());
    }
    let fallback: String = body.chars().take(80).collect();
    (fallback, body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn normalise_handles_empty_and_uniform_inputs() {
        assert!(normalise(&[]).is_empty());
        assert_eq!(normalise(&[2.0, 2.0, 2.0]), vec![0.5, 0.5, 0.5]);
    }

    #[test]
    fn normalise_scales_to_unit_interval() {
        let out = normalise(&[1.0, 5.0, 9.0]);
        assert_eq!(out, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn freshness_decays_with_age() {
        let now = Utc::now();
        let recent = freshness(now, now);
        let week_old = freshness(now - Duration::days(7), now);
        let month_old = freshness(now - Duration::days(30), now);
        assert!(recent > week_old);
        assert!(week_old > month_old);
        assert!((recent - 1.0).abs() < 1e-6);
    }

    #[test]
    fn mix_respects_weights() {
        let w = Weights::default();
        let s = mix(1.0, 1.0, 1.0, 1.0, &w);
        assert!(
            (s - 1.0).abs() < 1e-6,
            "all-ones must sum the weights to 1.0"
        );
        let s_cos_only = mix(1.0, 0.0, 0.0, 0.0, &w);
        assert!(
            (s_cos_only - w.cosine).abs() < 1e-6,
            "cosine-only score must equal the cosine weight"
        );
        // Cosine must dominate — a perfect-cosine/zero-everything-else
        // row has to outrank a zero-cosine/max-everything-else row.
        assert!(mix(1.0, 0.0, 0.0, 0.0, &w) > mix(0.0, 1.0, 1.0, 1.0, &w));
    }

    #[test]
    fn lexical_score_floors_exact_matches() {
        // A fuzzy match keeps its raw similarity.
        assert!((lexical_score(0.3, false) - 0.3).abs() < 1e-6);
        // An exact substring match is floored high even when the raw
        // trigram similarity is tiny (short query in long text).
        assert!(lexical_score(0.05, true) >= 0.85);
        // ...but a strong exact match keeps its higher similarity.
        assert!((lexical_score(0.95, true) - 0.95).abs() < 1e-6);
    }

    #[test]
    fn build_url_prefers_slug_and_trims_base() {
        let id = Uuid::nil();
        assert_eq!(
            build_url("https://krabarena.com/", "battles", Some("my-slug"), id),
            "https://krabarena.com/battles/my-slug"
        );
        assert_eq!(
            build_url("https://krabarena.com", "agents", None, id),
            format!("https://krabarena.com/agents/{id}")
        );
        // Empty slug falls back to the UUID.
        assert_eq!(
            build_url("http://localhost:3001", "claims", Some(""), id),
            format!("http://localhost:3001/claims/{id}")
        );
    }

    #[test]
    fn snippet_caps_long_input_with_ellipsis() {
        let s = "x".repeat(500);
        let out = snippet(&s, 240);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 241);
    }

    #[test]
    fn strip_markdown_flattens_to_prose() {
        let md = "# Title\n\n## Summary\n\nThis is **bold** and `code` and a [link](http://x).";
        let out = strip_markdown(md);
        assert!(!out.contains('#'));
        assert!(!out.contains('*'));
        assert!(!out.contains('`'));
        assert!(out.contains("Title"));
        assert!(out.contains("This is bold and code and a"));
        // No double spaces from collapsed blank lines.
        assert!(!out.contains("  "));
    }

    #[test]
    fn extract_title_grabs_first_h1() {
        let body = "# Hello World\n## Summary\nbody text";
        let (title, _rest) = extract_title_and_summary(body, None);
        assert_eq!(title, "Hello World");
    }

    #[test]
    fn extract_title_falls_back_to_topic() {
        let body = "no heading here";
        let (title, _rest) = extract_title_and_summary(body, Some("My Topic"));
        assert_eq!(title, "My Topic");
    }

    #[test]
    fn search_kind_parses_aliases() {
        assert_eq!(SearchKind::parse(None), SearchKind::All);
        assert_eq!(SearchKind::parse(Some("battle")), SearchKind::Battles);
        assert_eq!(SearchKind::parse(Some("BATTLES")), SearchKind::Battles);
        assert_eq!(SearchKind::parse(Some("claim")), SearchKind::Claims);
        assert_eq!(SearchKind::parse(Some("agents")), SearchKind::Users);
        assert_eq!(SearchKind::parse(Some("nope")), SearchKind::All);
    }

    #[test]
    fn search_kind_as_str_roundtrips() {
        for k in [
            SearchKind::Battles,
            SearchKind::Claims,
            SearchKind::Users,
            SearchKind::All,
        ] {
            assert_eq!(SearchKind::parse(Some(k.as_str())), k);
        }
    }
}
