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
use crate::services::profile_service;

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

    crate::services::profile_service::mark_avatar_edited(&state.pool, user_id).await?;

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

#[derive(Debug, serde::Serialize)]
pub struct MyProfileResponse {
    pub user_id: uuid::Uuid,
    pub display_name: String,
    pub profile_url: String,
    pub profile: profile_service::PublicProfile,
}

async fn my_profile_response(
    state: &AppState,
    user_id: uuid::Uuid,
) -> Result<MyProfileResponse, ApiError> {
    let display_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or_else(|| ApiError::NotFound("Agent not found".into()))?;
    let profile = profile_service::get_public_profile(&state.pool, user_id)
        .await?
        .unwrap_or_default();
    Ok(MyProfileResponse {
        user_id,
        display_name,
        profile_url: format!(
            "{}/agents/{user_id}",
            state.public_base_url.trim_end_matches('/')
        ),
        profile,
    })
}

pub async fn get_my_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    Ok(Json(ApiResponse::new(
        my_profile_response(&state, user_id).await?,
    )))
}

pub async fn patch_my_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<profile_service::ProfileUpdateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let edit = req.into_edit()?;
    profile_service::update_profile(&state.pool, user_id, &edit).await?;
    tracing::info!(user = %user_id, via = "jwt", "profile.updated");
    Ok(Json(ApiResponse::new(
        my_profile_response(&state, user_id).await?,
    )))
}

pub async fn authorize_agent_profile_edit(
    pool: &sqlx::PgPool,
    headers: &HeaderMap,
    jwt_secret: &str,
    target: uuid::Uuid,
) -> Result<&'static str, ApiError> {
    let mut authenticated = false;
    if crate::routes::auth::bearer_token(headers).is_some() {
        if let Ok(user_id) = extract_user_id(headers, jwt_secret) {
            authenticated = true;
            if user_id == target {
                return Ok("jwt");
            }
        }
    }
    if let Some(token) = crate::routes::profile::voter_token(headers) {
        if let Some(voter) =
            crate::services::voter_service::get_voter_by_session(pool, &token).await?
        {
            authenticated = true;
            if let Some(email) = voter.email.as_deref() {
                if profile_service::linked_agent_id(pool, email).await? == Some(target) {
                    return Ok("voter");
                }
            }
        }
    }
    Err(if authenticated {
        ApiError::Forbidden
    } else {
        ApiError::Unauthorized
    })
}

pub async fn patch_agent_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(user_id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<profile_service::ProfileUpdateRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let via =
        authorize_agent_profile_edit(&state.pool, &headers, &state.jwt_secret, user_id).await?;
    let edit = req.into_edit()?;
    profile_service::update_profile(&state.pool, user_id, &edit).await?;
    tracing::info!(user = %user_id, via = via, "profile.updated");
    Ok(Json(ApiResponse::new(
        my_profile_response(&state, user_id).await?,
    )))
}
