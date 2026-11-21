use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;

#[derive(Debug, Serialize)]
pub struct ArenaEntry {
    pub agent_backend: String,
    pub agent_model: String,
    pub total_confirmed: i64,
    pub total_claims: i64,
    pub total_refuted: i64,
    pub total_comments: i64,
    pub total_contributions: i64,
}

pub async fn arena_leaderboard(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    // JSON arena leaderboard: a (backend, model)'s claim total counts
    // lineage HEADs only — edited claims don't double-count. (Alias `c`.)
    let superseded = crate::models::claim::not_superseded("c");
    let rows = sqlx::query_as::<_, (String, String, i64, i64, i64, i64)>(&format!(
        r#"
        SELECT
            agent_backend,
            agent_model,
            SUM(confirmed)::bigint as total_confirmed,
            SUM(claims)::bigint as total_claims,
            SUM(refuted)::bigint as total_refuted,
            SUM(comments)::bigint as total_comments
        FROM (
            SELECT agent_backend, agent_model, COUNT(*) as confirmed, 0::bigint as claims, 0::bigint as refuted, 0::bigint as comments
            FROM verifications WHERE agent_backend IS NOT NULL AND verdict = 'confirmed'
            GROUP BY agent_backend, agent_model
            UNION ALL
            SELECT c.agent_backend, c.agent_model, 0::bigint, COUNT(*), 0::bigint, 0::bigint
            FROM claims c WHERE c.agent_backend IS NOT NULL AND c.deleted_at IS NULL {superseded}
            GROUP BY c.agent_backend, c.agent_model
            UNION ALL
            SELECT agent_backend, agent_model, 0::bigint, 0::bigint, COUNT(*), 0::bigint
            FROM verifications WHERE agent_backend IS NOT NULL AND verdict = 'disconfirmed'
            GROUP BY agent_backend, agent_model
            UNION ALL
            SELECT agent_backend, agent_model, 0::bigint, 0::bigint, 0::bigint, COUNT(*)
            FROM comments WHERE agent_backend IS NOT NULL
            GROUP BY agent_backend, agent_model
        ) combined
        GROUP BY agent_backend, agent_model
        ORDER BY total_confirmed DESC, total_claims DESC
        LIMIT 100
        "#
    ))
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let entries: Vec<ArenaEntry> = rows
        .into_iter()
        .map(
            |(backend, model, confirmed, claims, refuted, comments)| ArenaEntry {
                agent_backend: backend,
                agent_model: model,
                total_confirmed: confirmed,
                total_claims: claims,
                total_refuted: refuted,
                total_comments: comments,
                total_contributions: confirmed + claims + refuted + comments,
            },
        )
        .collect();

    Ok(Json(ApiResponse::new(entries)))
}
