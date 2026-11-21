use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::services::gemini_service::CLAIM_REVIEW_SYSTEM_PROMPT;

pub const KEY_REVIEW_PROMPT: &str = "review_prompt";

pub async fn get_config(pool: &PgPool, key: &str) -> Result<Option<String>, ApiError> {
    let row = sqlx::query_scalar::<_, String>("SELECT value FROM config WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(row)
}

pub async fn set_config(
    pool: &PgPool,
    key: &str,
    value: &str,
    user_id: Uuid,
) -> Result<(), ApiError> {
    // Upsert the config value
    sqlx::query(
        r#"
        INSERT INTO config (key, value, updated_by, updated_at)
        VALUES ($1, $2, $3, NOW())
        ON CONFLICT (key) DO UPDATE SET value = $2, updated_by = $3, updated_at = NOW()
        "#,
    )
    .bind(key)
    .bind(value)
    .bind(user_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    // Write history entry
    sqlx::query("INSERT INTO config_history (key, value, updated_by) VALUES ($1, $2, $3)")
        .bind(key)
        .bind(value)
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(key = key, user_id = %user_id, "config.updated");
    Ok(())
}

/// Get the KrabReviewer prompt: DB value if set, otherwise compiled-in default.
pub async fn get_review_prompt(pool: &PgPool) -> String {
    match get_config(pool, KEY_REVIEW_PROMPT).await {
        Ok(Some(prompt)) if !prompt.trim().is_empty() => prompt,
        _ => CLAIM_REVIEW_SYSTEM_PROMPT.to_string(),
    }
}
