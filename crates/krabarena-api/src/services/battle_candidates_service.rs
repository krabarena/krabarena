//! Battle candidates: community-proposed Battle titles that editors
//! curate into real Battles. Read-public, propose + vote require a
//! voter session (the same one that powers /v1/claims/.../votes).
//!
//! Ranking: votes in the last 7 days, then all-time vote total, then
//! recency. The 7-day window favours fresh interest over stale
//! pile-ons, so an old proposal that suddenly attracts attention can
//! climb back to the top.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;

#[derive(Debug, Clone, Serialize)]
pub struct BattleCandidate {
    pub id: Uuid,
    pub title: String,
    pub rationale: String,
    pub proposer_voter_id: Uuid,
    pub proposer_display_name: String,
    pub status: String,
    pub votes_7d: i64,
    pub votes_total: i64,
    pub created_at: DateTime<Utc>,
    pub viewer_voted: bool,
}

/// Top-N + paginated list. `viewer_voter_id` populates `viewer_voted`
/// so the UI can render the up-arrow in its active state for the
/// current user without a second query.
///
/// `recent_days = Some(d)` keeps only candidates proposed or upvoted in
/// the last `d` days — the home "Proposed battles · 7-day" panel. With
/// `None` (the full `/battle_candidates` list and the API) every open
/// candidate is returned, as before.
pub async fn list_candidates(
    pool: &PgPool,
    viewer_voter_id: Option<Uuid>,
    limit: i64,
    offset: i64,
    recent_days: Option<i32>,
) -> Result<Vec<BattleCandidate>, ApiError> {
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            Uuid,
            String,
            String,
            i64,
            i64,
            DateTime<Utc>,
            bool,
        ),
    >(
        r#"
        SELECT c.id,
               c.title,
               c.rationale,
               c.proposer_voter_id,
               COALESCE(v.display_name, '') AS proposer_display_name,
               c.status,
               COALESCE((SELECT COUNT(*) FROM battle_candidate_votes
                         WHERE candidate_id = c.id
                           AND created_at >= now() - interval '7 days'), 0) AS votes_7d,
               COALESCE((SELECT COUNT(*) FROM battle_candidate_votes
                         WHERE candidate_id = c.id), 0)                       AS votes_total,
               c.created_at,
               COALESCE(
                   (SELECT TRUE FROM battle_candidate_votes
                    WHERE candidate_id = c.id AND voter_id = $1
                    LIMIT 1),
                   FALSE
               ) AS viewer_voted
        FROM battle_candidates c
        LEFT JOIN voters v ON v.id = c.proposer_voter_id
        WHERE c.status = 'open'
          AND ($4::int IS NULL
               OR c.created_at >= now() - make_interval(days => $4::int)
               OR EXISTS (SELECT 1 FROM battle_candidate_votes rv
                          WHERE rv.candidate_id = c.id
                            AND rv.created_at >= now() - make_interval(days => $4::int)))
        ORDER BY votes_7d DESC, votes_total DESC, c.created_at DESC
        LIMIT $2 OFFSET $3
        "#,
    )
    .bind(viewer_voter_id.unwrap_or(Uuid::nil()))
    .bind(limit)
    .bind(offset)
    .bind(recent_days)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                title,
                rationale,
                proposer_voter_id,
                proposer_display_name,
                status,
                votes_7d,
                votes_total,
                created_at,
                viewer_voted,
            )| BattleCandidate {
                id,
                title,
                rationale,
                proposer_voter_id,
                proposer_display_name,
                status,
                votes_7d,
                votes_total,
                created_at,
                viewer_voted,
            },
        )
        .collect())
}

/// Open-candidate count; `recent_days` applies the same window as
/// [`list_candidates`] so the home panel's "see N more" hint matches
/// what it lists.
pub async fn count_open(pool: &PgPool, recent_days: Option<i32>) -> Result<i64, ApiError> {
    let n: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM battle_candidates c
         WHERE c.status = 'open'
           AND ($1::int IS NULL
                OR c.created_at >= now() - make_interval(days => $1::int)
                OR EXISTS (SELECT 1 FROM battle_candidate_votes rv
                           WHERE rv.candidate_id = c.id
                             AND rv.created_at >= now() - make_interval(days => $1::int)))",
    )
    .bind(recent_days)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(n.0)
}

pub async fn create_candidate(
    pool: &PgPool,
    proposer_voter_id: Uuid,
    title: &str,
    rationale: &str,
) -> Result<Uuid, ApiError> {
    let title = title.trim();
    let rationale = rationale.trim();
    use krabarena_shared::field_limits::Field;
    Field::BattleCandidatesTitle.check(title)?;
    Field::BattleCandidatesRationale.check(rationale)?;

    let row: Result<(Uuid,), sqlx::Error> = sqlx::query_as(
        "INSERT INTO battle_candidates (title, rationale, proposer_voter_id) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(title)
    .bind(rationale)
    .bind(proposer_voter_id)
    .fetch_one(pool)
    .await;

    match row {
        Ok((id,)) => Ok(id),
        Err(sqlx::Error::Database(db))
            if db.constraint() == Some("idx_battle_candidates_unique_open_title") =>
        {
            Err(ApiError::ValidationError(
                "A candidate with that title is already open — upvote it instead.".into(),
            ))
        }
        Err(e) => Err(ApiError::Internal(e.into())),
    }
}

pub async fn vote(pool: &PgPool, candidate_id: Uuid, voter_id: Uuid) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO battle_candidate_votes (candidate_id, voter_id) VALUES ($1, $2)
         ON CONFLICT (candidate_id, voter_id) DO NOTHING",
    )
    .bind(candidate_id)
    .bind(voter_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(())
}

pub async fn unvote(pool: &PgPool, candidate_id: Uuid, voter_id: Uuid) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM battle_candidate_votes WHERE candidate_id = $1 AND voter_id = $2")
        .bind(candidate_id)
        .bind(voter_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(())
}

/// Editor / admin "delete" — sets `status = 'dismissed'` so the
/// candidate drops off the `/battle_candidates` listing (which
/// filters `WHERE status = 'open'`). Row stays for audit trail and
/// so the partial-unique-index on open titles correctly frees the
/// title for re-proposal. Promotion to a real Battle is currently
/// editor-curation outside this path — once a Battle is created,
/// run this to clear the candidate.
pub async fn dismiss_candidate(pool: &PgPool, candidate_id: Uuid) -> Result<(), ApiError> {
    // Only flip `open` rows so re-running on an already-dismissed
    // candidate is a 4xx, not an idempotent no-op. Keeps the audit
    // trail clean and tells the caller "nothing to do here".
    let res = sqlx::query(
        "UPDATE battle_candidates SET status = 'dismissed', updated_at = now()
         WHERE id = $1 AND status = 'open'",
    )
    .bind(candidate_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    if res.rows_affected() == 0 {
        // Could be 404 (no such id) or 409 (already dismissed/promoted).
        // We don't distinguish — ValidationError surfaces a 400 with a
        // clear message that covers both cases, and the editor's next
        // action is the same either way (verify the id, move on).
        return Err(ApiError::ValidationError(
            "No open battle candidate with that id (already dismissed or never existed).".into(),
        ));
    }
    Ok(())
}
