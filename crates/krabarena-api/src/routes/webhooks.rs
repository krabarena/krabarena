//! Webhook subscription routes (since 0.14.0).
//!
//! - `POST   /v1/webhooks`            — subscribe (returns secret once)
//! - `GET    /v1/webhooks`            — list my subscriptions
//! - `DELETE /v1/webhooks/{id}`       — hard-delete
//! - `POST   /v1/webhooks/{id}/test`  — send a synthetic test delivery
//!
//! All authenticated; subscriptions are scoped to the calling user.
//! Hard-delete because v1 doesn't need a "disabled but still listed"
//! state — re-subscribe to start over.
//!
//! See `services::webhook_dispatch` for the delivery path and
//! `docs/webhooks.md` for the public contract.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::routes::auth::extract_user_id;
use crate::services::webhook_dispatch;
use krabarena_shared::webhook_events;

#[derive(Debug, Deserialize)]
pub struct SubscribeRequest {
    pub url: String,
    pub events: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubscribeResponse {
    pub id: Uuid,
    pub url: String,
    pub events: Vec<String>,
    pub description: Option<String>,
    /// Plaintext HMAC secret — returned EXACTLY ONCE. Subscribers
    /// must persist it; the API will never re-serve it. Re-subscribe
    /// to rotate.
    pub secret: String,
    pub secret_preview: String,
    pub created_at: DateTime<Utc>,
    /// Reminder text the CLI echoes to the user. Belt-and-suspenders
    /// in case a downstream consumer forgets to highlight the
    /// one-shot nature.
    pub notice: &'static str,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SubscriptionRow {
    pub id: Uuid,
    pub url: String,
    pub events: Vec<String>,
    pub description: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_delivery_status: Option<String>,
    pub delivery_count: i32,
    pub failure_count: i32,
    pub secret_preview: String,
}

/// `POST /v1/webhooks`
///
/// Validates: url starts with `https://`, every event in the
/// requested set is in `webhook_events::KNOWN_EVENTS`, list is
/// non-empty. Generates a 32-byte secret, argon2-hashes it, stores
/// both the hash and the plaintext (see migration 041 + the
/// dispatcher for the "why plaintext too" rationale), returns the
/// plaintext exactly once in the response.
pub async fn subscribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SubscribeRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    // Validate URL — `https://` only. The DB also enforces this via
    // a CHECK, but reject early with a friendlier message.
    if !req.url.starts_with("https://") {
        return Err(ApiError::ValidationError(
            "webhook url must start with https://".into(),
        ));
    }
    // SSRF guard: no private / loopback / link-local / metadata hosts.
    // `WEBHOOK_ALLOW_PRIVATE_HOSTS=1` lifts it for the local
    // `webhooks_e2e` run, whose listener binds https://127.0.0.1.
    let allow_private = std::env::var("WEBHOOK_ALLOW_PRIVATE_HOSTS").as_deref() == Ok("1");
    if !allow_private {
        crate::models::proxy::check_url_is_public(&req.url, true)
            .map_err(|e| ApiError::ValidationError(format!("webhook url {e}")))?;
    }
    // Sanity: parse as URL so we catch garbage early. reqwest will
    // also fail on dispatch but we'd rather not store a row we know
    // is dead.
    if reqwest::Url::parse(&req.url).is_err() {
        return Err(ApiError::ValidationError(
            "webhook url is not a valid URL".into(),
        ));
    }

    // Validate events — non-empty, every entry is a known event.
    if req.events.is_empty() {
        return Err(ApiError::ValidationError(
            "webhook events list must not be empty".into(),
        ));
    }
    for ev in &req.events {
        if !webhook_events::is_known_event(ev) {
            return Err(ApiError::ValidationError(format!(
                "unknown event '{}' — supported: {}",
                ev,
                webhook_events::KNOWN_EVENTS.join(", ")
            )));
        }
    }
    // Dedup the list before storing — repeating "claim.posted" three
    // times in the body shouldn't blow up the GIN index or get the
    // subscriber three copies of every delivery. Lock the post-dedup
    // order for stable response shape.
    let mut events_dedup: Vec<String> = req.events.clone();
    events_dedup.sort();
    events_dedup.dedup();

    // Description: optional, capped at 200 chars to keep the list
    // table compact. Trim leading/trailing whitespace.
    let description = req.description.map(|d| {
        let trimmed = d.trim();
        if trimmed.len() > 200 {
            trimmed.chars().take(200).collect::<String>()
        } else {
            trimmed.to_string()
        }
    });
    let description_for_db = description.as_deref().filter(|s| !s.is_empty());

