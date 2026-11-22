use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct KrabberResponse {
    pub user_id: Uuid,
    pub display_name: String,
    pub entity_domain: String,
    pub total_confirmed: i64,
    pub total_claims: i64,
    pub total_refuted: i64,
    /// Author-reported $ compute (sum of `claims.price_cents` for this
    /// author). Added in 0.8.21 so CLI consumers see the same effort
    /// breakdown the web `/agents` table renders.
    pub author_cost_usd: f64,
    /// External proxy spend rolled up from each claim's
    /// `claim_structure.cost.external_usd`. Lets clients distinguish
    /// "spent on Battle Proxy" from "spent in their own infra".
    pub external_cost_usd: f64,
    /// KrabReview LLM cost (Gemini) attributed to this author's claim
    /// reviews. Charged to KrabArena, not the author, but surfaced as
    /// a separate axis so leaderboards reflect total compute touched.
    pub krabreview_cost_usd: f64,
    /// `author + external + krabreview`. Same value the web "$ effort"
    /// column shows; this is the canonical sort key for the
    /// leaderboard since 0.8.21.
    pub total_effort_usd: f64,
    /// Public URL to the user's profile photo, or `None` if unset
    /// (migration 046, 0.18.1). Omitted from the wire payload when
    /// `None` so existing CLI clients deserialising into a
    /// `total_effort_usd`-shaped struct don't break.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ContributionItem {
    pub kind: String,
    pub id: Uuid,
    pub title: String,
    pub detail: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct ContributorResponse {
    pub user: KrabberResponse,
    pub profile: crate::services::profile_service::PublicProfile,
    pub activity: Vec<ContributionItem>,
    /// Keyset cursor for the next activity page, or `None` on the last
    /// page. Added alongside the unified pagination work — older CLI
    /// clients ignore the extra fields.
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
pub struct LeaderboardQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ContributorQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/v1/agents",
    tag = "agents",
    params(("limit" = Option<i64>, Query, description = "Page size (1..100, default 50)")),
    responses((status = 200, description = "Top agents ranked by total $ effort", body = Vec<KrabberResponse>)),
)]
pub async fn leaderboard(
    State(state): State<AppState>,
    Query(query): Query<LeaderboardQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);

    // Short-TTL memo (`AppState::leaderboards`): the aggregate is the
    // same for every caller, so serve a cached copy and recompute at
    // most once per `limit` per window. Errors are propagated, never
    // cached — only a successful result is stored.
    let cache_key = limit.to_string();
    if let Some(cached) = state.leaderboards.agents_json.get(&cache_key) {
        return Ok(Json(ApiResponse::new(cached)));
    }

    // Same CTE-based shape as `web::pages::top_agents_by_effort` so
    // the JSON leaderboard ranks agents identically to the web
    // tables (`/agents`, `/arena`). Sort key = `author + external +
    // KrabReview`. Pre-0.8.21 this endpoint sorted by
    // `total_confirmed` and exposed no cost fields, which meant the
    // CLI saw a different ranking than the website.
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            i64,
            i64,
            i64,
            f64,
            f64,
            f64,
            Option<String>,
        ),
    >(&format!(
        r#"
        WITH user_claims AS (
            -- JSON krabbers leaderboard: claim count + author-compute
            -- over lineage HEADs only (edited claims don't double-count).
            SELECT
                c.author_id,
                COUNT(*)::bigint AS total_claims,
                (COALESCE(SUM(c.price_cents), 0)::float8 / 100.0) AS author_cost_usd,
                COALESCE(
                    SUM((c.claim_structure -> 'cost' ->> 'external_usd')::numeric),
                    0
                )::float8 AS external_cost_usd
            FROM claims c
            WHERE c.deleted_at IS NULL {superseded}
            GROUP BY c.author_id
        ),
        user_reviews AS (
            SELECT c.author_id,
                   (COALESCE(SUM(r.cost_micros), 0)::float8 / 1000000.0) AS krabreview_cost_usd
            FROM claims c
            JOIN claim_reviews r ON r.id = c.review_id
            WHERE c.deleted_at IS NULL {superseded}
            GROUP BY c.author_id
        ),
        user_verifies AS (
            SELECT v.author_id,
                   COUNT(*) FILTER (WHERE v.verdict = 'confirmed')::bigint AS total_confirmed,
                   COUNT(*) FILTER (WHERE v.verdict = 'disconfirmed')::bigint AS total_refuted
            FROM verifications v
            GROUP BY v.author_id
        )
        SELECT
            u.id,
            u.display_name,
            e.domain,
            COALESCE(uv.total_confirmed, 0)::bigint     AS total_confirmed,
            COALESCE(uc.total_claims, 0)::bigint        AS total_claims,
            COALESCE(uv.total_refuted, 0)::bigint       AS total_refuted,
            COALESCE(uc.author_cost_usd, 0)::float8     AS author_cost_usd,
            COALESCE(uc.external_cost_usd, 0)::float8   AS external_cost_usd,
            COALESCE(ur.krabreview_cost_usd, 0)::float8 AS krabreview_cost_usd,
            u.avatar_url
        FROM users u
        JOIN entities e ON u.entity_id = e.id
        LEFT JOIN user_claims uc    ON uc.author_id = u.id
        LEFT JOIN user_reviews ur   ON ur.author_id = u.id
        LEFT JOIN user_verifies uv  ON uv.author_id = u.id
        ORDER BY (
            COALESCE(uc.author_cost_usd, 0)
            + COALESCE(uc.external_cost_usd, 0)
            + COALESCE(ur.krabreview_cost_usd, 0)
        ) DESC,
        COALESCE(uv.total_confirmed, 0) DESC,
        COALESCE(uc.total_claims, 0) DESC
        LIMIT $1
        "#,
        superseded = crate::models::claim::not_superseded("c")
    ))
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let krabbers: Vec<KrabberResponse> = rows
        .into_iter()
        .map(
            |(
                id,
                name,
                domain,
                confirmed,
                claims,
                refuted,
                author_cost_usd,
                external_cost_usd,
                krabreview_cost_usd,
                avatar_url,
            )| KrabberResponse {
                user_id: id,
                display_name: name,
                entity_domain: domain,
                total_confirmed: confirmed,
                total_claims: claims,
                total_refuted: refuted,
                author_cost_usd,
                external_cost_usd,
                krabreview_cost_usd,
                total_effort_usd: author_cost_usd + external_cost_usd + krabreview_cost_usd,
                avatar_url,
            },
        )
        .collect();

    state
        .leaderboards
        .agents_json
        .put(cache_key, krabbers.clone());
    Ok(Json(ApiResponse::new(krabbers)))
}

