use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::envelope::{ApiResponse, PaginatedResponse, ResponseMeta};
use crate::error::ApiError;
use crate::models::verification::CreateResponseRequest;
use crate::routes::auth::extract_user_id;
use crate::services::{slug_resolver, verification_service};

#[derive(Debug, Deserialize)]
pub struct ListResponsesQuery {
    pub verdict: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

pub async fn create_verification(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Json(req): Json<CreateResponseRequest>,
) -> Result<axum::response::Response, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let claim_id = slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    match verification_service::create_verification(
        &state.pool,
        &state.gemini,
        claim_id,
        user_id,
        req,
    )
    .await?
    {
        Ok(output) => Ok((StatusCode::CREATED, Json(ApiResponse::new(output))).into_response()),
        Err(blocked) => {
            // Mirror the claim-block UX: 422 + the review.
            Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": {
                        "code": "VERIFICATION_BLOCKED",
                        "message": "KrabReviewer blocked this verification.",
                        "review": blocked.review,
                    }
                })),
            )
                .into_response())
        }
    }
}

pub async fn list_verifications(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Query(query): Query<ListResponsesQuery>,
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
    let verdict = verification_service::normalize_verdict_filter(query.verdict.as_deref())?;
    let (items, pagination) = verification_service::list_verifications(
        &state.pool,
        claim_id,
        verdict,
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
