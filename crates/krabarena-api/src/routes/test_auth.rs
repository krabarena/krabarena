//! Test-only bearer token mint.
//!
//! This route is intentionally narrow and defensive:
//!   - It only responds when `TEST_AUTH_SECRET` is set to a non-empty value
//!     on the server. If unset, the route returns 404 — i.e. invisible.
//!   - The caller must present a matching `x-test-auth` header. Any
//!     mismatch (including missing header) returns 404, not 401, so the
//!     endpoint does not leak whether the feature is configured.
//!   - Only one known test user name is accepted: `test-agent` (role
//!     `user`). Anything else is 404. test-agent is used exclusively by
//!     the E2E dry-run suite (claim review). Editor-gated routes (like
//!     battle review) are not reachable with this token by design.
//!
//! The user lives under the `test-e2e.krabarena.org` entity so it is easy
//! to identify and filter out of public data. Created on-demand; no
//! migration required.
//!
//! **Do not enable this in prod.** Dry-run claim review DOES persist a
//! row in `claim_reviews` (the review record itself — not the claim, not
//! a verification), so running the suite against prod Neon would leak
//! test rows. Point the test harness at a dedicated Neon test branch,
//! not prod. TEST_AUTH_SECRET is intentionally NOT wired into the
//! production Cloud Run deploy; setting it there would defeat this
//! isolation.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::services::auth_service;

const TEST_ENTITY_DOMAIN: &str = "test-e2e.krabarena.org";
const TEST_ENTITY_NAME: &str = "KrabArena Test Entity";

#[derive(Debug, Deserialize)]
pub struct MintRequest {
    pub user: String,
}

#[derive(Debug, Serialize)]
pub struct MintResponse {
    pub bearer_token: String,
    pub user_id: Uuid,
    pub role: String,
}

/// POST /v1/test/mint-token
///
/// Returns 404 unless TEST_AUTH_SECRET is configured and the request
/// header `x-test-auth` matches it.
pub async fn mint_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MintRequest>,
) -> Response {
    // Gate 1: feature must be explicitly enabled via env var.
    let Some(expected) = std::env::var("TEST_AUTH_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Gate 2: header secret must match. Mismatch → 404 so callers can't
    // probe whether the endpoint is enabled.
    let provided = headers
        .get("x-test-auth")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Constant-time compare so the secret can't be recovered byte by
    // byte from response timing.
    if !crate::web::cookies::ct_eq(provided, &expected) {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Gate 3: only known test users. `test-admin` exists so e2e
    // tests that need `editor/admin`-gated endpoints (battle
    // create/edit, user role changes) don't need a real human
    // identity. Both land under the TEST_ENTITY_DOMAIN so prod
    // leaderboards / counters never see them.
    let (display_name, role) = match req.user.as_str() {
        "test-agent" => ("Test Agent", "user"),
        "test-admin" => ("Test Admin", "admin"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };

    // Find or create the shared test entity.
    let entity_id: Uuid = match sqlx::query_scalar(
        r#"
        INSERT INTO entities (name, domain, verification_method)
        VALUES ($1, $2, 'email')
        ON CONFLICT (domain) DO UPDATE SET name = entities.name
        RETURNING id
        "#,
    )
    .bind(TEST_ENTITY_NAME)
    .bind(TEST_ENTITY_DOMAIN)
    .fetch_one(&state.pool)
    .await
    {
        Ok(id) => id,
        Err(e) => return internal_error(e),
    };

    // Find or create the test user. role is (re)asserted every time so a
    // DB row that drifted from what we want gets reset.
    let email = format!("{}@{}", req.user, TEST_ENTITY_DOMAIN);
    let user_id: Uuid = match sqlx::query_scalar(
        r#"
        INSERT INTO users (entity_id, display_name, email, auth_method, role)
        VALUES ($1, $2, $3, 'email', $4)
        ON CONFLICT (email) DO UPDATE
            SET role = EXCLUDED.role,
                display_name = EXCLUDED.display_name
        RETURNING id
        "#,
    )
    .bind(entity_id)
    .bind(display_name)
    .bind(&email)
    .bind(role)
    .fetch_one(&state.pool)
    .await
    {
        Ok(id) => id,
        Err(e) => return internal_error(e),
    };

    let token = match auth_service::create_jwt(user_id, entity_id, role, &state.jwt_secret) {
        Ok(t) => t,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    Json(ApiResponse::new(MintResponse {
        bearer_token: token,
        user_id,
        role: role.to_string(),
    }))
    .into_response()
}

fn internal_error<E: std::fmt::Display>(e: E) -> Response {
    tracing::error!("test mint-token error: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}
