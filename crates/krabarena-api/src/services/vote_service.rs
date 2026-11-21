use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::models::vote::{VoteResponse, VoteRow};

pub async fn cast_vote(
    pool: &PgPool,
    voter_id: Uuid,
    claim_id: Uuid,
    vote_type: &str,
) -> Result<VoteResponse, ApiError> {
    if vote_type != "up" && vote_type != "down" {
        return Err(ApiError::ValidationError(
            "vote_type must be 'up' or 'down'".into(),
        ));
    }

    crate::services::claim_visibility::ensure_visible(pool, claim_id, &None).await?;

    // Check for existing vote
    let existing =
        sqlx::query_as::<_, VoteRow>("SELECT * FROM votes WHERE voter_id = $1 AND claim_id = $2")
            .bind(voter_id)
            .bind(claim_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

    if let Some(old_vote) = existing {
        if old_vote.vote_type == vote_type {
            // Same vote — remove it (toggle off)
            sqlx::query("DELETE FROM votes WHERE id = $1")
                .bind(old_vote.id)
                .execute(pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;

            let col = if vote_type == "up" {
                "upvote_count"
            } else {
                "downvote_count"
            };
            sqlx::query(&format!(
                "UPDATE claims SET {} = GREATEST({} - 1, 0) WHERE id = $1",
                col, col
            ))
            .bind(claim_id)
            .execute(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

            // Return a "removed" response
            return Ok(VoteResponse {
                id: old_vote.id,
                claim_id,
                vote_type: "removed".to_string(),
            });
        }

        // Different vote — flip it
        sqlx::query("UPDATE votes SET vote_type = $1 WHERE id = $2")
            .bind(vote_type)
            .bind(old_vote.id)
            .execute(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

        let (inc_col, dec_col) = if vote_type == "up" {
            ("upvote_count", "downvote_count")
        } else {
            ("downvote_count", "upvote_count")
        };
        sqlx::query(&format!(
            "UPDATE claims SET {} = {} + 1, {} = GREATEST({} - 1, 0) WHERE id = $1",
            inc_col, inc_col, dec_col, dec_col
        ))
        .bind(claim_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

        return Ok(VoteResponse {
            id: old_vote.id,
            claim_id,
            vote_type: vote_type.to_string(),
        });
    }

    // New vote
    let row = sqlx::query_as::<_, VoteRow>(
        "INSERT INTO votes (voter_id, claim_id, vote_type) VALUES ($1, $2, $3) RETURNING *",
    )
    .bind(voter_id)
    .bind(claim_id)
    .bind(vote_type)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let col = if vote_type == "up" {
        "upvote_count"
    } else {
        "downvote_count"
    };
    sqlx::query(&format!(
        "UPDATE claims SET {} = {} + 1 WHERE id = $1",
        col, col
    ))
    .bind(claim_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(VoteResponse::from(row))
}

pub async fn get_vote_counts(pool: &PgPool, claim_id: Uuid) -> Result<(i32, i32), ApiError> {
    let row = sqlx::query_as::<_, (i32, i32)>(
        "SELECT upvote_count, downvote_count FROM claims WHERE id = $1",
    )
    .bind(claim_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .ok_or(ApiError::ClaimNotFound)?;

    Ok(row)
}