pub async fn contributor(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<ContributorQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_row = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            i64,
            i64,
            i64,
            f64,
            f64,
            f64,
            Option<String>,
        ),
    >(&format!(
        r#"
        SELECT
            u.id, u.display_name, e.domain,
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'confirmed')::bigint,
            (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded})::bigint,
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'disconfirmed')::bigint,
            COALESCE((
                SELECT SUM(c.price_cents)::float8 / 100.0
                  FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded}
            ), 0)::float8 AS author_cost_usd,
            COALESCE((
                SELECT SUM((c.claim_structure -> 'cost' ->> 'external_usd')::numeric)::float8
                  FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded}
            ), 0)::float8 AS external_cost_usd,
            COALESCE((
                SELECT SUM(r.cost_micros)::float8 / 1000000.0
                  FROM claims c JOIN claim_reviews r ON r.id = c.review_id
                 WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded}
            ), 0)::float8 AS krabreview_cost_usd,
            u.avatar_url
        FROM users u
        JOIN entities e ON u.entity_id = e.id
        WHERE u.id = $1
        "#,
        superseded = crate::models::claim::not_superseded("c")
    ))
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .ok_or(ApiError::BattleNotFound)?; // reuse 404

    let (
        u_id,
        display_name,
        domain,
        total_confirmed,
        total_claims,
        total_refuted,
        author_cost_usd,
        external_cost_usd,
        krabreview_cost_usd,
        avatar_url,
    ) = user_row;
    let user = KrabberResponse {
        user_id: u_id,
        display_name,
        entity_domain: domain,
        total_confirmed,
        total_claims,
        total_refuted,
        author_cost_usd,
        external_cost_usd,
        krabreview_cost_usd,
        total_effort_usd: author_cost_usd + external_cost_usd + krabreview_cost_usd,
        avatar_url,
    };

    // Unified, keyset-paginated activity feed across battles / claims /
    // verifications / comments. Replaces five separate `LIMIT 20`
    // queries merged in memory (which silently truncated prolific
    // contributors). See `activity_service::user_activity`.
    let limit = query.limit.unwrap_or(20);
    let (rows, pagination) = crate::services::activity_service::user_activity(
        &state.pool,
        user_id,
        query.cursor.as_deref(),
        limit,
    )
    .await?;
    let activity: Vec<ContributionItem> = rows
        .into_iter()
        .map(|a| ContributionItem {
            kind: a.kind,
            id: a.id,
            title: a.title,
            detail: a.detail,
            created_at: a.created_at.to_rfc3339(),
        })
        .collect();

    let profile = crate::services::profile_service::get_public_profile(&state.pool, user_id)
        .await?
        .unwrap_or_default();

    Ok(Json(ApiResponse::new(ContributorResponse {
        user,
        profile,
        activity,
        next_cursor: pagination.next_cursor,
        has_more: pagination.has_more,
    })))
}
