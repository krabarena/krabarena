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

#[derive(Debug, Deserialize, Default)]
pub struct NotificationQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    #[serde(default)]
    pub unread: bool,
    /// Comma-separated kinds, e.g. `response,reply,mention`.
    pub kind: Option<String>,
}

impl NotificationQuery {
    pub fn filter(&self) -> activity_service::NotificationFilter {
        let kinds = self.kind.as_deref().map(|k| {
            k.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        });
        activity_service::NotificationFilter {
            unread_only: self.unread,
            kinds: kinds.filter(|k| !k.is_empty()),
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct UnreadCount {
    pub unread: i64,
    pub capped: bool,
}

impl UnreadCount {
    pub fn new(n: i64) -> Self {
        Self {
            unread: n.min(activity_service::UNREAD_COUNT_CAP),
            capped: n > activity_service::UNREAD_COUNT_CAP,
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct MarkedRead {
    pub marked: usize,
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

/// GET /v1/notifications — the user's notification feed, newest first.
/// `?unread=true` keeps unread items only; `?kind=a,b` narrows by kind.
pub async fn user_notifications(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<NotificationQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let limit = query.limit.unwrap_or(50);
    let (items, pagination) = activity_service::user_notifications_filtered(
        &state.pool,
        user_id,
        query.cursor.as_deref(),
        limit,
        &query.filter(),
    )
    .await?;

    Ok(Json(PaginatedResponse {
        data: items,
        pagination,
        meta: ResponseMeta::generate(),
    }))
}

/// GET /v1/notifications/unread-count
pub async fn notifications_unread_count(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let n = activity_service::unread_count(&state.pool, user_id).await?;
    Ok(Json(crate::envelope::ApiResponse::new(UnreadCount::new(n))))
}

/// POST /v1/notifications/{id}/read
pub async fn notification_mark_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    activity_service::mark_read(&state.pool, user_id, &[item_id]).await?;
    Ok(Json(crate::envelope::ApiResponse::new(MarkedRead {
        marked: 1,
    })))
}

/// POST /v1/notifications/read-all
pub async fn notifications_mark_all_read(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    activity_service::mark_all_read(&state.pool, user_id).await?;
    let n = activity_service::unread_count(&state.pool, user_id).await?;
    Ok(Json(crate::envelope::ApiResponse::new(UnreadCount::new(n))))
}
