use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::envelope::PaginationMeta;
use crate::error::ApiError;
use crate::models::claim::{CLAIM_COLUMNS, ClaimResponse, ClaimRow, not_superseded};
use crate::pagination::{Cursor, RankCursor, decode_cursor, decode_rank_cursor, finalize_page};

pub struct CreateClaimParams<'a> {
    /// `None` for standalone claims (no Battle yet). The standalone
    /// path enforces `topic` + `topic_tags` and overrides
    /// `visibility` to `'private'` so the orphan-claim space can't
    /// be spammed.
    pub battle_id: Option<Uuid>,
    pub author_id: Uuid,
    pub claim: &'a str,
    pub artifact_hash: &'a str,
    pub artifact_url: &'a str,
    pub artifact_size: i64,
    pub artifact_filename: Option<&'a str>,
    pub predecessor_id: Option<Uuid>,
    pub agent_backend: Option<&'a str>,
    pub agent_model: Option<&'a str>,
    pub review_id: Option<Uuid>,
    pub warnings_ignored: bool,
    pub artifact_summary: Option<&'a str>,
    pub artifact_highlights: Option<&'a serde_json::Value>,
    /// Structured benchmark + leaderboard JSON. May be `None` for
    /// legacy claims or claims whose KrabReviewer pass couldn't
    /// extract a leaderboard (the review verdict on the row records
    /// the warn so the UI can surface it).
    pub claim_structure: Option<&'a serde_json::Value>,
    /// Denormalized winners list. Pulled from `claim_structure.winners`
    /// at insert time so leaderboard queries don't have to scan JSONB.
    /// Empty when `claim_structure` is `None`.
    pub winners: &'a [String],
    /// Author-reported token consumption pulled from
    /// `structure.json.cost.tokens`. INSERT only fills the column
    /// when the value is `Some` AND no PoW-metadata caller has set
    /// it earlier on the same request — we never overwrite an
    /// explicit value already on the row.
    pub tokens_consumed: Option<i32>,
    /// Author-reported dollar cost (rounded to cents) pulled from
    /// `structure.json.cost.usd`. Same overwrite policy as
    /// `tokens_consumed`.
    pub price_cents: Option<i32>,
    /// 768-dim Gemini text embedding of the claim text. Stored on
    /// the row so future posts can dedup semantically without
    /// re-embedding old claims. Optional because the embedding call
    /// can fail and we'd rather still store the claim than reject
    /// the post.
    pub claim_embedding: Option<&'a [f32]>,
    /// Standalone-claim topic (added migration 035). Required on
    /// the standalone path (`battle_id = None`), free-form text on
    /// the Battle-attached path.
    pub topic: Option<&'a str>,
    /// Topic tags (added migration 035). Empty on Battle-attached
    /// claims; the standalone path validates at least one entry.
    pub topic_tags: &'a [String],
    /// Visibility on insert. `'public'` for Battle-attached claims
    /// (default), `'private'` for standalone (overridden by the
    /// route handler). Adoption flips this to `'adopted'`.
    pub visibility: &'a str,
    /// Reviewer-generated social-card summary (migration 067).
    /// Best-effort — `None` for legacy paths and any caller that
    /// didn't run this claim through the KrabReviewer (admin/test
    /// fixtures).
    pub og_summary: Option<&'a str>,
    /// Reviewer-generated viral share layer object, serialized
    /// (migration 067). Best-effort, same `None` cases as
    /// `og_summary`.
    pub presentation: Option<&'a serde_json::Value>,
}

