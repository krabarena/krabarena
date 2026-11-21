use krabarena_shared::field_limits::{Field, truncate_to_cap};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::models::voter::VoterRow;

pub async fn find_or_create_voter(
    pool: &PgPool,
    provider: &str,
    provider_id: &str,
    display_name: &str,
    email: Option<&str>,
    avatar_url: Option<&str>,
) -> Result<VoterRow, ApiError> {
    // OAuth carve-out: providers occasionally return absurd values
    // (multi-KB display names, very long avatar URL query strings).
    // We TRUNCATE rather than REJECT so a pathological IdP response
    // doesn't lock the user out of the platform. The DB CHECK on
    // voters.display_name / voters.avatar_url matches these caps —
    // see `krabarena_shared::field_limits` module docstring for the
    // full policy.
    let (display_name, dn_trunc) = truncate_to_cap(display_name, Field::VotersDisplayName.max());
    if let Some(info) = dn_trunc {
        tracing::info!(
            provider = provider,
            provider_id = provider_id,
            field = "voters.display_name",
            original_chars = info.original_chars,
            cap = info.cap,
            "oauth.field.truncated"
        );
    }
    let avatar_url_owned: Option<String> = avatar_url.map(|u| {
        let (clipped, info) = truncate_to_cap(u, Field::VotersAvatarUrl.max());
        if let Some(info) = info {
            tracing::info!(
                provider = provider,
                provider_id = provider_id,
                field = "voters.avatar_url",
                original_chars = info.original_chars,
                cap = info.cap,
                "oauth.field.truncated"
            );
        }
        clipped
    });

    // Upsert: create voter if not exists, otherwise return existing
    let row = sqlx::query_as::<_, VoterRow>(
        r#"
        INSERT INTO voters (provider, provider_id, display_name, email, avatar_url)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (provider, provider_id)
        DO UPDATE SET display_name = EXCLUDED.display_name, email = EXCLUDED.email, avatar_url = EXCLUDED.avatar_url
        RETURNING *
        "#,
    )
    .bind(provider)
    .bind(provider_id)
    .bind(&display_name)
    .bind(email)
    .bind(avatar_url_owned.as_deref())
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(row)
}

/// Create a voter session. `verified_email` is the address the login
/// method actually proved (email code / magic link, or a provider email
/// flagged verified) — `None` when nothing was proved. It is the ONLY
/// email trusted for admin-token minting on `/auth/voter/me`.
pub async fn create_session(
    pool: &PgPool,
    voter_id: Uuid,
    verified_email: Option<&str>,
) -> Result<String, ApiError> {
    let token = Uuid::new_v4().to_string();
    let expires_at = chrono::Utc::now() + chrono::Duration::days(30);

    sqlx::query(
        "INSERT INTO voter_sessions (voter_id, token, expires_at, verified_email) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(voter_id)
    .bind(&token)
    .bind(expires_at)
    .bind(verified_email)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(token)
}

/// The email the session's login proved, if any (see `create_session`).
pub async fn session_verified_email(
    pool: &PgPool,
    token: &str,
) -> Result<Option<String>, ApiError> {
    let row: Option<Option<String>> = sqlx::query_scalar(
        "SELECT verified_email FROM voter_sessions WHERE token = $1 AND expires_at > NOW()",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(row.flatten())
}

pub async fn delete_session(pool: &PgPool, token: &str) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM voter_sessions WHERE token = $1")
        .bind(token)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(())
}

pub async fn get_voter_by_session(
    pool: &PgPool,
    token: &str,
) -> Result<Option<VoterRow>, ApiError> {
    let row = sqlx::query_as::<_, VoterRow>(
        r#"
        SELECT v.* FROM voters v
        JOIN voter_sessions s ON s.voter_id = v.id
        WHERE s.token = $1 AND s.expires_at > NOW()
        "#,
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(row)
}
