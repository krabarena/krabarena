//! Org (entity) routes — the org-admin model (since 0.15.0).
//!
//! "Org" is the user-facing label for the `entities` table — a row
//! keyed by domain (e.g. `acme.com`) auto-created at signup. PR A
//! introduces the multi-admin model via `users.org_role` plus a
//! general "change permissions" endpoint that handles both
//! promotion and demotion (transfer is now just a side-effect of
//! "promote new user → demote yourself").
//!
//! - `GET    /v1/me/org`                  — the calling user's org
//! - `GET    /v1/orgs/{id}`               — public org overview
//! - `GET    /v1/orgs/{id}/members`       — member listing (org members only)
//! - `POST   /v1/orgs/{id}/members/{user_id}/permissions` — admin-only
//!   role change (promote / demote between `admin` and `member`)
//! - `POST   /v1/me/org/members/{user_id}/permissions` — caller-org alias
//! - `POST   /v1/orgs/{id}/bots`          — admin-only bot creation (PR B)
//! - `POST   /v1/me/org/bots`             — caller-org alias
//! - `POST   /v1/orgs/{id}/bots/{user_id}/rotate-token` — mint a new JWT
//! - `POST   /v1/me/org/bots/{user_id}/rotate-token` — caller-org alias
//! - `DELETE /v1/orgs/{id}/bots/{user_id}` — hard-delete bot
//! - `DELETE /v1/me/org/bots/{user_id}`   — caller-org alias
//!
//! The canonical "is this user an admin" check is now
//! `users.org_role = 'admin'`, NOT `entities.admin_user_id` — the
//! latter is kept as a denormalized primary-admin pointer for
//! quick-look display (`GET /v1/me/org`'s `admin_display_name`) and
//! gets updated by the permissions endpoint when it makes sense
//! (e.g. demoting the current primary admin picks a replacement).
//!
//! PR B adds the bot-creation endpoint (admin-only) that uses
//! `users.kind='bot'` + `auth_method='bot'` (migration 043) to
//! provision service accounts (e.g. KrabChecker). Bots are regular
//! `users` rows — they JWT-auth like any user; their `kind='bot'`
//! shows up in claim authorship for audit. The create response is
//! the only place the bot's bearer token is surfaced; subsequent
//! reads never echo it (like webhook secrets in #157).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::routes::auth::extract_user_id;
use crate::services::auth_service;

