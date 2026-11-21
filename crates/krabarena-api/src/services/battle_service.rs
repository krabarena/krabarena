use sqlx::PgPool;
use uuid::Uuid;

use crate::envelope::PaginationMeta;
use crate::error::ApiError;
use crate::models::battle::{
    BATTLE_COLUMNS, BattleListItem, BattleResponse, BattleRow, CreateBattleRequest,
    UpdateBattleRequest,
};
use crate::models::claim::not_superseded;
use crate::pagination::{Cursor, decode_cursor, finalize_page};

/// `true` when `user_id` has the platform `admin` role.
async fn is_admin(pool: &PgPool, user_id: Uuid) -> Result<bool, ApiError> {
    let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(role.as_deref() == Some("admin"))
}

/// Validate + canonicalize an incoming `produced_by` array against the
/// static producer registry. Returns `None` when the field is absent
/// (leave unchanged), or `Some(slugs)` — the normalized, de-duplicated
/// slugs to store (an empty vec explicitly clears the label).
///
/// Attaching producer labels is **admin-only**: any caller that supplies
/// the field (set or clear) must hold the `admin` role, otherwise 403.
/// Unknown slugs are rejected with a 400 listing the valid producers.
async fn resolve_produced_by(
    pool: &PgPool,
    actor: Uuid,
    input: Option<&[String]>,
) -> Result<Option<Vec<String>>, ApiError> {
    let Some(raw) = input else {
        return Ok(None);
    };
    if !is_admin(pool, actor).await? {
        return Err(ApiError::Forbidden);
    }
    let mut slugs: Vec<String> = Vec::new();
    for s in raw {
        let Some(slug) = crate::models::producer::normalize(s) else {
            continue;
        };
        if !crate::models::producer::is_valid(&slug) {
            return Err(ApiError::ValidationError(format!(
                "unknown producer '{}'. valid producers: {}",
                s.trim(),
                crate::models::producer::valid_slugs().join(", ")
            )));
        }
        if !slugs.contains(&slug) {
            slugs.push(slug);
        }
    }
    Ok(Some(slugs))
}