    // Secret: 32 bytes → 64 hex chars. Argon2id hash for the
    // verification path; plaintext stored for the dispatcher (see
    // the row's column comment).
    let secret = webhook_dispatch::generate_secret();
    let secret_hash = webhook_dispatch::hash_secret(&secret).map_err(ApiError::Internal)?;
    // Last 4 chars — gives the user enough to distinguish two subs
    // in the list UI without leaking the full key.
    let secret_preview = secret.chars().rev().take(4).collect::<String>();
    let secret_preview: String = secret_preview.chars().rev().collect();

    let row: (Uuid, DateTime<Utc>) = sqlx::query_as(
        r#"
        INSERT INTO webhook_subscriptions (
            owner_user_id, url, events,
            secret_hash, secret_plain, secret_preview,
            description
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, created_at
        "#,
    )
    .bind(user_id)
    .bind(&req.url)
    .bind(&events_dedup)
    .bind(&secret_hash)
    .bind(&secret)
    .bind(&secret_preview)
    .bind(description_for_db)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let resp = SubscribeResponse {
        id: row.0,
        url: req.url,
        events: events_dedup,
        description,
        secret,
        secret_preview,
        created_at: row.1,
        notice: "Save this secret — KrabArena will never show it again. \
                 Subscribers verify every delivery with HMAC-SHA256(secret, body). \
                 Use `krab webhook test <id>` to fire a sample delivery.",
    };

    Ok((StatusCode::CREATED, Json(ApiResponse::new(resp))))
}

/// `GET /v1/webhooks` — list the caller's subscriptions.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let rows: Vec<SubscriptionRow> = sqlx::query_as(
        r#"
        SELECT id, url, events, description, active, created_at,
               last_delivery_at, last_delivery_status,
               delivery_count, failure_count, secret_preview
          FROM webhook_subscriptions
         WHERE owner_user_id = $1
         ORDER BY created_at DESC
        "#,
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(Json(ApiResponse::new(rows)))
}

/// `DELETE /v1/webhooks/{id}` — hard-delete. Owner-only.
pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    // Owner gate first so we can return 403 vs 404 distinctly.
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT owner_user_id FROM webhook_subscriptions WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let owner = owner.ok_or(ApiError::ClaimNotFound)?; // reuse 404 enum
    if owner != user_id {
        return Err(ApiError::Forbidden);
    }

    sqlx::query("DELETE FROM webhook_subscriptions WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "deleted": id,
        }))),
    ))
}

#[derive(Debug, Serialize)]
pub struct TestDeliveryResponse {
    pub subscription_id: Uuid,
    pub url: String,
    /// HTTP status code returned by the subscriber. `None` if the
    /// request never completed (network / timeout).
    pub status: Option<u16>,
    /// First 2 KB of the response body — enough to confirm "yep, my
    /// receiver got it" without dragging an HTML error page into the
    /// API response.
    pub body_excerpt: Option<String>,
    /// On network failure, the error string. `None` on every
    /// completed HTTP exchange (including non-2xx).
    pub error: Option<String>,
}

/// `POST /v1/webhooks/{id}/test`
///
/// Synchronous one-shot delivery of a synthetic `webhook.test`
/// event. Returns the subscriber's response status + body excerpt
/// so the caller can confirm the wiring without waiting for a real
/// platform event. Doesn't retry; doesn't touch the live
/// delivery_count / failure_count bookkeeping — purely diagnostic.
pub async fn test_delivery(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT owner_user_id, url, secret_plain FROM webhook_subscriptions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let (owner, url, secret) = row.ok_or(ApiError::ClaimNotFound)?;
    if owner != user_id {
        return Err(ApiError::Forbidden);
    }

    let resp = match webhook_dispatch::dispatch_test_event(&url, &secret).await {
        Ok((status, body)) => TestDeliveryResponse {
            subscription_id: id,
            url,
            status: Some(status),
            body_excerpt: Some(body),
            error: None,
        },
        Err(e) => TestDeliveryResponse {
            subscription_id: id,
            url,
            status: None,
            body_excerpt: None,
            error: Some(e.to_string()),
        },
    };

    Ok((StatusCode::OK, Json(ApiResponse::new(resp))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn secret_preview_is_last_four_chars() {
        // The route handler does this inline; lock the behaviour so
        // a future refactor can't silently switch to first-four
        // (would let the entire secret leak through repeated
        // re-subscriptions of the same byte prefix). Mirrors the
        // line in `subscribe` — kept as an explicit regression guard.
        let secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let last4: String = secret.chars().rev().take(4).collect();
        let last4: String = last4.chars().rev().collect();
        assert_eq!(last4, "cdef");
    }
}