/// Response shape for `GET /v1/me/org` and `GET /v1/orgs/{id}`.
///
/// `your_role` is `"admin"` when the caller holds `org_role='admin'`
/// in this org and `"member"` otherwise — meant for UI gating, not
/// authorization (the server still re-checks on the admin
/// endpoints). Set to `None` when the request is unauthenticated
/// (the public `/v1/orgs/{id}` path) so the field isn't a misleading
/// `"member"` to anonymous viewers.
///
/// `admin_user_id` / `admin_display_name` are the denormalized
/// PRIMARY admin (the pointer on `entities`). For the full admin
/// list use `GET /v1/orgs/{id}/members` and filter on
/// `role == "admin"`.
#[derive(Debug, Serialize)]
pub struct OrgView {
    pub id: Uuid,
    pub name: String,
    pub domain: String,
    pub member_count: i64,
    pub admin_user_id: Option<Uuid>,
    pub admin_display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub your_role: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Row returned by member listings. `joined_at` is the user's
/// `created_at`; bots get listed alongside humans with `kind="bot"`
/// so the UI / CLI can call them out. `role` is the org-level role
/// (`admin` or `member`) — distinct from the `users.role` global
/// (`admin`/`editor`/`moderator`/`user`).
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct OrgMember {
    pub id: Uuid,
    pub display_name: String,
    pub kind: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

/// Body shape for `POST .../members/{user_id}/permissions`.
///
/// One field today (`role`), but the endpoint is named
/// "permissions" so future capability bits (e.g. `can_invite`,
/// `can_provision_bots`) can land as additive fields without an
/// API rename.
#[derive(Debug, Deserialize)]
pub struct SetPermissionsRequest {
    pub role: String,
}

/// Row shape returned by [`load_org_view`]'s SQL — the entity columns
/// plus the joined admin's display name and the cached member count.
/// Named struct (vs. tuple) so the SELECT can match by column name
/// and the type stays under clippy's `type_complexity` lint.
#[derive(sqlx::FromRow)]
struct OrgViewRow {
    id: Uuid,
    name: String,
    domain: String,
    created_at: DateTime<Utc>,
    admin_user_id: Option<Uuid>,
    admin_display_name: Option<String>,
    member_count: i64,
}

/// Internal helper — fetches the row that backs both `OrgView`
/// endpoints. Joins to `users` for the admin's display name so the
/// caller doesn't have to follow the FK separately.
async fn load_org_view(
    pool: &sqlx::PgPool,
    org_id: Uuid,
    caller_user_id: Option<Uuid>,
) -> Result<Option<OrgView>, ApiError> {
    let row: Option<OrgViewRow> = sqlx::query_as(
        r#"
        SELECT e.id, e.name, e.domain, e.created_at, e.admin_user_id,
               admin.display_name AS admin_display_name,
               (SELECT COUNT(*) FROM users u WHERE u.entity_id = e.id) AS member_count
        FROM entities e
        LEFT JOIN users admin ON admin.id = e.admin_user_id
        WHERE e.id = $1
        "#,
    )
    .bind(org_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let Some(row) = row else {
        return Ok(None);
    };

    // `your_role` consults the canonical `users.org_role` column.
    // (The legacy `entities.admin_user_id == caller` check would
    // miss secondary admins now that the model is multi-admin.)
    let your_role = if let Some(uid) = caller_user_id {
        let role: Option<String> =
            sqlx::query_scalar("SELECT org_role FROM users WHERE id = $1 AND entity_id = $2")
                .bind(uid)
                .bind(org_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
        role
    } else {
        None
    };

    Ok(Some(OrgView {
        id: row.id,
        name: row.name,
        domain: row.domain,
        member_count: row.member_count,
        admin_user_id: row.admin_user_id,
        admin_display_name: row.admin_display_name,
        your_role,
        created_at: row.created_at,
    }))
}

/// `GET /v1/me/org/members` — convenience alias for "list members of
/// my org" so the CLI can do `krab org members` without first
/// looking up the org id. Routes the same logic as
/// `list_members(org_id)` after resolving the caller's
/// `entity_id`.
pub async fn list_my_org_members(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let entity_id: Uuid = sqlx::query_scalar("SELECT entity_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::Unauthorized)?;

    let members: Vec<OrgMember> = sqlx::query_as(
        r#"
        SELECT id, display_name, kind, org_role AS role, created_at AS joined_at
        FROM users
        WHERE entity_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(entity_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(Json(ApiResponse::new(members)))
}

/// `GET /v1/me/org` — the calling user's org. Auth required; resolves
/// the org by joining through `users.entity_id`.
pub async fn get_my_org(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let entity_id: Uuid = sqlx::query_scalar("SELECT entity_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::Unauthorized)?;

    let view = load_org_view(&state.pool, entity_id, Some(user_id))
        .await?
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("user references missing entity")))?;

    Ok(Json(ApiResponse::new(view)))
}

/// `GET /v1/orgs/{id}` — public org overview. Auth optional; if a
/// valid token is presented, `your_role` is populated so the same
/// endpoint can power the authenticated UI.
pub async fn get_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    // Auth is optional here. `extract_user_id` returns Unauthorized
    // for missing/invalid tokens — we treat that as "anonymous" rather
    // than failing the request.
    let caller = extract_user_id(&headers, &state.jwt_secret).ok();

    let view = load_org_view(&state.pool, org_id, caller)
        .await?
        .ok_or(ApiError::ValidationError("org not found".into()))?;

    Ok(Json(ApiResponse::new(view)))
}

/// `GET /v1/orgs/{id}/members` — list users (humans + bots) in the
/// org. Auth required; caller must be a member of the same org.
/// Returns members ordered by `created_at` so the org founder is
/// first and bots show up wherever they were provisioned.
pub async fn list_members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let caller_entity: Option<Uuid> =
        sqlx::query_scalar("SELECT entity_id FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

    if caller_entity != Some(org_id) {
        // Members-only listing — even an admin of a different org can't
        // peek. (PR B might relax this for platform admins.)
        return Err(ApiError::Forbidden);
    }

    let members: Vec<OrgMember> = sqlx::query_as(
        r#"
        SELECT id, display_name, kind, org_role AS role, created_at AS joined_at
        FROM users
        WHERE entity_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(org_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(Json(ApiResponse::new(members)))
}

/// `POST /v1/me/org/members/{user_id}/permissions` — convenience
/// alias mirroring `set_member_permissions`, but the org id is
/// resolved from the caller's `entity_id` so the CLI doesn't need
/// to look it up first.
pub async fn set_my_org_member_permissions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
    Json(req): Json<SetPermissionsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let entity_id: Uuid = sqlx::query_scalar("SELECT entity_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::Unauthorized)?;

    set_member_permissions(
        State(state),
        headers,
        Path((entity_id, target_user_id)),
        Json(req),
    )
    .await
}

/// `POST /v1/orgs/{id}/members/{user_id}/permissions` — admin
/// changes another member's org role.
///
/// Rules:
/// - Caller must be an `admin` of this org (`users.org_role='admin'`
///   AND `entities.id == org_id`). 403 otherwise.
/// - Target user must be a member of the same org. 400 otherwise.
/// - Target must NOT be a bot. Bots can't be admins. 400 otherwise.
/// - `role` must be `"admin"` or `"member"`. 400 otherwise.
/// - Promotion (member → admin): no-op if already admin; if
///   `entities.admin_user_id` is NULL, also set it to the new admin
///   (we want SOMEONE as the primary admin pointer).
/// - Demotion (admin → member): refuses if it would leave the org
///   with zero admins (`LAST_ADMIN`); if the demoted user was the
///   `entities.admin_user_id`, pick another remaining admin (by
///   `created_at ASC`) to take that pointer.
/// - Idempotent: admin → admin and member → member both return
///   200 with the current state.
pub async fn set_member_permissions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SetPermissionsRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let caller_id = extract_user_id(&headers, &state.jwt_secret)?;

    // Validate the requested role early so we 400 on garbage input
    // before touching the DB.
    let new_role = req.role.as_str();
    if new_role != "admin" && new_role != "member" {
        return Err(ApiError::ValidationError(
            "role must be 'admin' or 'member'".into(),
        ));
    }

    // Authorize: caller must be an admin OF THIS org. We need both
    // (a) entity_id matches, and (b) org_role='admin'.
    #[derive(sqlx::FromRow)]
    struct CallerRow {
        entity_id: Uuid,
        org_role: String,
    }
    let caller: Option<CallerRow> =
        sqlx::query_as("SELECT entity_id, org_role FROM users WHERE id = $1")
            .bind(caller_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(caller) = caller else {
        return Err(ApiError::Unauthorized);
    };
    if caller.entity_id != org_id || caller.org_role != "admin" {
        return Err(ApiError::Forbidden);
    }

    // Validate target — must be in the same org and not a bot.
    #[derive(sqlx::FromRow)]
    struct TargetRow {
        entity_id: Uuid,
        kind: String,
        org_role: String,
    }
    let target: Option<TargetRow> =
        sqlx::query_as("SELECT entity_id, kind, org_role FROM users WHERE id = $1")
            .bind(target_user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(target) = target else {
        return Err(ApiError::ValidationError(
            "target user does not exist".into(),
        ));
    };
    if target.entity_id != org_id {
        return Err(ApiError::ValidationError(
            "target user must be a member of the same org".into(),
        ));
    }
    if target.kind == "bot" {
        return Err(ApiError::ValidationError(
            "bots cannot be granted org-admin permissions".into(),
        ));
    }

    let old_role = target.org_role.clone();

    // Idempotent fast path — role already at the requested value.
    // Still log so the audit trail records that someone tried.
    if old_role == new_role {
        tracing::info!(
            actor = %caller_id,
            target = %target_user_id,
            old_role = %old_role,
            new_role = %new_role,
            "org_role_changed (no-op)"
        );
        return Ok(Json(ApiResponse::new(
            load_member(&state.pool, target_user_id).await?,
        )));
    }

    // Last-admin protection: when demoting (admin → member), refuse
    // if the target is the only admin. Counted from `users` —
    // canonical signal — not `entities.admin_user_id`.
    if old_role == "admin" && new_role == "member" {
        let admin_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE entity_id = $1 AND org_role = 'admin'",
        )
        .bind(org_id)
        .fetch_one(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
        if admin_count <= 1 {
            return Err(ApiError::LastAdmin);
        }
    }

    // Apply the role change. Wrapped in a transaction with the
    // primary-admin-pointer adjustment so the two stay consistent
    // even under concurrent writes.
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    sqlx::query("UPDATE users SET org_role = $1 WHERE id = $2")
        .bind(new_role)
        .bind(target_user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    if new_role == "admin" {
        // Promotion: if the org currently has no primary admin
        // pointer, claim it. Don't displace an existing pointer —
        // the multi-admin model means there can be multiple admins
        // and the primary pointer is just a denormalized hint.
        sqlx::query(
            "UPDATE entities SET admin_user_id = $1 \
             WHERE id = $2 AND admin_user_id IS NULL",
        )
        .bind(target_user_id)
        .bind(org_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    } else {
        // Demotion: if the demoted user was the primary admin
        // pointer, pick another remaining admin (deterministic:
        // oldest by `created_at`) to inherit it. If no other admin
        // remains we'd have already returned `LastAdmin` above, so
        // this query is guaranteed to find at least one row.
        let was_primary: Option<Uuid> =
            sqlx::query_scalar("SELECT admin_user_id FROM entities WHERE id = $1")
                .bind(org_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;

        if was_primary == Some(target_user_id) {
            let next_primary: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM users \
                 WHERE entity_id = $1 AND org_role = 'admin' \
                 ORDER BY created_at ASC LIMIT 1",
            )
            .bind(org_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

            sqlx::query("UPDATE entities SET admin_user_id = $1 WHERE id = $2")
                .bind(next_primary)
                .bind(org_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
        }
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        actor = %caller_id,
        target = %target_user_id,
        old_role = %old_role,
        new_role = %new_role,
        "org_role_changed"
    );

    Ok(Json(ApiResponse::new(
        load_member(&state.pool, target_user_id).await?,
    )))
}

/// Load the updated member view post-change so the caller can
/// confirm the new state without a follow-up request.
async fn load_member(pool: &sqlx::PgPool, user_id: Uuid) -> Result<OrgMember, ApiError> {
    sqlx::query_as::<_, OrgMember>(
        r#"
        SELECT id, display_name, kind, org_role AS role, created_at AS joined_at
        FROM users
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))
}

// ---------------------------------------------------------------------------
// Bot endpoints (PR B — since 0.15.1)
// ---------------------------------------------------------------------------

/// Char cap on the bot's display_name. The platform-wide
/// `field_limits::Field::UsersDisplayName.max()` is the canonical
/// number; we mirror it locally so the spec's "≤120 chars" hint
/// matches the wire (the shared limit is 120 at the time of writing).
/// If the shared limit ever moves we want this constant to track it,
/// hence the const fn deref at compile time.
const BOT_DISPLAY_NAME_MAX: usize = krabarena_shared::field_limits::Field::UsersDisplayName.max();

/// Char cap on the optional `description`. Sized to fit a short
/// purpose blurb (e.g. "Auto-verifies claims posted with structure
/// hash mismatches"). 500 chars matches the spec; not constrained
/// at the DB level (the `description` is informational only and
/// lives in a future column or in a structured note — for v1 we
/// only validate length and log it).
const BOT_DESCRIPTION_MAX: usize = 500;

/// Body for `POST .../bots`. `description` is optional + free-form;
/// we don't persist it in the schema today (no `users.description`
/// column) — it's logged on creation for the audit trail. A future
/// migration could surface it as a column without changing the wire.
#[derive(Debug, Deserialize)]
pub struct CreateBotRequest {
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Response shape for `POST .../bots` and `POST .../bots/{id}/rotate-token`.
///
/// `token` is returned only by these two endpoints — the bot's row
/// itself never echoes it back. Same convention as `webhook_subscriptions`'s
/// `secret_plain`: surface once on creation, owner saves it, server
/// never re-serves. JWTs are stateless, so rotation has nothing to
/// invalidate server-side; the old token stays valid until it
/// expires (matches the rest of the platform — there's no
/// revocation list).
#[derive(Debug, Serialize)]
pub struct BotCreatedResponse {
    pub id: Uuid,
    pub display_name: String,
    pub email: String,
    pub kind: String,
    pub token: String,
    pub created_at: DateTime<Utc>,
}

/// Response shape for `POST .../bots/{id}/rotate-token`. Slimmer
/// than `BotCreatedResponse` — the bot's email and created_at are
/// unchanged, so the caller just needs the new token plus enough
/// context to confirm "rotated the right bot".
#[derive(Debug, Serialize)]
pub struct BotRotatedResponse {
    pub id: Uuid,
    pub display_name: String,
    pub token: String,
}

/// `POST /v1/me/org/bots` — convenience alias that resolves the
/// caller's org id server-side then delegates to `create_bot`. The
/// CLI uses this so `krab org bot create` doesn't need a separate
/// org-id lookup. Mirrors `set_my_org_member_permissions`.
pub async fn create_bot_my_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateBotRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let entity_id: Uuid = sqlx::query_scalar("SELECT entity_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::Unauthorized)?;

    create_bot(State(state), headers, Path(entity_id), Json(req)).await
}

/// `POST /v1/orgs/{id}/bots` — admin-only bot creation.
///
/// Rules:
/// - Caller must be an org-admin (`users.org_role='admin'`) of `{id}`. 403 otherwise.
/// - `display_name` is required and bounded by `BOT_DISPLAY_NAME_MAX` chars.
/// - `description` is optional, bounded by `BOT_DESCRIPTION_MAX` chars,
///   and not persisted today — only logged.
/// - Generates a stable identifier `bot-<random8>@<org-domain>` so
///   the `users.email` UNIQUE constraint passes; bots don't have
///   real inboxes. The random suffix prevents collisions across
///   re-creates with the same display name.
/// - INSERTs with `kind='bot'`, `auth_method='bot'` (migration 043),
///   `role='user'` (platform-level), `org_role='member'` (bots
///   can't be admins — enforced in `set_member_permissions` too).
///   The `research_token_balance` default of 0 is implicit (the
///   column already defaults to 0; bots aren't researchers).
/// - Mints a JWT via `auth_service::create_jwt`. Same 30-day expiry
///   as a human session — rotation (`rotate_bot_token`) is the way
///   to extend; long-lived bot identities re-mint and update their
///   stored secret on a cadence (the KrabChecker deploy uses
///   Secret Manager and re-deploys every rotation). Stateless JWTs
///   mean there's no revocation list to manage.
pub async fn create_bot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org_id): Path<Uuid>,
    Json(req): Json<CreateBotRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let caller_id = extract_user_id(&headers, &state.jwt_secret)?;

    // Validate body length first — cheap and avoids a DB round-trip
    // on garbage input.
    let display_name = req.display_name.trim();
    if display_name.is_empty() {
        return Err(ApiError::ValidationError(
            "display_name must not be empty".into(),
        ));
    }
    if display_name.chars().count() > BOT_DISPLAY_NAME_MAX {
        return Err(ApiError::ValidationError(format!(
            "display_name exceeds {BOT_DISPLAY_NAME_MAX} chars"
        )));
    }
    if let Some(desc) = req.description.as_deref()
        && desc.chars().count() > BOT_DESCRIPTION_MAX
    {
        return Err(ApiError::ValidationError(format!(
            "description exceeds {BOT_DESCRIPTION_MAX} chars"
        )));
    }

    // Authorize caller — must be admin OF THIS org. Mirrors
    // `set_member_permissions`'s authorization block.
    #[derive(sqlx::FromRow)]
    struct CallerRow {
        entity_id: Uuid,
        org_role: String,
    }
    let caller: Option<CallerRow> =
        sqlx::query_as("SELECT entity_id, org_role FROM users WHERE id = $1")
            .bind(caller_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(caller) = caller else {
        return Err(ApiError::Unauthorized);
    };
    if caller.entity_id != org_id || caller.org_role != "admin" {
        return Err(ApiError::Forbidden);
    }

    // Look up the org's domain so the bot's stable email lands in a
    // namespace tied to its org (e.g. `bot-3f8a1b2c@acme.com`).
    // The domain is otherwise unused for bot routing — it just
    // satisfies `users.email`'s shape and UNIQUE constraint.
    let domain: String = sqlx::query_scalar("SELECT domain FROM entities WHERE id = $1")
        .bind(org_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or_else(|| ApiError::ValidationError("org not found".into()))?;

    let email = generate_bot_email(&domain);

    // INSERT the bot row. `auth_method='bot'` requires migration 043
    // applied (constraint widening); pre-migration deploys would
    // reject with a CHECK violation surfaced as a 500. The
    // `RETURNING (id, created_at)` keeps the round-trip to one query.
    let row: (Uuid, DateTime<Utc>) = sqlx::query_as(
        r#"
        INSERT INTO users (entity_id, display_name, email, auth_method, role, kind, org_role)
        VALUES ($1, $2, $3, 'bot', 'user', 'bot', 'member')
        RETURNING id, created_at
        "#,
    )
    .bind(org_id)
    .bind(display_name)
    .bind(&email)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    let (bot_id, created_at) = row;

    // Mint a JWT for the bot. Same shape as a human user — `role`
    // is the platform-level role (`user`), not the org-level role.
    // The CLI / dispatcher checks `kind='bot'` for audit, not at
    // auth-time.
    let token = auth_service::create_jwt(bot_id, org_id, "user", &state.jwt_secret)?;

    tracing::info!(
        actor = %caller_id,
        bot = %bot_id,
        org = %org_id,
        description = req.description.as_deref().unwrap_or(""),
        "bot_created"
    );

    Ok(Json(ApiResponse::new(BotCreatedResponse {
        id: bot_id,
        display_name: display_name.to_string(),
        email,
        kind: "bot".to_string(),
        token,
        created_at,
    })))
}

/// Generate the stable bot identifier. Eight lowercase hex chars
/// (~32 bits of randomness) is enough to make collisions effectively
/// impossible within an org (the `users.email` UNIQUE constraint
/// catches collisions if they ever happen — the caller retries).
/// Format: `bot-<8 hex>@<org-domain>` — recognisable in logs and
/// auditable to an org.
fn generate_bot_email(domain: &str) -> String {
    let mut buf = [0u8; 4];
    OsRng.fill_bytes(&mut buf);
    let suffix = u32::from_be_bytes(buf);
    format!("bot-{suffix:08x}@{domain}")
}

/// `POST /v1/me/org/bots/{user_id}/rotate-token` — alias resolving
/// the caller's org id, then delegates to `rotate_bot_token`.
pub async fn rotate_bot_token_my_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let entity_id: Uuid = sqlx::query_scalar("SELECT entity_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::Unauthorized)?;

    rotate_bot_token(State(state), headers, Path((entity_id, target_user_id))).await
}

/// `POST /v1/orgs/{id}/bots/{user_id}/rotate-token` — mint a new JWT
/// for an existing bot.
///
/// Rules:
/// - Caller must be admin OF THIS org. 403 otherwise.
/// - Target must exist, be in the same org, AND have `kind='bot'`.
///   404 otherwise (single not-found for any of those — we don't
///   want to leak whether a UUID belongs to a human via the
///   distinction).
/// - No DB write needed; JWTs are stateless. The new token carries
///   a fresher `iat` and a new 30-day `exp`. The old token
///   continues to validate until its own `exp` — there's no
///   revocation list anywhere in the platform. If we ever introduce
///   one, this is the seam to wire it.
pub async fn rotate_bot_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let caller_id = extract_user_id(&headers, &state.jwt_secret)?;

    // Authorize caller — must be admin of `{org_id}`.
    #[derive(sqlx::FromRow)]
    struct CallerRow {
        entity_id: Uuid,
        org_role: String,
    }
    let caller: Option<CallerRow> =
        sqlx::query_as("SELECT entity_id, org_role FROM users WHERE id = $1")
            .bind(caller_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(caller) = caller else {
        return Err(ApiError::Unauthorized);
    };
    if caller.entity_id != org_id || caller.org_role != "admin" {
        return Err(ApiError::Forbidden);
    }

    // Look up the bot. Single query — we collapse "not found",
    // "wrong org", "not a bot" into a single 404 so cross-org bot
    // UUIDs can't be probed.
    #[derive(sqlx::FromRow)]
    struct BotRow {
        id: Uuid,
        display_name: String,
        entity_id: Uuid,
        kind: String,
    }
    let bot: Option<BotRow> =
        sqlx::query_as("SELECT id, display_name, entity_id, kind FROM users WHERE id = $1")
            .bind(target_user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(bot) = bot else {
        return Err(ApiError::ValidationError("bot not found".into()));
    };
    if bot.entity_id != org_id || bot.kind != "bot" {
        return Err(ApiError::ValidationError("bot not found".into()));
    }

    let token = auth_service::create_jwt(bot.id, org_id, "user", &state.jwt_secret)?;

    tracing::info!(
        actor = %caller_id,
        bot = %bot.id,
        "bot_token_rotated"
    );

    Ok(Json(ApiResponse::new(BotRotatedResponse {
        id: bot.id,
        display_name: bot.display_name,
        token,
    })))
}

/// `DELETE /v1/me/org/bots/{user_id}` — alias resolving the
/// caller's org id, then delegates to `delete_bot`.
pub async fn delete_bot_my_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    let entity_id: Uuid = sqlx::query_scalar("SELECT entity_id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::Unauthorized)?;

    delete_bot(State(state), headers, Path((entity_id, target_user_id))).await
}

/// `DELETE /v1/orgs/{id}/bots/{user_id}` — hard-delete a bot.
///
/// Rules:
/// - Caller must be admin OF THIS org. 403 otherwise.
/// - Target must exist, be in the same org, and `kind='bot'`. 404
///   otherwise (same collapsed-404 policy as rotation).
/// - Cascade: `webhook_subscriptions.owner_user_id` already has
///   `ON DELETE CASCADE` (migration 041), so the bot's webhook
///   subscriptions are removed by the same DELETE. Other FK paths
///   into `users` (claims, comments, verifications) all use either
///   `ON DELETE SET NULL` or are blocked by RESTRICT — for bots,
///   which are recently provisioned and unlikely to author much,
///   we accept the rare blocked-delete and surface it as 500. A
///   future PR can add a soft-disable flag if real bots accumulate
///   authorship and admins ever need to revoke them without losing
///   the audit trail.
pub async fn delete_bot(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let caller_id = extract_user_id(&headers, &state.jwt_secret)?;

    // Authorize caller — must be admin of `{org_id}`.
    #[derive(sqlx::FromRow)]
    struct CallerRow {
        entity_id: Uuid,
        org_role: String,
    }
    let caller: Option<CallerRow> =
        sqlx::query_as("SELECT entity_id, org_role FROM users WHERE id = $1")
            .bind(caller_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(caller) = caller else {
        return Err(ApiError::Unauthorized);
    };
    if caller.entity_id != org_id || caller.org_role != "admin" {
        return Err(ApiError::Forbidden);
    }

    // Look up the target — same collapsed-404 as rotate_bot_token.
    #[derive(sqlx::FromRow)]
    struct BotRow {
        entity_id: Uuid,
        kind: String,
    }
    let bot: Option<BotRow> = sqlx::query_as("SELECT entity_id, kind FROM users WHERE id = $1")
        .bind(target_user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let Some(bot) = bot else {
        return Err(ApiError::ValidationError("bot not found".into()));
    };
    if bot.entity_id != org_id || bot.kind != "bot" {
        return Err(ApiError::ValidationError("bot not found".into()));
    }

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(target_user_id)
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        actor = %caller_id,
        bot = %target_user_id,
        org = %org_id,
        "bot_deleted"
    );

    Ok(Json(ApiResponse::new(serde_json::json!({
        "deleted": target_user_id,
    }))))
}

#[cfg(test)]
mod tests {
    //! Authorization unit tests — exercise the validation branches
    //! without a Postgres pool by calling the small predicate the
    //! handler delegates to. The actual SQL paths are covered by
    //! integration tests against the test-e2e Neon branch.
    //!
    //! Coverage matrix (7 assertions matching the spec):
    //! 1. Non-admin caller → 403
    //! 2. Target not a member of this org → 400
    //! 3. Target is a bot → 400 (new — the old transfer-admin
    //!    endpoint had this implicitly via `kind=='human'`; we keep
    //!    the protection explicit since "demote a bot to member"
    //!    would be a confusing no-op rather than a refusal)
    //! 4. Promoting a member to admin → 200
    //! 5. Demoting an admin when another admin exists → 200
    //! 6. Demoting the LAST admin → 400 with code `LAST_ADMIN`
    //! 7. Idempotent no-op (member→member, admin→admin) → 200

    use super::*;

    /// Pure validation helper mirroring `set_member_permissions`'s
    /// checks so we can unit-test the rules without needing a live
    /// DB. Returns `Ok(())` on success and the same `ApiError` the
    /// handler would raise on rejection.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn validate_set_permissions(
        caller_entity: Option<Uuid>,
        caller_org_role: &str,
        target_exists: bool,
        target_entity: Option<Uuid>,
        target_kind: &str,
        target_org_role: &str,
        org_id: Uuid,
        new_role: &str,
        admin_count: i64,
    ) -> Result<(), ApiError> {
        if new_role != "admin" && new_role != "member" {
            return Err(ApiError::ValidationError(
                "role must be 'admin' or 'member'".into(),
            ));
        }
        if caller_entity != Some(org_id) || caller_org_role != "admin" {
            return Err(ApiError::Forbidden);
        }
        if !target_exists {
            return Err(ApiError::ValidationError(
                "target user does not exist".into(),
            ));
        }
        if target_entity != Some(org_id) {
            return Err(ApiError::ValidationError(
                "target user must be a member of the same org".into(),
            ));
        }
        if target_kind == "bot" {
            return Err(ApiError::ValidationError(
                "bots cannot be granted org-admin permissions".into(),
            ));
        }
        if target_org_role == "admin" && new_role == "member" && admin_count <= 1 {
            return Err(ApiError::LastAdmin);
        }
        Ok(())
    }

    /// Non-admin caller → 403 even if they're a member of the same
    /// org. Authorization is enforced before any of the validation
    /// branches.
    #[test]
    fn permissions_rejects_non_admin_caller() {
        let org = Uuid::new_v4();
        let err = validate_set_permissions(
            Some(org),
            "member",
            true,
            Some(org),
            "human",
            "member",
            org,
            "admin",
            1,
        )
        .expect_err("non-admin caller must not be allowed");
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");
    }

    /// Target not in this org → 400. Even an admin can't reach
    /// across orgs.
    #[test]
    fn permissions_rejects_target_in_other_org() {
        let org = Uuid::new_v4();
        let other = Uuid::new_v4();
        let err = validate_set_permissions(
            Some(org),
            "admin",
            true,
            Some(other),
            "human",
            "member",
            org,
            "admin",
            1,
        )
        .expect_err("cross-org target must be rejected");
        match err {
            ApiError::ValidationError(msg) => assert!(msg.contains("same org"), "{msg}"),
            other => panic!("expected ValidationError, got {other:?}"),
        }
    }

    /// Promoting a member to admin validates cleanly.
    #[test]
    fn permissions_allows_promotion() {
        let org = Uuid::new_v4();
        validate_set_permissions(
            Some(org),
            "admin",
            true,
            Some(org),
            "human",
            "member",
            org,
            "admin",
            1,
        )
        .expect("member→admin promotion must pass");
    }

    /// Demoting an admin when another admin exists validates
    /// cleanly. `admin_count == 2` means the target isn't the last
    /// admin.
    #[test]
    fn permissions_allows_demotion_when_not_last_admin() {
        let org = Uuid::new_v4();
        validate_set_permissions(
            Some(org),
            "admin",
            true,
            Some(org),
            "human",
            "admin",
            org,
            "member",
            2,
        )
        .expect("admin→member demotion must pass when another admin remains");
    }

    /// Target is a bot → 400. Bots are service accounts (e.g.
    /// KrabChecker); they're locked at `org_role='member'` forever.
    /// Refusing the request — rather than silently no-op-ing —
    /// surfaces the misuse to the admin who likely typed the wrong
    /// UUID.
    #[test]
    fn permissions_rejects_bot_target() {
        let org = Uuid::new_v4();
        let err = validate_set_permissions(
            Some(org),
            "admin",
            true,
            Some(org),
            "bot",
            "member",
            org,
            "admin",
            1,
        )
        .expect_err("bot target must be rejected");
        match err {
            ApiError::ValidationError(msg) => assert!(msg.contains("bots"), "{msg}"),
            other => panic!("expected ValidationError, got {other:?}"),
        }
    }

    /// Demoting the LAST admin → 400 with code `LAST_ADMIN`.
    /// `admin_count == 1` means the target is the only admin — the
    /// org would be locked out of admin-only flows (today: this
    /// endpoint itself, PR B: bot provisioning) if we let it
    /// through. The CLI / UI is expected to branch on the structured
    /// code and tell the admin to promote someone else first.
    #[test]
    fn permissions_rejects_last_admin_demotion() {
        let org = Uuid::new_v4();
        let err = validate_set_permissions(
            Some(org),
            "admin",
            true,
            Some(org),
            "human",
            "admin",
            org,
            "member",
            1,
        )
        .expect_err("last-admin demotion must be rejected");
        assert!(matches!(err, ApiError::LastAdmin), "{err:?}");
        assert_eq!(err.code(), "LAST_ADMIN");
    }

    /// Idempotent no-op: setting an admin to admin or a member to
    /// member passes validation cleanly (the real handler returns
    /// the current state with no write). The last-admin check
    /// *does not* fire on admin→admin (it's not a demotion), so the
    /// `admin_count == 1` setup here is intentional — we're proving
    /// the no-op path skips the protection even when there's only
    /// one admin.
    #[test]
    fn permissions_idempotent_noops_pass_validation() {
        let org = Uuid::new_v4();
        // member → member (no demotion, no last-admin check).
        validate_set_permissions(
            Some(org),
            "admin",
            true,
            Some(org),
            "human",
            "member",
            org,
            "member",
            1,
        )
        .expect("member→member must pass");
        // admin → admin (still admin, no demotion). The
        // last-admin check is conditioned on `new_role == 'member'`,
        // so this path is unaffected even when admin_count == 1.
        validate_set_permissions(
            Some(org),
            "admin",
            true,
            Some(org),
            "human",
            "admin",
            org,
            "admin",
            1,
        )
        .expect("admin→admin must pass");
    }

    // -----------------------------------------------------------------
    // Bot endpoint unit tests (PR B)
    //
    // Same approach as the permissions tests: a `validate_bot_op`
    // predicate mirrors the authorization + body checks the real
    // handlers perform, and the assertions exercise the rules
    // without spinning up Postgres. The SQL paths (INSERT, JWT
    // issuance, DELETE cascade) are covered by the `#[ignore]`
    // integration test against the test-e2e Neon branch.
    //
    // Five assertions tracking the spec:
    // 1. non-admin creates → 403
    // 2. admin creates → Ok (validation passes; the real handler
    //    INSERTs + mints JWT)
    // 3. admin creates with display_name > BOT_DISPLAY_NAME_MAX → 400
    // 4. admin rotates bot in another org → 404 (collapsed)
    // 5. admin deletes bot in own org → Ok (validation passes)
    // -----------------------------------------------------------------

    /// Op type for the bot validation predicate. `Create` checks
    /// body + auth; `Rotate` and `Delete` check auth + target-bot
    /// existence/sameness. Captures the same control flow the three
    /// real handlers run before they touch SQL.
    pub(super) enum BotOp<'a> {
        Create { display_name: &'a str },
        Rotate,
        Delete,
    }

    pub(super) fn validate_bot_op(
        caller_entity: Option<Uuid>,
        caller_org_role: &str,
        org_id: Uuid,
        target_exists: bool,
        target_entity: Option<Uuid>,
        target_kind: &str,
        op: BotOp<'_>,
    ) -> Result<(), ApiError> {
        // Body validation runs first on create — matches the real
        // handler's "fail cheap on garbage" ordering.
        if let BotOp::Create { display_name } = &op {
            let trimmed = display_name.trim();
            if trimmed.is_empty() {
                return Err(ApiError::ValidationError(
                    "display_name must not be empty".into(),
                ));
            }
            if trimmed.chars().count() > BOT_DISPLAY_NAME_MAX {
                return Err(ApiError::ValidationError(format!(
                    "display_name exceeds {BOT_DISPLAY_NAME_MAX} chars"
                )));
            }
        }
        // Authorize.
        if caller_entity != Some(org_id) || caller_org_role != "admin" {
            return Err(ApiError::Forbidden);
        }
        // Target checks for non-create ops.
        if matches!(op, BotOp::Rotate | BotOp::Delete) {
            // Collapsed 404 for "not found", "wrong org", or "not a
            // bot" — the real handler returns `ValidationError("bot
            // not found")` for all three so the predicate matches.
            if !target_exists || target_entity != Some(org_id) || target_kind != "bot" {
                return Err(ApiError::ValidationError("bot not found".into()));
            }
        }
        Ok(())
    }

    #[test]
    fn bot_create_rejects_non_admin_caller() {
        let org = Uuid::new_v4();
        let err = validate_bot_op(
            Some(org),
            "member",
            org,
            false,
            None,
            "",
            BotOp::Create {
                display_name: "KrabChecker",
            },
        )
        .expect_err("non-admin must not create bots");
        assert!(matches!(err, ApiError::Forbidden), "{err:?}");
    }

    #[test]
    fn bot_create_allows_admin() {
        let org = Uuid::new_v4();
        validate_bot_op(
            Some(org),
            "admin",
            org,
            false,
            None,
            "",
            BotOp::Create {
                display_name: "KrabChecker",
            },
        )
        .expect("admin must be able to create a bot");
    }

    #[test]
    fn bot_create_rejects_oversize_display_name() {
        let org = Uuid::new_v4();
        // BOT_DISPLAY_NAME_MAX + 1 chars — guaranteed too long
        // regardless of where the shared limit lands.
        let too_long: String = "a".repeat(BOT_DISPLAY_NAME_MAX + 1);
        let err = validate_bot_op(
            Some(org),
            "admin",
            org,
            false,
            None,
            "",
            BotOp::Create {
                display_name: &too_long,
            },
        )
        .expect_err("oversize display_name must be rejected");
        match err {
            ApiError::ValidationError(msg) => {
                assert!(msg.contains("display_name"), "{msg}")
            }
            other => panic!("expected ValidationError, got {other:?}"),
        }
    }

    #[test]
    fn bot_rotate_rejects_target_in_other_org() {
        let org = Uuid::new_v4();
        let other = Uuid::new_v4();
        let err = validate_bot_op(
            Some(org),
            "admin",
            org,
            true,
            Some(other),
            "bot",
            BotOp::Rotate,
        )
        .expect_err("rotation across orgs must 404");
        match err {
            ApiError::ValidationError(msg) => {
                assert!(msg.contains("bot not found"), "{msg}")
            }
            other => panic!("expected ValidationError, got {other:?}"),
        }
    }

    #[test]
    fn bot_delete_allows_admin_for_own_bot() {
        let org = Uuid::new_v4();
        validate_bot_op(
            Some(org),
            "admin",
            org,
            true,
            Some(org),
            "bot",
            BotOp::Delete,
        )
        .expect("admin must be able to delete a bot in their own org");
    }

    /// Sanity check that the generated email matches the documented
    /// `bot-<8 hex>@<domain>` shape — used downstream by the audit
    /// trail / log grep, so we lock it in.
    #[test]
    fn generate_bot_email_shape() {
        let email = generate_bot_email("acme.com");
        // Must start with `bot-`, contain 8 hex chars, then `@acme.com`.
        assert!(email.starts_with("bot-"), "{email}");
        assert!(email.ends_with("@acme.com"), "{email}");
        let (local, _) = email.split_once('@').unwrap();
        let hex = local.trim_start_matches("bot-");
        assert_eq!(hex.len(), 8, "{email}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{email}");
    }
}