pub async fn create_battle(
    pool: &PgPool,
    author_id: Uuid,
    req: CreateBattleRequest,
) -> Result<BattleResponse, ApiError> {
    use krabarena_shared::field_limits::Field;
    Field::BattlesTitle.check(&req.title)?;
    Field::BattlesDescription.check(&req.description)?;
    if req.domain_tags.is_empty() {
        return Err(ApiError::ValidationError(
            "at least one domain tag required".into(),
        ));
    }

    // Round-trip-validate proxy_spec at write time so a malformed
    // shape can't land in the DB and then fail on every read. The
    // canonical schema is documented at `docs/proxy-spec.md`.
    if let Some(ref raw) = req.proxy_spec {
        crate::models::proxy::ProxySpec::validate(raw).map_err(ApiError::ValidationError)?;
    }

    let produced_by = resolve_produced_by(pool, author_id, req.produced_by.as_deref()).await?;

    let slug = {
        let natural = crate::models::battle::slugify(&req.title);
        crate::models::battle::ensure_unique_slug(pool, "battles", &natural)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    };

    let row = sqlx::query_as::<_, BattleRow>(
        r#"
        INSERT INTO battles (author_id, parent_id, title, description, domain_tags, proxy_spec, slug, produced_by)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING *
        "#,
    )
    .bind(author_id)
    .bind(req.parent_id)
    .bind(&req.title)
    .bind(&req.description)
    .bind(&req.domain_tags)
    .bind(&req.proxy_spec)
    .bind(&slug)
    .bind(&produced_by)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    // Initialize proxy budget if spec is provided
    if req.proxy_spec.is_some() {
        let _ =
            sqlx::query("INSERT INTO proxy_budgets (battle_id) VALUES ($1) ON CONFLICT DO NOTHING")
                .bind(row.id)
                .execute(pool)
                .await;
    }

    tracing::info!(
        battle_id = %row.id,
        author_id = %author_id,
        tags = ?req.domain_tags,
        has_proxy = req.proxy_spec.is_some(),
        "battle.created"
    );

    // Fire `battle.created` (since 0.43.0). `create_battle` is the
    // single Battle insert path — both the editor route
    // (`POST /v1/battles`) and the admin route (`POST /v1/admin/battles`)
    // call it — so every new Battle dispatches exactly once. The admin
    // route may flip `status` right after; the payload carries the
    // status as inserted. Fire-and-forget like `claim.posted`.
    let (event, payload) = crate::services::webhook_payloads::battle_created_webhook(
        &crate::services::webhook_payloads::BattleEventInput {
            battle_id: row.id,
            slug: row.slug.as_deref(),
            title: &row.title,
            author_id: row.author_id,
            parent_id: row.parent_id,
            status: &row.status,
            domain_tags: &row.domain_tags,
            created_at: row.created_at,
        },
    );
    crate::services::webhook_dispatch::dispatch_event(pool.clone(), event, payload);

    Ok(BattleResponse {
        id: row.id,
        author_id: row.author_id,
        parent_id: row.parent_id,
        title: row.title,
        description: row.description,
        domain_tags: row.domain_tags,
        status: row.status,
        claim_count: 0,
        total_confirmed: 0,
        total_refuted: 0,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

pub async fn update_battle(
    pool: &PgPool,
    battle_id: Uuid,
    user_id: Uuid,
    req: UpdateBattleRequest,
) -> Result<BattleResponse, ApiError> {
    // Fetch existing battle
    let sql = format!("SELECT {BATTLE_COLUMNS} FROM battles WHERE id = $1");
    let row = sqlx::query_as::<_, BattleRow>(&sql)
        .bind(battle_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::BattleNotFound)?;

    // Permission: author can always edit their own Battle; editors
    // and admins can edit any Battle. Other users get 403. The
    // editor/admin carve-out matters when a Battle was opened via a
    // DNS-auth service principal on the entity (e.g. `keenable.ai`
    // created with `auth_method = dns`) but a human admin in the
    // same entity needs to update it — without this the entire
    // entity is locked out of editing their own content.
    if row.author_id != user_id {
        let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        if !matches!(role.as_deref(), Some("admin") | Some("editor")) {
            return Err(ApiError::Forbidden);
        }
    }

    let title = req.title.as_deref().unwrap_or(&row.title);
    let description = req.description.as_deref().unwrap_or(&row.description);
    let tags = req.domain_tags.as_ref().unwrap_or(&row.domain_tags);
    let proxy_spec = req.proxy_spec.as_ref().or(row.proxy_spec.as_ref());
    // Three-way: a provided array is validated + admin-checked (an empty
    // array explicitly clears the label); an absent field leaves the
    // producers unchanged.
    let produced_by: Option<Vec<String>> = match req.produced_by.as_deref() {
        Some(raw) => resolve_produced_by(pool, user_id, Some(raw)).await?,
        None => row.produced_by.clone(),
    };

    use krabarena_shared::field_limits::Field;
    Field::BattlesTitle.check(title)?;
    Field::BattlesDescription.check(description)?;
    if tags.is_empty() {
        return Err(ApiError::ValidationError(
            "at least one domain tag required".into(),
        ));
    }
    // Mirror the create path: only validate proxy_spec when the
    // caller provided one. Skipping the check when they didn't
    // touch it means a previously-stored-but-now-invalid spec (from
    // before validation was wired up) doesn't lock the Battle out
    // of all other edits — authors can still update title /
    // description / status while we wait for them to supply a valid
    // proxy_spec too. Canonical schema: `docs/proxy-spec.md`.
    if let Some(ref raw) = req.proxy_spec {
        crate::models::proxy::ProxySpec::validate(raw).map_err(ApiError::ValidationError)?;
    }

    // Only validate status when the caller actually changed it.
    // Re-validating `row.status` would reject records the *system*
    // put into valid-but-system-only states (e.g. `active`, which
    // the Claim-landing hook transitions to automatically). The DB
    // has the authoritative CHECK constraint at
    // `battles_status_check = open|active|settled`; we mirror that
    // app-side for a clean 400 on bad user input, but never
    // "re-check" an untouched state. Legacy `contested` / `holding`
    // values from older API clients are translated to `active`.
    let status: String = match req.status.as_deref() {
        Some(provided) => match krabarena_shared::domain::BattleStatus::parse_lenient(provided) {
            Some(s) => s.to_string(),
            None => {
                return Err(ApiError::ValidationError(format!(
                    "status must be one of: open, active, settled (got '{provided}')"
                )));
            }
        },
        None => row.status.clone(),
    };

    let updated = sqlx::query_as::<_, BattleRow>(
        r#"
        UPDATE battles
        SET title = $1,
            description = $2,
            domain_tags = $3,
            status = $4,
            proxy_spec = $5,
            produced_by = $6,
            updated_at = NOW()
        WHERE id = $7
        RETURNING *
        "#,
    )
    .bind(title)
    .bind(description)
    .bind(tags)
    .bind(&status)
    .bind(proxy_spec)
    .bind(&produced_by)
    .bind(battle_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(battle_id = %battle_id, author_id = %user_id, "battle.updated");

    // Public battle totals count lineage HEADs only — a superseded
    // (edited-away) claim version stops counting toward the battle's
    // claim/verify/refute totals, matching the lists.
    let superseded = not_superseded("s");
    let stats = sqlx::query_as::<_, (i64, i64, i64)>(&format!(
        r#"
        SELECT
            COUNT(s.id) as claim_count,
            COALESCE(SUM(s.verify_count), 0) as total_confirmed,
            COALESCE(SUM(s.refute_count), 0) as total_refuted
        FROM claims s
        WHERE s.battle_id = $1 AND s.deleted_at IS NULL {superseded}
        "#
    ))
    .bind(battle_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(BattleResponse {
        id: updated.id,
        author_id: updated.author_id,
        parent_id: updated.parent_id,
        title: updated.title,
        description: updated.description,
        domain_tags: updated.domain_tags,
        status: updated.status,
        claim_count: stats.0,
        total_confirmed: stats.1,
        total_refuted: stats.2,
        created_at: updated.created_at,
        updated_at: updated.updated_at,
    })
}

pub async fn get_battle(pool: &PgPool, battle_id: Uuid) -> Result<BattleResponse, ApiError> {
    // `deleted_at IS NULL` — a soft-deleted battle 404s publicly
    // (migration 053). Admins restore via the admin endpoints.
    let sql = format!("SELECT {BATTLE_COLUMNS} FROM battles WHERE id = $1 AND deleted_at IS NULL");
    let row = sqlx::query_as::<_, BattleRow>(&sql)
        .bind(battle_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::BattleNotFound)?;

    // Soft-deleted claims don't count toward the public verify/refute/
    // claim totals shown on the battle. Superseded (edited-away) claim
    // versions don't count either — only the lineage HEAD does.
    let superseded = not_superseded("s");
    let stats = sqlx::query_as::<_, (i64, i64, i64)>(&format!(
        r#"
        SELECT
            COUNT(s.id) as claim_count,
            COALESCE(SUM(s.verify_count), 0) as total_confirmed,
            COALESCE(SUM(s.refute_count), 0) as total_refuted
        FROM claims s
        WHERE s.battle_id = $1 AND s.deleted_at IS NULL {superseded}
        "#
    ))
    .bind(battle_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(BattleResponse {
        id: row.id,
        author_id: row.author_id,
        parent_id: row.parent_id,
        title: row.title,
        description: row.description,
        domain_tags: row.domain_tags,
        status: row.status,
        claim_count: stats.0,
        total_confirmed: stats.1,
        total_refuted: stats.2,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

pub async fn list_battles(
    pool: &PgPool,
    status: Option<&str>,
    domain: Option<&str>,
    cursor: Option<&str>,
    limit: i64,
) -> Result<(Vec<BattleListItem>, PaginationMeta), ApiError> {
    let limit = limit.clamp(1, 100);
    let fetch_limit = limit + 1;
    let cursor = decode_cursor(cursor);

    // Explicit BATTLE_COLUMNS instead of `SELECT *` — see AGENTS.md
    // → "ALTER TABLE on prod" for the PgBouncer cached-plan trap.
    // Keyset `(updated_at, id) < (cursor.ts, cursor.id)` — the `id`
    // tie-break keeps rows sharing an `updated_at` from straddling a
    // page boundary. See `crate::pagination`.
    let sql = format!(
        "SELECT {BATTLE_COLUMNS} FROM battles \
         WHERE deleted_at IS NULL \
           AND ($1::text IS NULL OR status = $1) \
           AND ($2::text IS NULL OR $2 = ANY(domain_tags)) \
           AND ($3::timestamptz IS NULL OR updated_at < $3 \
                OR (updated_at = $3 AND id < $4)) \
         ORDER BY updated_at DESC, id DESC \
         LIMIT $5"
    );
    let rows = sqlx::query_as::<_, BattleRow>(&sql)
        .bind(status)
        .bind(domain)
        .bind(cursor.map(|c| c.ts))
        .bind(cursor.map(|c| c.id))
        .bind(fetch_limit)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let items: Vec<BattleListItem> = rows
        .into_iter()
        .map(|row| BattleListItem {
            id: row.id,
            author_id: row.author_id,
            parent_id: row.parent_id,
            title: row.title,
            domain_tags: row.domain_tags,
            status: row.status,
            claim_count: 0,
            total_confirmed: 0,
            total_refuted: 0,
            updated_at: row.updated_at,
        })
        .collect();

    Ok(finalize_page(items, limit, |b| {
        Cursor::new(b.updated_at, b.id)
    }))
}

pub async fn list_sub_battles(
    pool: &PgPool,
    parent_id: Uuid,
) -> Result<Vec<BattleListItem>, ApiError> {
    let sub_sql = format!(
        "SELECT {BATTLE_COLUMNS} FROM battles WHERE parent_id = $1 \
           AND deleted_at IS NULL \
         ORDER BY updated_at DESC"
    );
    let rows = sqlx::query_as::<_, BattleRow>(&sub_sql)
        .bind(parent_id)
        .fetch_all(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(rows
        .into_iter()
        .map(|row| BattleListItem {
            id: row.id,
            author_id: row.author_id,
            parent_id: row.parent_id,
            title: row.title,
            domain_tags: row.domain_tags,
            status: row.status,
            claim_count: 0,
            total_confirmed: 0,
            total_refuted: 0,
            updated_at: row.updated_at,
        })
        .collect())
}

/// Re-derive the lifecycle status of a Battle from its claims.
///
/// The lifecycle is intentionally minimal — `open` (no claims yet)
/// vs `active` (has claims) vs `settled` (closed by an editor,
/// terminal). The "is this debate heated" signal lives on the
/// `temperature` column, computed separately from verify/refute
/// activity. Don't conflate the two: a one-claim battle with no
/// pushback should still read as `active`, just with low
/// temperature.
pub async fn recalculate_hole_status(pool: &PgPool, battle_id: Uuid) -> Result<(), ApiError> {
    let claim_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM claims WHERE battle_id = $1 AND deleted_at IS NULL",
    )
    .bind(battle_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let current_status =
        sqlx::query_scalar::<_, String>("SELECT status FROM battles WHERE id = $1")
            .bind(battle_id)
            .fetch_one(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

    // Settled is terminal; never auto-transition out of it.
    if current_status == "settled" {
        return Ok(());
    }

    let new_status = if claim_count > 0 { "active" } else { "open" };
    if new_status == current_status {
        return Ok(());
    }

    sqlx::query("UPDATE battles SET status = $1, updated_at = NOW() WHERE id = $2")
        .bind(new_status)
        .bind(battle_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(())
}
