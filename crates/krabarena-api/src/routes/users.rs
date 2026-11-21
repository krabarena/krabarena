//! `/v1/users/me/*` — self-management endpoints for the authenticated
//! user. Currently a single endpoint, the avatar uploader.
//!
//! `POST /v1/users/me/avatar` accepts either:
//!   - `multipart/form-data` with an `image` field (PNG / JPEG / WebP,
//!     ≤ 256 KB; matches `services::avatar_service::MAX_AVATAR_BYTES`),
//!     OR
//!   - `application/json` body `{ "url": "https://…" }` — server
//!     fetches the URL and stores the bytes. Useful when an external
//!     tool already pushed the bytes online (the bot-logo generation
//!     flow lands them on GCS first).
//!
//! Both shapes terminate in the same GCS write, so a write always
//! produces a stable `https://storage.googleapis.com/krabarena-
//! artifacts/avatars/<sha>.<ext>` URL. The response echoes that URL
//! plus the updated [`UserResponse`] so the caller can update its
//! cached view without a follow-up `GET /v1/users/me`.
//!
//! Auth: standard JWT (header `x-krabarena-token` or
//! `Authorization: Bearer …`). The route is `/me/avatar`, so the
//! caller always edits their own row; there's no admin-override
//! path in this PR.

use axum::Json;
use axum::extract::{FromRequest, Multipart, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::models::user::{UserResponse, UserRow};
use crate::routes::auth::extract_user_id;
use crate::services::avatar_service;

/// JSON body for the URL-import path. The route handler decides
/// which shape to dispatch on by sniffing the request's
/// `Content-Type` header — multipart goes through the file branch,
/// JSON through this one.
#[derive(Debug, Deserialize)]
pub struct AvatarUrlRequest {
    pub url: String,
}

/// `POST /v1/users/me/avatar`.
///
/// Content-type-driven dispatch:
///   * `multipart/form-data` → parse the `image` field, upload bytes.
///   * `application/json`    → parse `{ url }`, fetch + upload bytes.
///   * anything else         → 400.
///
/// We do the dispatch by hand rather than using axum's
/// `axum::extract::TypedHeader` so the same handler can host both
/// content types and the error messages remain readable.
pub async fn upload_avatar(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let uploaded = if content_type.starts_with("multipart/") {
        // Re-run multipart extraction on the request now that we've
        // decided the dispatch branch.
        let mut multipart = Multipart::from_request(request, &state)
            .await
            .map_err(|e| ApiError::ValidationError(e.to_string()))?;
        let mut bytes: Option<Vec<u8>> = None;
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| ApiError::ValidationError(e.to_string()))?
        {
            if field.name() == Some("image") {
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| ApiError::ValidationError(e.to_string()))?
                        .to_vec(),
                );
            }
        }
        let bytes =
            bytes.ok_or_else(|| ApiError::ValidationError("missing field: image".into()))?;
        avatar_service::upload_avatar_bytes(&state.blob_store_url, &bytes).await?
    } else if content_type.starts_with("application/json") {
        let Json(body): Json<AvatarUrlRequest> = Json::from_request(request, &state)
            .await
            .map_err(|e| ApiError::ValidationError(e.to_string()))?;
        avatar_service::upload_avatar_from_url(&state.blob_store_url, &body.url).await?
    } else {
        return Err(ApiError::ValidationError(format!(
            "unsupported content-type for avatar upload: {content_type}; \
             expected multipart/form-data or application/json"
        )));
    };

    // Persist + return the refreshed user row. Explicit column list
    // for the same cached-plan-trap reason `auth_service::USER_COLUMNS`
    // uses one — `RETURNING *` on `users` would re-trigger the
    // 0A000 issue across migrations.
    let row: UserRow = sqlx::query_as(
        r#"
        UPDATE users
           SET avatar_url = $1
         WHERE id = $2
        RETURNING id, entity_id, display_name, email, auth_method, role,
                  created_at, kind, org_role, avatar_url
        "#,
    )
    .bind(&uploaded.url)
    .bind(user_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        user = %user_id,
        avatar_url = %uploaded.url,
        bytes = uploaded.bytes,
        mime = uploaded.mime_type,
        "avatar.uploaded"
    );

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(json!({
            "url": uploaded.url,
            "user": UserResponse::from(row),
        }))),
    )
        .into_response())
}
