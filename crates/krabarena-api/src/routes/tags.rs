//! `GET /v1/tags` — the unified tag dictionary (autocomplete source).
//!
//! Tags live in two columns by design — `battles.domain_tags` and
//! `claims.topic_tags` (the latter only populated on standalone claims).
//! This endpoint UNIONs both into a single, case-folded vocabulary with
//! occurrence counts, so the home-page tag autocomplete and the `/t/<tag>`
//! landing pages treat "domain" and "topic" tags as one namespace.
//!
//! Matching is case-insensitive (everything is `lower()`ed) so `Rust`,
//! `rust`, and `RUST` collapse to one entry. The corpus is small (tens of
//! battles, hundreds of claims) so the unnest+group aggregation is cheap;
//! if the tag space grows we'd cache it or back it with a materialized
//! view.

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::{Json, http::StatusCode};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct TagsQuery {
    /// Optional case-insensitive prefix filter for autocomplete. Empty /
    /// missing returns the most common tags overall.
    pub q: Option<String>,
    /// Optional 1..100 result cap. Defaults to 20.
    pub limit: Option<i64>,
}

pub async fn list_tags(
    State(state): State<AppState>,
    Query(params): Query<TagsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let prefix = params.q.as_deref().unwrap_or("").trim().to_lowercase();
    let limit = params.limit.unwrap_or(20).clamp(1, 100);

    let rows = crate::services::tag_service::list_tags(&state.pool, &prefix, limit)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("tags query failed: {e}")))?;

    let data: Vec<_> = rows
        .into_iter()
        .map(|row| json!({ "tag": row.tag, "count": row.count }))
        .collect();

    Ok((StatusCode::OK, Json(json!({ "data": data }))))
}