pub async fn create_claim(
    pool: &PgPool,
    params: CreateClaimParams<'_>,
) -> Result<ClaimResponse, ApiError> {
    let CreateClaimParams {
        battle_id,
        author_id,
        claim,
        artifact_hash,
        artifact_url,
        artifact_size,
        predecessor_id,
        agent_backend,
        agent_model,
        review_id,
        warnings_ignored,
        artifact_filename,
        artifact_summary,
        artifact_highlights,
        claim_structure,
        winners,
        tokens_consumed,
        price_cents,
        claim_embedding,
        topic,
        topic_tags,
        visibility,
        og_summary,
        presentation,
    } = params;

    // Generate slug from first line of claim text, then disambiguate
    // against already-taken slugs so a repeat title doesn't collide on
    // the `claims_slug_key` unique index and trip a 500.
    let slug = {
        let title_line = claim
            .lines()
            .find(|l| l.trim().starts_with("# "))
            .map(|l| l.trim().trim_start_matches("# "))
            .unwrap_or(claim.lines().next().unwrap_or(claim));
        let natural = crate::models::battle::slugify(title_line);
        crate::models::battle::ensure_unique_slug(pool, "claims", &natural)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    };
    if claim.is_empty() || claim.len() > 5000 {
        return Err(ApiError::ValidationError(
            "claim must be 1-5000 characters".into(),
        ));
    }

    // Verify battle exists (skipped for standalone claims where
    // `battle_id` is None — they ARE the "no battle" case).
    if let Some(bid) = battle_id {
        let battle_exists =
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM battles WHERE id = $1)")
                .bind(bid)
                .fetch_one(pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;

        if !battle_exists {
            return Err(ApiError::BattleNotFound);
        }
    }

    // INSERT ... RETURNING * can commit the row in Postgres but then
    // fail on the sqlx side when decoding into ClaimRow (e.g. a Neon
    // cached prepared-plan drift after a migration, or a schema
    // change that hasn't propagated to the struct yet). Without a
    // recovery path, the client gets a 500 while the Claim exists in
    // the DB — so on retry the user hits the duplicate detector on
    // their own successful post. Confusing.
    //
    // Recovery: if the INSERT errors, check whether a row with the
    // same (battle_id, author_id, artifact_hash) was in fact
    // committed just now (within the last minute). If so, return it
    // as success. The SELECT uses a fresh prepared statement so it
    // doesn't share the potentially-stale plan.
    let topic_tags_vec: Vec<String> = topic_tags.to_vec();
    let row = match sqlx::query_as::<_, ClaimRow>(
        r#"
        INSERT INTO claims (
            battle_id, author_id, claim, artifact_hash, artifact_url, artifact_size,
            predecessor_id, agent_backend, agent_model,
            review_id, warnings_ignored, artifact_summary, artifact_highlights,
            slug, artifact_filename,
            claim_structure, winners,
            tokens_consumed, price_cents,
            claim_embedding,
            topic, topic_tags, visibility,
            og_summary, presentation
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25)
        RETURNING *
        "#,
    )
    .bind(battle_id)
    .bind(author_id)
    .bind(claim)
    .bind(artifact_hash)
    .bind(artifact_url)
    .bind(artifact_size)
    .bind(predecessor_id)
    .bind(agent_backend)
    .bind(agent_model)
    .bind(review_id)
    .bind(warnings_ignored)
    .bind(artifact_summary)
    .bind(artifact_highlights)
    .bind(&slug)
    .bind(artifact_filename)
    .bind(claim_structure)
    .bind(winners)
    .bind(tokens_consumed)
    .bind(price_cents)
    .bind(claim_embedding.map(|v| pgvector::Vector::from(v.to_vec())))
    .bind(topic)
    .bind(&topic_tags_vec)
    .bind(visibility)
    .bind(og_summary)
    .bind(presentation)
    .fetch_one(pool)
    .await
    {
        Ok(row) => row,
        Err(insert_err) => {
            // Recovery SELECT must handle both Battle-attached
            // (`battle_id IS NOT DISTINCT FROM $1`) and standalone
            // (battle_id IS NULL) cases. Using `IS NOT DISTINCT FROM`
            // makes NULL = NULL the way SQL's `=` does not.
            let recovered = sqlx::query_as::<_, ClaimRow>(
                "SELECT * FROM claims \
                 WHERE battle_id IS NOT DISTINCT FROM $1 \
                   AND author_id = $2 AND artifact_hash = $3 \
                   AND created_at > NOW() - INTERVAL '1 minute' \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(battle_id)
            .bind(author_id)
            .bind(artifact_hash)
            .fetch_optional(pool)
            .await;
            match recovered {
                Ok(Some(row)) => {
                    tracing::warn!(
                        claim_id = %row.id,
                        error = %insert_err,
                        "INSERT RETURNING failed but row was committed — recovering as success"
                    );
                    row
                }
                _ => return Err(ApiError::Internal(insert_err.into())),
            }
        }
    };

    tracing::info!(
        claim_id = %row.id,
        battle_id = ?battle_id,
        artifact_hash = %artifact_hash,
        author_id = %author_id,
        "claim.created"
    );

    // Instant emails (since 0.43.0): `battle_claim` to the Battle
    // author, `claim_review` (KrabReviewer verdict) to the Claim author.
    // Both REST post paths (Battle-attached + standalone) insert here.
    for event in crate::services::notification_email::claim_created_events(
        row.id,
        row.battle_id,
        row.visibility.as_deref(),
        row.review_id,
    ) {
        crate::services::notification_email::emit(pool, event);
    }

    let review_id = row.review_id;
    let mut resp = ClaimResponse::from(row);
    populate_review_cost(pool, review_id, &mut resp).await;

    Ok(resp)
}

/// Top-K semantic neighbours for `embedding` among ALL claims on
/// `battle_id` that have an embedding stored. Returns rows in
/// descending similarity order (most similar first).
///
/// Each row carries enough context for the Gemini dedup judge to
/// reason about whether a borderline match is a true duplicate, an
/// improvement of one, or a genuinely new claim:
///   - `(id, slug)` — for linking back from the response
///   - `claim` — full claim text; the judge reads the title from
///     the leading `# ` line and the body for the substantive
///     comparison
///   - `similarity` — cosine sim, 1.0 = identical, 0.0 = orthogonal
///
/// Uses pgvector's `<=>` cosine-distance operator server-side, so no
/// candidate vectors cross the wire. Per-battle scope (no author /
/// time filter) — duplicate-detection should catch a copy from any
/// agent, not just the same author's last 24h.
pub async fn nearest_neighbors_topk(
    pool: &PgPool,
    battle_id: Option<Uuid>,
    embedding: &[f32],
    k: i64,
    exclude_id: Option<Uuid>,
) -> Result<Vec<DedupCandidate>, ApiError> {
    let v = pgvector::Vector::from(embedding.to_vec());
    // For standalone claims (battle_id None) the dedup horizon is the
    // OTHER standalone claims; for Battle-attached posts it's claims
    // on the same Battle. `IS NOT DISTINCT FROM` evaluates NULL = NULL
    // as TRUE, so a None bind matches only the standalone rows.
    //
    // `exclude_id` drops one claim from the horizon — set it to the
    // forced predecessor on an `edit`/`--predecessor` post so a
    // revision is never flagged a duplicate of the very claim it
    // supersedes. `id IS DISTINCT FROM $4` is a no-op when the bind is
    // NULL (id is never NULL), so a normal post excludes nothing.
    let rows: Vec<(Uuid, Option<String>, String, Uuid, f64)> = sqlx::query_as(
        "SELECT id, slug, claim, author_id, \
                1 - (claim_embedding <=> $1) AS similarity \
         FROM claims \
         WHERE battle_id IS NOT DISTINCT FROM $2 \
           AND claim_embedding IS NOT NULL \
           AND id IS DISTINCT FROM $4 \
         ORDER BY claim_embedding <=> $1 ASC \
         LIMIT $3",
    )
    .bind(v)
    .bind(battle_id)
    .bind(k)
    .bind(exclude_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(rows
        .into_iter()
        .map(|(id, slug, claim, author_id, sim)| DedupCandidate {
            id,
            slug,
            claim,
            author_id,
            similarity: sim as f32,
        })
        .collect())
}

/// One candidate row from `nearest_neighbors_topk`. The shape is
/// dedup-handler-specific — keep it private to this module's
/// callers (claim post handler) until a second consumer appears.
#[derive(Debug, Clone)]
pub struct DedupCandidate {
    pub id: Uuid,
    pub slug: Option<String>,
    pub claim: String,
    pub author_id: Uuid,
    pub similarity: f32,
}

pub async fn get_claim(pool: &PgPool, claim_id: Uuid) -> Result<ClaimResponse, ApiError> {
    // `deleted_at IS NULL` — a soft-deleted claim 404s publicly
    // (migration 053). Admins restore via the admin endpoints.
    let sql = format!("SELECT {CLAIM_COLUMNS} FROM claims WHERE id = $1 AND deleted_at IS NULL");
    let row = sqlx::query_as::<_, ClaimRow>(&sql)
        .bind(claim_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::ClaimNotFound)?;

    let review_id = row.review_id;
    let mut resp = ClaimResponse::from(row);
    resp.author = load_claim_author(pool, resp.author_id).await.ok().flatten();
    populate_review_cost(pool, review_id, &mut resp).await;
    Ok(resp)
}

/// Look up the `claim_reviews` row keyed by `review_id` and write the
/// total tokens (prompt + completion) and dollar cost onto the
/// response. Best-effort — a missing row, NULL `review_id`, or DB
/// error leaves the fields `None` so we never 500 the read because
/// the cost lookup hiccupped.
pub(crate) async fn populate_review_cost(
    pool: &PgPool,
    review_id: Option<Uuid>,
    resp: &mut ClaimResponse,
) {
    let Some(review_id) = review_id else {
        return;
    };
    // `cost_micros` is BIGINT (microUSD); the token columns are INTEGER
    // and may be NULL on legacy rows written before migration 022.
    let row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
        "SELECT \
           (COALESCE(prompt_tokens, 0) + COALESCE(completion_tokens, 0))::bigint, \
           cost_micros \
         FROM claim_reviews \
         WHERE id = $1",
    )
    .bind(review_id)
    .fetch_optional(pool)
    .await;
    let Ok(Some((tokens, cost_micros))) = row else {
        return;
    };
    if let Some(t) = tokens {
        // Clamp to i32::MAX so an absurd reviewer transcript can't wrap.
        let clamped: i32 = t.clamp(0, i32::MAX as i64) as i32;
        // Suppress the field when the row genuinely recorded zero
        // tokens (legacy review pre-v0.8.2): the column is NOT NULL
        // after the COALESCE, so `Some(0)` would still print "0
        // tokens · ..." which is misleading. Treat 0 as absent.
        if clamped > 0 {
            resp.review_tokens_total = Some(clamped);
        }
    }
    if let Some(m) = cost_micros {
        resp.review_cost_usd = Some(m as f64 / 1_000_000.0);
    }
}

/// Join users + entities + activity counts for a claim's author.
/// Returns `Ok(None)` if the user row is gone (FK should prevent this,
/// but we don't want a stale record to 500 the fetch).
pub async fn load_claim_author(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Option<crate::models::claim::ClaimAuthor>, ApiError> {
    Ok(load_claim_authors(pool, &[user_id]).await?.remove(&user_id))
}

pub async fn load_claim_authors(
    pool: &PgPool,
    user_ids: &[Uuid],
) -> Result<HashMap<Uuid, crate::models::claim::ClaimAuthor>, ApiError> {
    let mut ids: Vec<Uuid> = user_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let superseded = not_superseded("c");
    let rows = sqlx::query_as::<_, (Uuid, String, String, i64, i64, i64)>(&format!(
        r#"
        SELECT
            u.id,
            u.display_name,
            e.domain,
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'confirmed'),
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'disconfirmed'),
            (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded})
        FROM users u
        JOIN entities e ON u.entity_id = e.id
        WHERE u.id = ANY($1)
        "#
    ))
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(rows
        .into_iter()
        .map(
            |(user_id, display_name, entity_domain, confirmed, refuted, claims)| {
                (
                    user_id,
                    crate::models::claim::ClaimAuthor {
                        user_id,
                        display_name,
                        entity_domain,
                        total_confirmed: confirmed,
                        total_refuted: refuted,
                        total_claims: claims,
                    },
                )
            },
        )
        .collect())
}

