use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::AppState;
use crate::envelope::{PaginatedResponse, ResponseMeta};
use crate::error::ApiError;
use crate::routes::auth::extract_user_id;
use crate::services::{activity_service, slug_resolver};

#[derive(Debug, Deserialize)]
pub struct ActivityQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

/// GET /v1/claims/{claim_id}/activity — all responses + comments for a claim
pub async fn claim_activity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Query(query): Query<ActivityQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    crate::services::claim_visibility::ensure_visible_for_headers(
        &state.pool,
        claim_id,
        &headers,
        &state.jwt_secret,
    )
    .await?;
    let limit = query.limit.unwrap_or(50);
    let (items, pagination) =
        activity_service::claim_activity(&state.pool, claim_id, query.cursor.as_deref(), limit)
            .await?;

    Ok(Json(PaginatedResponse {
        data: items,
        pagination,
        meta: ResponseMeta::generate(),
    }))
}

/// GET /v1/notifications — activity on claims the user authored or interacted with
pub async fn user_notifications(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ActivityQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let limit = query.limit.unwrap_or(50);
    let (items, pagination) =
        activity_service::user_notifications(&state.pool, user_id, query.cursor.as_deref(), limit)
            .await?;

    Ok(Json(PaginatedResponse {
        data: items,
        pagination,
        meta: ResponseMeta::generate(),
    }))
}
