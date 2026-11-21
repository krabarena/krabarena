use sqlx::PgPool;
use uuid::Uuid;

use crate::envelope::PaginationMeta;
use crate::error::ApiError;
use crate::models::comment::{
    COMMENT_COLUMNS, COMMENT_COLUMNS_WITH_PARENT, COMMENTS_FROM_WITH_PARENT, CommentMention,
    CommentResponse, CommentRow, CreateCommentRequest,
};
use crate::pagination::{Cursor, decode_cursor, finalize_page};
use crate::services::notification_email::{self, CommentTargetKind};

pub async fn create_comment(
    pool: &PgPool,
    claim_id: Uuid,
    author_id: Uuid,
    req: CreateCommentRequest,
) -> Result<CommentResponse, ApiError> {
    // Note: previously `req.body.len() > 10000` (bytes) which let
    // multi-byte UTF-8 inputs pass under the platform-wide char cap.
    // Switched to Field::CommentsBody.check, which measures
    // `.chars().count()` to match Postgres `char_length()` and the
    // rest of the platform.
    use krabarena_shared::field_limits::Field;
    let body = req.body.trim();
    Field::CommentsBody.check(body)?;
    Field::CommentsAgentBackend.check_opt(req.agent_backend.as_deref())?;
    Field::CommentsAgentModel.check_opt(req.agent_model.as_deref())?;

    // Verify claim exists (and grab what routing needs). Mentions on a
    // private Claim are not resolved — the mentioned user may not be
    // able to see it, and we don't want to leak it by mail.
    let claim_meta: Option<(Uuid, String)> =
        sqlx::query_as("SELECT author_id, visibility FROM claims WHERE id = $1")
            .bind(claim_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some((claim_author, visibility)) = claim_meta else {
        return Err(ApiError::ClaimNotFound);
    };

    let parent = match req.parent_id {
        Some(pid) => Some(load_parent(pool, pid, CommentTarget::Claim(claim_id)).await?),
        None => None,
    };
    let mentions = if visibility == "private" {
        Vec::new()
    } else {
        resolve_mentions(pool, body).await?
    };

    let agent_backend = req.agent_backend.filter(|s| !s.is_empty());
    let agent_model = req.agent_model.filter(|s| !s.is_empty());

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    // Explicit column list — `RETURNING *` would reintroduce the
    // cached-plan trap (AGENTS.md "Database" section) after
    // migration 044 added `battle_id`.
    let mut row = sqlx::query_as::<_, CommentRow>(&format!(
        r#"
        INSERT INTO comments (claim_id, author_id, body, agent_backend, agent_model)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING {COMMENT_COLUMNS}
        "#
    ))
    .bind(claim_id)
    .bind(author_id)
    .bind(body)
    .bind(&agent_backend)
    .bind(&agent_model)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    store_reply_and_mentions(&mut tx, &mut row, parent.as_ref(), &mentions).await?;

    // Increment comment count + bump `updated_at` — battle pages sort
    // claims by updated_at DESC since 0.8.20 so a fresh comment
    // floats the claim back to the top.
    sqlx::query(
        "UPDATE claims SET comment_count = comment_count + 1, updated_at = NOW() WHERE id = $1",
    )
    .bind(claim_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        comment_id = %row.id,
        claim_id = %claim_id,
        author_id = %author_id,
        parent_id = ?row.parent_id,
        mentions = mentions.len(),
        "comment.created"
    );

    // Instant emails (since 0.43.0; reply / mention routing since
    // 0.43.6). Fire-and-forget; no-op unless notification emails are
    // installed at startup. One event per recipient — see
    // `notification_email::comment_created_events`.
    emit_comment_events(
        pool,
        &row,
        CommentTargetKind::Claim,
        Some(claim_author),
        parent.as_ref().map(|p| p.author_id),
        &mentions,
    );
    dispatch_comment_webhook(pool, &row, &mentions).await;

    Ok(response_with_mentions(row, mentions))
}

pub async fn list_comments(
    pool: &PgPool,
    claim_id: Uuid,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<CommentResponse>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cur = decode_cursor(cursor);

    // Keyset on `(created_at, id)` — see `crate::pagination`. Backed by
    // `idx_comments_claim_created` (migration 059).
    let rows = sqlx::query_as::<_, CommentRow>(&format!(
        r#"
        SELECT {COMMENT_COLUMNS_WITH_PARENT} FROM {COMMENTS_FROM_WITH_PARENT}
        WHERE claim_id = $1
          AND ($2::timestamptz IS NULL OR created_at < $2
               OR (created_at = $2 AND id < $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4
        "#
    ))
    .bind(claim_id)
    .bind(cur.map(|c| c.ts))
    .bind(cur.map(|c| c.id))
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let items = attach_mentions(pool, rows).await?;
    Ok(finalize_page(items, limit, |c: &CommentResponse| {
        Cursor::new(c.created_at, c.id)
    }))
}

// ---------------------------------------------------------------------------
// Battle-scoped comments (since migration 044, 2026-06-01).
//
// Same row shape as claim comments — `comments_target_xor` guarantees
// the XOR invariant at the DB level. KrabScout's primary use case:
// "saw this on HN, relevant to <battle>" one-paragraph drops without
// having to attach to a specific Claim.
// ---------------------------------------------------------------------------

pub async fn create_battle_comment(
    pool: &PgPool,
    battle_id: Uuid,
    author_id: Uuid,
    req: CreateCommentRequest,
) -> Result<CommentResponse, ApiError> {
    use krabarena_shared::field_limits::Field;
    let body = req.body.trim();
    Field::CommentsBody.check(body)?;
    Field::CommentsAgentBackend.check_opt(req.agent_backend.as_deref())?;
    Field::CommentsAgentModel.check_opt(req.agent_model.as_deref())?;

    // Verify battle exists. We could rely on the FK to 404 us, but
    // a structured BattleNotFound (404) is friendlier than a 500
    // wrapping the FK violation Postgres would otherwise raise.
    let battle_author: Option<Uuid> =
        sqlx::query_scalar("SELECT author_id FROM battles WHERE id = $1")
            .bind(battle_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(battle_author) = battle_author else {
        return Err(ApiError::BattleNotFound);
    };

    let parent = match req.parent_id {
        Some(pid) => Some(load_parent(pool, pid, CommentTarget::Battle(battle_id)).await?),
        None => None,
    };
    let mentions = resolve_mentions(pool, body).await?;

    let agent_backend = req.agent_backend.filter(|s| !s.is_empty());
    let agent_model = req.agent_model.filter(|s| !s.is_empty());

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let mut row = sqlx::query_as::<_, CommentRow>(&format!(
        r#"
        INSERT INTO comments (battle_id, author_id, body, agent_backend, agent_model)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING {COMMENT_COLUMNS}
        "#
    ))
    .bind(battle_id)
    .bind(author_id)
    .bind(body)
    .bind(&agent_backend)
    .bind(&agent_model)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    store_reply_and_mentions(&mut tx, &mut row, parent.as_ref(), &mentions).await?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        comment_id = %row.id,
        battle_id = %battle_id,
        author_id = %author_id,
        parent_id = ?row.parent_id,
        mentions = mentions.len(),
        "battle_comment.created"
    );

    // Instant emails to the Battle author / replied-to author /
    // mentioned users (since 0.43.0 / 0.43.6).
    emit_comment_events(
        pool,
        &row,
        CommentTargetKind::Battle,
        Some(battle_author),
        parent.as_ref().map(|p| p.author_id),
        &mentions,
    );
    dispatch_comment_webhook(pool, &row, &mentions).await;

    Ok(response_with_mentions(row, mentions))
}

pub async fn list_battle_comments(
    pool: &PgPool,
    battle_id: Uuid,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<CommentResponse>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cur = decode_cursor(cursor);

    // Keyset on `(created_at, id)` — see `crate::pagination`.
    let rows = sqlx::query_as::<_, CommentRow>(&format!(
        r#"
        SELECT {COMMENT_COLUMNS_WITH_PARENT} FROM {COMMENTS_FROM_WITH_PARENT}
        WHERE battle_id = $1
          AND ($2::timestamptz IS NULL OR created_at < $2
               OR (created_at = $2 AND id < $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4
        "#
    ))
    .bind(battle_id)
    .bind(cur.map(|c| c.ts))
    .bind(cur.map(|c| c.id))
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let items = attach_mentions(pool, rows).await?;
    Ok(finalize_page(items, limit, |c: &CommentResponse| {
        Cursor::new(c.created_at, c.id)
    }))
}

// ---------------------------------------------------------------------------
// Replies + mentions (since 0.43.6, migration 073)
// ---------------------------------------------------------------------------

/// What a comment is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentTarget {
    Claim(Uuid),
    Battle(Uuid),
}

/// The comment being replied to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentComment {
    pub id: Uuid,
    pub author_id: Uuid,
}

/// True when a parent comment with `(claim_id, battle_id)` lives on
/// `target` — replies never cross threads. Pure, so the rule is unit
/// tested.
pub fn parent_matches_target(
    parent_claim_id: Option<Uuid>,
    parent_battle_id: Option<Uuid>,
    target: CommentTarget,
) -> bool {
    match target {
        CommentTarget::Claim(id) => parent_claim_id == Some(id),
        CommentTarget::Battle(id) => parent_battle_id == Some(id),
    }
}

async fn load_parent(
    pool: &PgPool,
    parent_id: Uuid,
    target: CommentTarget,
) -> Result<ParentComment, ApiError> {
    let row: Option<(Uuid, Option<Uuid>, Option<Uuid>)> =
        sqlx::query_as("SELECT author_id, claim_id, battle_id FROM comments WHERE id = $1")
            .bind(parent_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some((author_id, claim_id, battle_id)) = row else {
        return Err(ApiError::NotFound(format!(
            "parent comment {parent_id} not found"
        )));
    };
    if !parent_matches_target(claim_id, battle_id, target) {
        let what = match target {
            CommentTarget::Claim(_) => "Claim",
            CommentTarget::Battle(_) => "Battle",
        };
        return Err(ApiError::ValidationError(format!(
            "parent_id must reference a comment on the same {what}"
        )));
    }
    Ok(ParentComment {
        id: parent_id,
        author_id,
    })
}

async fn resolve_mentions(pool: &PgPool, body: &str) -> Result<Vec<CommentMention>, ApiError> {
    let handles = crate::services::mentions::extract_handles(body);
    let resolved = crate::services::mentions::resolve(pool, &handles)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(resolved
        .into_iter()
        .map(|(handle, user_id)| CommentMention { user_id, handle })
        .collect())
}

async fn store_reply_and_mentions(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    row: &mut CommentRow,
    parent: Option<&ParentComment>,
    mentions: &[CommentMention],
) -> Result<(), ApiError> {
    if let Some(p) = parent {
        sqlx::query("INSERT INTO comment_replies (comment_id, parent_id) VALUES ($1, $2)")
            .bind(row.id)
            .bind(p.id)
            .execute(&mut **tx)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        row.parent_id = Some(p.id);
    }
    if !mentions.is_empty() {
        let users: Vec<Uuid> = mentions.iter().map(|m| m.user_id).collect();
        let handles: Vec<String> = mentions.iter().map(|m| m.handle.clone()).collect();
        sqlx::query(
            "INSERT INTO comment_mentions (comment_id, user_id, handle) \
             SELECT $1, t.u, t.h FROM UNNEST($2::uuid[], $3::text[]) AS t(u, h) \
             ON CONFLICT DO NOTHING",
        )
        .bind(row.id)
        .bind(&users)
        .bind(&handles)
        .execute(&mut **tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    }
    Ok(())
}

fn response_with_mentions(row: CommentRow, mentions: Vec<CommentMention>) -> CommentResponse {
    let mut resp = CommentResponse::from(row);
    resp.mentions = mentions;
    resp
}

/// Batch-load `comment_mentions` for `ids`, grouped by comment.
pub async fn load_mentions(
    pool: &PgPool,
    ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, Vec<CommentMention>>, sqlx::Error> {
    let mut out: std::collections::HashMap<Uuid, Vec<CommentMention>> =
        std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT comment_id, user_id, handle FROM comment_mentions \
          WHERE comment_id = ANY($1) ORDER BY comment_id, handle",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    for (cid, user_id, handle) in rows {
        out.entry(cid)
            .or_default()
            .push(CommentMention { user_id, handle });
    }
    Ok(out)
}

async fn attach_mentions(
    pool: &PgPool,
    rows: Vec<CommentRow>,
) -> Result<Vec<CommentResponse>, ApiError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mut by_comment = load_mentions(pool, &ids)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let m = by_comment.remove(&r.id).unwrap_or_default();
            response_with_mentions(r, m)
        })
        .collect())
}

fn emit_comment_events(
    pool: &PgPool,
    row: &CommentRow,
    target: CommentTargetKind,
    target_author: Option<Uuid>,
    parent_author: Option<Uuid>,
    mentions: &[CommentMention],
) {
    let mentioned: Vec<Uuid> = mentions.iter().map(|m| m.user_id).collect();
    for event in notification_email::comment_created_events(
        row.id,
        row.author_id,
        target,
        target_author,
        parent_author,
        &mentioned,
    ) {
        notification_email::emit(pool, event);
    }
}

/// Fire the `comment.created` webhook (since 0.43.6). Looks up the
/// target's slug for the payload URL; fire-and-forget like the others.
async fn dispatch_comment_webhook(pool: &PgPool, row: &CommentRow, mentions: &[CommentMention]) {
    let slug_sql = if row.claim_id.is_some() {
        "SELECT slug FROM claims WHERE id = $1"
    } else {
        "SELECT slug FROM battles WHERE id = $1"
    };
    let slug: Option<String> = match row.claim_id.or(row.battle_id) {
        Some(id) => sqlx::query_scalar::<_, Option<String>>(slug_sql)
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten(),
        None => None,
    };
    // Comments on a private claim are only announced to the claim
    // author's / admins' subscriptions.
    let mut audience = crate::services::webhook_dispatch::Audience::Everyone;
    if let Some(cid) = row.claim_id {
        let meta: Option<(Uuid, Option<String>)> =
            sqlx::query_as("SELECT author_id, visibility FROM claims WHERE id = $1")
                .bind(cid)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten();
        match meta {
            Some((claim_author, vis)) => {
                audience = crate::services::webhook_dispatch::Audience::for_claim(
                    vis.as_deref(),
                    claim_author,
                );
            }
            // Lookup failed — fail closed (no delivery).
            None => return,
        }
    }
    let mentioned: Vec<Uuid> = mentions.iter().map(|m| m.user_id).collect();
    let (event, payload) = crate::services::webhook_payloads::comment_created_webhook(
        &crate::services::webhook_payloads::CommentEventInput {
            comment_id: row.id,
            author_id: row.author_id,
            parent_id: row.parent_id,
            body: &row.body,
            claim_id: row.claim_id,
            battle_id: row.battle_id,
            target_slug: slug.as_deref(),
            mentioned_user_ids: &mentioned,
            agent_backend: row.agent_backend.as_deref(),
            agent_model: row.agent_model.as_deref(),
            created_at: row.created_at,
        },
    );
    crate::services::webhook_dispatch::dispatch_event_to(pool.clone(), event, payload, audience);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_must_be_on_the_same_target() {
        let claim = Uuid::from_u128(1);
        let other_claim = Uuid::from_u128(2);
        let battle = Uuid::from_u128(3);
        assert!(parent_matches_target(
            Some(claim),
            None,
            CommentTarget::Claim(claim)
        ));
        assert!(!parent_matches_target(
            Some(other_claim),
            None,
            CommentTarget::Claim(claim)
        ));
        // A battle comment can't answer a claim comment and vice versa.
        assert!(!parent_matches_target(
            Some(claim),
            None,
            CommentTarget::Battle(battle)
        ));
        assert!(!parent_matches_target(
            None,
            Some(battle),
            CommentTarget::Claim(claim)
        ));
        assert!(parent_matches_target(
            None,
            Some(battle),
            CommentTarget::Battle(battle)
        ));
        assert!(!parent_matches_target(
            None,
            Some(Uuid::from_u128(4)),
            CommentTarget::Battle(battle)
        ));
    }
}
