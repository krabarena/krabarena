use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::services::battle_candidates_service as svc;
use crate::services::voter_service;

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    pub title: String,
    #[serde(default)]
    pub rationale: String,
}

/// Read the voter-session token (the same one /v1/claims/.../votes uses)
/// from `x-voter-token`, resolve it through `voter_sessions`, and return
/// the matching voter's `id`. Returns `Unauthorized` if header missing or
/// session expired/invalid.
async fn require_voter(state: &AppState, headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let token = headers
        .get("x-voter-token")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let voter = voter_service::get_voter_by_session(&state.pool, token)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(voter.id)
}

/// Best-effort viewer resolution: returns `Some(voter_id)` when a valid
/// `x-voter-token` is present, otherwise `None`. Used by `list` to
/// populate `viewer_voted` without requiring auth.
async fn optional_voter(state: &AppState, headers: &HeaderMap) -> Option<Uuid> {
    let token = headers.get("x-voter-token").and_then(|v| v.to_str().ok())?;
    voter_service::get_voter_by_session(&state.pool, token)
        .await
        .ok()
        .flatten()
        .map(|v| v.id)
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let viewer = optional_voter(&state, &headers).await;
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = svc::list_candidates(&state.pool, viewer, limit, offset, None).await?;
    Ok(Json(ApiResponse::new(items)))
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let voter_id = require_voter(&state, &headers).await?;
    let id = svc::create_candidate(&state.pool, voter_id, &req.title, &req.rationale).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(id))))
}

pub async fn vote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let voter_id = require_voter(&state, &headers).await?;
    svc::vote(&state.pool, id, voter_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn unvote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let voter_id = require_voter(&state, &headers).await?;
    svc::unvote(&state.pool, id, voter_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/battle-candidates/{id}` — editor / admin only.
///
/// Soft-deletes (sets `status='dismissed'`) so the candidate drops
/// off the public listing. Used as the "promotion path" exit: once
/// an editor curates a candidate into a real Battle, dismissing the
/// candidate clears it from the queue. Auth is via CLI JWT
/// (`x-krabarena-token`) so editors can run this from `krab`
/// without going through the voter session flow.
pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let _editor_id = crate::routes::auth::require_editor(&headers, &state.jwt_secret)?;
    svc::dismiss_candidate(&state.pool, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
