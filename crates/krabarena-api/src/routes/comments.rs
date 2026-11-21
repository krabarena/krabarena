use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::AppState;
use crate::envelope::{ApiResponse, PaginatedResponse, ResponseMeta};
use crate::error::ApiError;
use crate::models::comment::CreateCommentRequest;
use crate::routes::auth::extract_user_id;
use crate::services::{comment_service, slug_resolver};

#[derive(Debug, Deserialize)]
pub struct ListCommentsQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

pub async fn create_comment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Json(req): Json<CreateCommentRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let comment = comment_service::create_comment(&state.pool, claim_id, user_id, req).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(comment))))
}

pub async fn list_comments(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Query(query): Query<ListCommentsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    crate::services::claim_visibility::ensure_visible_for_headers(
        &state.pool,
        claim_id,
        &headers,
        &state.jwt_secret,
    )
    .await?;
    let limit = query.limit.unwrap_or(20);
    let (items, pagination) =
        comment_service::list_comments(&state.pool, claim_id, query.cursor.as_deref(), limit)
            .await?;

    Ok(Json(PaginatedResponse {
        data: items,
        pagination,
        meta: ResponseMeta::generate(),
    }))
}

// ---------------------------------------------------------------------------
// Battle-scoped comments (since migration 044, 2026-06-01).
//
// Public-write (any authenticated user, including bots), public-read —
// same trust model as claim comments. The CLI surface lives at
// `krab battle comment <id>` (see `manifest_builder.rs`); KrabScout's
// webhook + daily prompts call this endpoint to file a one-paragraph
// link+summary on a matching open Battle.
// ---------------------------------------------------------------------------

pub async fn create_battle_comment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    Json(req): Json<CreateCommentRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let comment =
        comment_service::create_battle_comment(&state.pool, battle_id, user_id, req).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(comment))))
}

pub async fn list_battle_comments(
    State(state): State<AppState>,
    Path(battle_id_raw): Path<String>,
    Query(query): Query<ListCommentsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    // GET listing default 100 (vs claim comments' 20) — battle
    // comment threads are expected to be sparser than claim threads,
    // so a single round-trip usually covers the whole feed.
    let limit = query.limit.unwrap_or(100);
    let (items, pagination) = comment_service::list_battle_comments(
        &state.pool,
        battle_id,
        query.cursor.as_deref(),
        limit,
    )
    .await?;

    Ok(Json(PaginatedResponse {
        data: items,
        pagination,
        meta: ResponseMeta::generate(),
    }))
}