pub async fn attach_authors(pool: &PgPool, items: &mut [ClaimResponse]) {
    let ids: Vec<Uuid> = items.iter().map(|c| c.author_id).collect();
    let authors = load_claim_authors(pool, &ids).await.unwrap_or_default();
    for item in items.iter_mut() {
        item.author = authors.get(&item.author_id).cloned();
    }
}

pub async fn list_claims(
    pool: &PgPool,
    battle_id: Uuid,
    sort: Option<&str>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<ClaimResponse>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;

    // Explicit CLAIM_COLUMNS (not `SELECT *`) — cached-plan trap, see
    // AGENTS.md. `deleted_at IS NULL` hides soft-deleted claims from
    // the public battle claim list (migration 053).
    // `not_superseded("c")` hides the old version of an edited claim
    // (the predecessor that `claim edit` superseded) from the public
    // battle list — only the lineage HEAD is listed.
    //
    // The `confirmed`/`refuted` sorts rank by a mutable integer metric,
    // so they keyset on `(count, created_at, id)` via `RankCursor`; the
    // default chronological sort keysets on `(created_at, id)`. Both add
    // `id` (and `created_at`) as deterministic tie-breakers so rows
    // never straddle a page boundary. See `crate::pagination`.
    // Private claims never appear in the public battle list.
    let superseded = format!(
        "{} {}",
        not_superseded("c"),
        crate::services::claim_visibility::public_only_sql("c")
    );

    let (items, pagination) = match sort {
        Some(metric @ ("confirmed" | "refuted")) => {
            let count_col = if metric == "confirmed" {
                "verify_count"
            } else {
                "refute_count"
            };
            let cur = decode_rank_cursor(cursor);
            let query = format!(
                "SELECT {CLAIM_COLUMNS} FROM claims c \
                 WHERE c.battle_id = $1 AND c.deleted_at IS NULL {superseded} \
                   AND ($2::bigint IS NULL \
                        OR {count_col} < $2 \
                        OR ({count_col} = $2 AND c.created_at < $3) \
                        OR ({count_col} = $2 AND c.created_at = $3 AND c.id < $4)) \
                 ORDER BY {count_col} DESC, c.created_at DESC, c.id DESC \
                 LIMIT $5"
            );
            let rows = sqlx::query_as::<_, ClaimRow>(&query)
                .bind(battle_id)
                .bind(cur.map(|c| c.rank))
                .bind(cur.map(|c| c.ts))
                .bind(cur.map(|c| c.id))
                .bind(fetch_limit)
                .fetch_all(pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
            let items: Vec<ClaimResponse> = rows.into_iter().map(ClaimResponse::from).collect();
            let rank_of = |c: &ClaimResponse| {
                let rank = if metric == "confirmed" {
                    c.verify_count
                } else {
                    c.refute_count
                };
                RankCursor::new(rank as i64, c.created_at, c.id)
            };
            finalize_page(items, limit, rank_of)
        }
        _ => {
            let cur = decode_cursor(cursor);
            let query = format!(
                "SELECT {CLAIM_COLUMNS} FROM claims c \
                 WHERE c.battle_id = $1 AND c.deleted_at IS NULL {superseded} \
                   AND ($2::timestamptz IS NULL OR c.created_at < $2 \
                        OR (c.created_at = $2 AND c.id < $3)) \
                 ORDER BY c.created_at DESC, c.id DESC \
                 LIMIT $4"
            );
            let rows = sqlx::query_as::<_, ClaimRow>(&query)
                .bind(battle_id)
                .bind(cur.map(|c| c.ts))
                .bind(cur.map(|c| c.id))
                .bind(fetch_limit)
                .fetch_all(pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
            let items: Vec<ClaimResponse> = rows.into_iter().map(ClaimResponse::from).collect();
            finalize_page(items, limit, |c: &ClaimResponse| {
                Cursor::new(c.created_at, c.id)
            })
        }
    };

    let mut items = items;
    attach_authors(pool, &mut items).await;

    Ok((items, pagination))
}

/// List latest Claims globally, matching the home page's latest-claim
/// activity source but with private-claim protection. Anonymous callers
/// see only public/adopted/legacy rows; authors and admins also see
/// private standalone claims they are allowed to fetch directly.
pub async fn list_latest_claims(
    pool: &PgPool,
    viewer: Option<(Uuid, String)>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<ClaimResponse>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cur = decode_cursor(cursor);
    let viewer_id = viewer.as_ref().map(|(uid, _)| *uid);
    let viewer_is_admin = viewer
        .as_ref()
        .map(|(_, role)| role == "admin")
        .unwrap_or(false);

    // Superseded versions drop out of the global latest feed too —
    // only the lineage HEAD shows up. See `not_superseded`. Keyset on
    // `(created_at, id)` — see `crate::pagination`.
    let superseded = not_superseded("c");
    let sql = format!(
        "SELECT {CLAIM_COLUMNS} FROM claims c \
         WHERE ($1::timestamptz IS NULL OR c.created_at < $1 \
                OR (c.created_at = $1 AND c.id < $2)) \
           AND (c.visibility IS NULL OR c.visibility != 'private' OR c.author_id = $3 OR $4) \
           {superseded} \
         ORDER BY c.created_at DESC, c.id DESC LIMIT $5"
    );
    let rows = sqlx::query_as::<_, ClaimRow>(&sql)
        .bind(cur.map(|c| c.ts))
        .bind(cur.map(|c| c.id))
        .bind(viewer_id)
        .bind(viewer_is_admin)
        .bind(fetch_limit)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let items: Vec<ClaimResponse> = rows.into_iter().map(ClaimResponse::from).collect();
    let (mut items, pagination) = finalize_page(items, limit, |c: &ClaimResponse| {
        Cursor::new(c.created_at, c.id)
    });

    attach_authors(pool, &mut items).await;

    Ok((items, pagination))
}

/// Move a standalone Claim into a Battle. Idempotent when re-adopted
/// into the same Battle; refuses when the Claim is already adopted
/// into a different Battle. Visibility is forced to `'adopted'` so
/// the listing surfaces treat it as a public Battle claim (and not
/// as a candidate for `/claims/standalone`).
pub async fn adopt_claim(
    pool: &PgPool,
    claim_id: Uuid,
    new_battle_id: Uuid,
) -> Result<ClaimResponse, ApiError> {
    // Confirm the destination Battle exists before we touch the
    // Claim row, so callers see `BATTLE_NOT_FOUND` (not a generic
    // 500 on an FK violation) when they pass a bad target.
    let battle_exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM battles WHERE id = $1)")
            .bind(new_battle_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    if !battle_exists {
        return Err(ApiError::BattleNotFound);
    }

    let existing: Option<(Option<Uuid>, String)> =
        sqlx::query_as("SELECT battle_id, visibility FROM claims WHERE id = $1")
            .bind(claim_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let (current_battle_id, _current_visibility) = existing.ok_or(ApiError::ClaimNotFound)?;
    if let Some(bid) = current_battle_id
        && bid != new_battle_id
    {
        return Err(ApiError::ValidationError(format!(
            "claim is already attached to battle {bid}; adoption to a different battle is not permitted — \
             open a new claim or delete the existing one first"
        )));
    }

    // Idempotent — if the claim is already adopted into THIS battle
    // we skip the UPDATE so callers can safely retry.
    if current_battle_id == Some(new_battle_id) {
        return get_claim(pool, claim_id).await;
    }

    sqlx::query(
        "UPDATE claims SET battle_id = $1, visibility = 'adopted' \
         WHERE id = $2",
    )
    .bind(new_battle_id)
    .bind(claim_id)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        claim_id = %claim_id,
        new_battle_id = %new_battle_id,
        "claim.adopted"
    );

    get_claim(pool, claim_id).await
}

/// Flip visibility between `'public'` and `'private'` on a standalone
/// claim. Admin-only at the route layer. Used to promote a private
/// standalone claim into the public `/claims/standalone` list, or to
/// de-list a public one. No-op if the requested state matches the
/// current visibility.
pub async fn set_visibility(
    pool: &PgPool,
    claim_id: Uuid,
    visibility: &str,
) -> Result<ClaimResponse, ApiError> {
    if visibility != "public" && visibility != "private" {
        return Err(ApiError::ValidationError(format!(
            "visibility must be 'public' or 'private' (got '{visibility}'). Use \
             `POST /v1/claims/{{id}}/adopt` to move a claim into a battle."
        )));
    }
    let row: Option<(Option<Uuid>, String)> =
        sqlx::query_as("SELECT battle_id, visibility FROM claims WHERE id = $1")
            .bind(claim_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let (battle_id, current) = row.ok_or(ApiError::ClaimNotFound)?;
    if current == "adopted" || battle_id.is_some() {
        return Err(ApiError::ValidationError(
            "claim is attached to a Battle (visibility='adopted'); admin promote is only allowed \
             on standalone claims"
                .into(),
        ));
    }
    if current == visibility {
        return get_claim(pool, claim_id).await;
    }
    sqlx::query("UPDATE claims SET visibility = $1 WHERE id = $2")
        .bind(visibility)
        .bind(claim_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    tracing::info!(
        claim_id = %claim_id,
        visibility = %visibility,
        "claim.visibility_changed"
    );
    get_claim(pool, claim_id).await
}

/// List public standalone Claims (no Battle yet, visibility='public').
/// Powers `/claims/standalone` plus the JSON variant used by the
/// e2e test. Ordered by `created_at DESC`. Empty-list returns are
/// not an error.
pub async fn list_standalone_claims(
    pool: &PgPool,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<ClaimResponse>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cur = decode_cursor(cursor);
    let superseded = not_superseded("c");
    // Keyset on `(created_at, id)` — see `crate::pagination`.
    let sql = format!(
        "SELECT {CLAIM_COLUMNS} FROM claims c \
         WHERE c.battle_id IS NULL AND c.visibility = 'public' AND c.deleted_at IS NULL \
           {superseded} \
           AND ($1::timestamptz IS NULL OR c.created_at < $1 \
                OR (c.created_at = $1 AND c.id < $2)) \
         ORDER BY c.created_at DESC, c.id DESC LIMIT $3"
    );
    let rows = sqlx::query_as::<_, ClaimRow>(&sql)
        .bind(cur.map(|c| c.ts))
        .bind(cur.map(|c| c.id))
        .bind(fetch_limit)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let items: Vec<ClaimResponse> = rows.into_iter().map(ClaimResponse::from).collect();
    let (mut items, pagination) = finalize_page(items, limit, |c: &ClaimResponse| {
        Cursor::new(c.created_at, c.id)
    });
    attach_authors(pool, &mut items).await;
    Ok((items, pagination))
}

pub async fn get_lineage(pool: &PgPool, claim_id: Uuid) -> Result<Vec<ClaimResponse>, ApiError> {
    let mut lineage = Vec::new();
    let mut current_id = Some(claim_id);

    let sql = format!("SELECT {CLAIM_COLUMNS} FROM claims WHERE id = $1 AND deleted_at IS NULL");
    while let Some(id) = current_id {
        let row = sqlx::query_as::<_, ClaimRow>(&sql)
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

        match row {
            Some(claim) => {
                current_id = claim.predecessor_id;
                lineage.push(ClaimResponse::from(claim));
            }
            None => break,
        }
    }

    Ok(lineage)
}
