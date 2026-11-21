// --- User management endpoints ---

/// Public-shaped user row for the admin endpoints. Includes the
/// admin-only fields (`suspended_until`, `suspension_reason`,
/// `email`) that we don't expose on `/v1/agents/{id}`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminUserResponse {
    pub user_id: Uuid,
    pub entity_id: Uuid,
    pub display_name: String,
    pub email: Option<String>,
    pub entity_name: Option<String>,
    pub entity_domain: Option<String>,
    pub role: String,
    pub org_role: String,
    pub kind: String,
    pub suspended_until: Option<chrono::DateTime<chrono::Utc>>,
    pub suspension_reason: Option<String>,
    pub rate_limit_until: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub claim_count: i64,
    pub battle_count: i64,
    pub last_activity_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<AdminUserClaimItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub battles: Vec<AdminUserBattleItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub risk_signals: Vec<String>,
    pub available_admin_actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<bool>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminUserListResponse {
    pub items: Vec<AdminUserResponse>,
    pub pagination: AdminListPaginationResponse,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminUserClaimItem {
    pub id: Uuid,
    pub slug: Option<String>,
    pub title: String,
    pub battle_id: Option<Uuid>,
    pub battle_slug: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminUserBattleItem {
    pub id: Uuid,
    pub slug: Option<String>,
    pub title: String,
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct ListUsersQuery {
    /// Exact email match (admin's primary lookup key).
    pub email: Option<String>,
    /// Filter by role (`admin`, `editor`, `moderator`, `user`).
    pub role: Option<String>,
    pub entity_id: Option<Uuid>,
    pub entity_domain: Option<String>,
    pub org_role: Option<String>,
    pub kind: Option<String>,
    pub suspended: Option<bool>,
    pub created_after: Option<DateTime<Utc>>,
    pub created_before: Option<DateTime<Utc>>,
    /// When `Some("eligible")`, excludes users who have unsubscribed
    /// from the digest (`user_email_preferences.digest_frequency =
    /// 'off'`). Any other non-empty value is a 400.
    pub digest: Option<String>,
    /// 1..200, default 50.
    pub limit: Option<i64>,
    /// Zero-based offset, default 0.
    pub offset: Option<i64>,
    /// When true, return only the filtered total count.
    pub count_only: Option<bool>,
    /// Comma-separated sort fields. Prefix with `+` or `-`.
    pub order_by: Option<String>,
}

/// GET /v1/admin/users
///
/// List users with optional account, entity, organization and
/// suspension filters. Email match is exact (admin tools always know
/// the email they want). Returns the full admin row including
/// suspension state.
#[utoipa::path(
    get,
    path = "/v1/admin/users",
    tag = "admin",
    params(
        ("email" = Option<String>, Query, description = "Exact email match"),
        ("role"  = Option<String>, Query, description = "Filter by role (admin|editor|moderator|user)"),
        ("entity_id" = Option<Uuid>, Query, description = "Filter by linked entity UUID"),
        ("entity_domain" = Option<String>, Query, description = "Filter by linked entity domain"),
        ("org_role" = Option<String>, Query, description = "Filter by organization role (admin|member)"),
        ("kind" = Option<String>, Query, description = "Filter by entity kind (human|bot)"),
        ("suspended" = Option<bool>, Query, description = "Filter currently suspended or active users"),
        ("created_after" = Option<DateTime<Utc>>, Query, description = "Created-at lower bound"),
        ("created_before" = Option<DateTime<Utc>>, Query, description = "Created-at upper bound"),
        ("digest" = Option<String>, Query, description = "When 'eligible', excludes users who unsubscribed from the digest"),
        ("limit" = Option<i64>,    Query, description = "Page size (1..200, default 50)"),
        ("offset" = Option<i64>,   Query, description = "Zero-based page offset"),
        ("count_only" = Option<bool>, Query, description = "Return only pagination.count and no items"),
        ("order_by" = Option<String>, Query, description = "Comma-separated sort fields, e.g. -created_at,+user_id"),
    ),
    responses(
        (status = 200, description = "User list", body = AdminUserListResponse),
        (status = 403, description = "Not an admin"),
    ),
)]
pub async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListUsersQuery>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;

    let pagination = admin_list_pagination(q.limit, q.offset, q.count_only)?;
    validate_admin_user_filters(&q)?;
    let digest_filter = digest_eligible_filter(&q);
    let order_by = admin_order_by(
        q.order_by.as_deref(),
        ADMIN_USER_SORT_FIELDS,
        ADMIN_USER_DEFAULT_SORT,
        AdminSortTerm::asc("user_id", "u.id"),
    )?;

    let where_sql = r#"
         WHERE ($1::text IS NULL OR u.email = $1)
           AND ($2::text IS NULL OR u.role = $2)
           AND ($3::uuid IS NULL OR u.entity_id = $3)
           AND ($4::text IS NULL OR e.domain = $4)
           AND ($5::text IS NULL OR u.org_role = $5)
           AND ($6::text IS NULL OR u.kind = $6)
           AND (
               $7::bool IS NULL
               OR (
                   $7 = TRUE
                   AND (
                       COALESCE(u.suspended_until > NOW(), FALSE)
                       OR COALESCE(u.rate_limit_until > NOW(), FALSE)
                   )
               )
               OR (
                   $7 = FALSE
                   AND NOT (
                       COALESCE(u.suspended_until > NOW(), FALSE)
                       OR COALESCE(u.rate_limit_until > NOW(), FALSE)
                   )
               )
           )
           AND ($8::timestamptz IS NULL OR u.created_at >= $8)
           AND ($9::timestamptz IS NULL OR u.created_at <= $9)
           AND (
               $10::bool IS NULL
               OR NOT EXISTS (
                   SELECT 1 FROM user_email_preferences p
                    WHERE p.user_id = u.id AND p.digest_frequency = 'off'
               )
           )
        "#;

    // Skip the full-set COUNT(*) on deep pages — see `should_count`.
    let count: i64 = if pagination.should_count() {
        let count_sql = format!(
            r#"
        SELECT COUNT(*)::BIGINT
          FROM users u
          JOIN entities e ON e.id = u.entity_id
        {where_sql}
        "#
        );
        sqlx::query_scalar(&count_sql)
            .bind(q.email.as_deref())
            .bind(q.role.as_deref())
            .bind(q.entity_id)
            .bind(q.entity_domain.as_deref())
            .bind(q.org_role.as_deref())
            .bind(q.kind.as_deref())
            .bind(q.suspended)
            .bind(q.created_after)
            .bind(q.created_before)
            .bind(digest_filter)
            .fetch_one(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    } else {
        ADMIN_COUNT_SKIPPED
    };

    let rows: Vec<AdminUserRow> = if pagination.count_only {
        Vec::new()
    } else {
        let sql = admin_user_select(&format!(
            "{where_sql} ORDER BY {order_by} LIMIT $11 OFFSET $12"
        ));
        sqlx::query_as(&sql)
            .bind(q.email.as_deref())
            .bind(q.role.as_deref())
            .bind(q.entity_id)
            .bind(q.entity_domain.as_deref())
            .bind(q.org_role.as_deref())
            .bind(q.kind.as_deref())
            .bind(q.suspended)
            .bind(q.created_after)
            .bind(q.created_before)
            .bind(digest_filter)
            .bind(pagination.limit)
            .bind(pagination.offset)
            .fetch_all(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    };

    let users: Vec<AdminUserResponse> = rows
        .into_iter()
        .map(|row| admin_user_response(row, Vec::new(), Vec::new(), None))
        .collect();

    Ok(Json(ApiResponse::new(AdminUserListResponse {
        items: users,
        pagination: pagination.response(count),
    })))
}

const ADMIN_USER_SORT_FIELDS: &[AdminSortField] = &[
    AdminSortField {
        public: "created_at",
        sql: "u.created_at",
    },
    AdminSortField {
        public: "updated_at",
        sql: "u.created_at",
    },
    AdminSortField {
        public: "email",
        sql: "u.email",
    },
    AdminSortField {
        public: "role",
        sql: "u.role",
    },
    AdminSortField {
        public: "entity_domain",
        sql: "e.domain",
    },
    AdminSortField {
        public: "org_role",
        sql: "u.org_role",
    },
    AdminSortField {
        public: "kind",
        sql: "u.kind",
    },
    AdminSortField {
        public: "suspended",
        sql: "(COALESCE(u.suspended_until > NOW(), FALSE) OR COALESCE(u.rate_limit_until > NOW(), FALSE))",
    },
    AdminSortField {
        public: "user_id",
        sql: "u.id",
    },
];

const ADMIN_USER_DEFAULT_SORT: &[AdminSortTerm] = &[
    AdminSortTerm::desc("created_at", "u.created_at"),
    AdminSortTerm::asc("user_id", "u.id"),
];

/// GET /v1/admin/users/{id_or_email}
///
/// Lookup a single user. Path slot accepts a UUID or an email
/// address — admins working from a Slack ping usually have one or
/// the other, not both. Returns 404 if neither matches.
#[utoipa::path(
    get,
    path = "/v1/admin/users/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "User UUID or email")),
    responses(
        (status = 200, description = "User found", body = AdminUserResponse),
        (status = 403, description = "Not an admin"),
        (status = 404, description = "User not found"),
    ),
)]
pub async fn get_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_or_email): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let user = load_admin_user(&state.pool, &id_or_email, None).await?;
    Ok(Json(ApiResponse::new(user)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub entity_id: Option<Uuid>,
    pub role: Option<String>,
    /// Set to `true` to clear `suspended_until` + `suspension_reason`
    /// + `rate_limit_until`.
    pub lift_suspension: Option<bool>,
    /// Set to a reason to suspend the user indefinitely. Mutually
    /// exclusive with `lift_suspension=true` in the same request.
    pub suspend_reason: Option<String>,
    pub reason: Option<String>,
    pub self_change_confirmed: Option<bool>,
}

/// PATCH /v1/admin/users/{user_id}
///
/// Back-compatible admin user update envelope. Existing `role`,
/// `lift_suspension`, and `suspend_reason` payloads still work;
/// newer callers can also change profile fields.
pub async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;
    validate_admin_reason(req.reason.as_deref())?;
    validate_update_user_request(&state.pool, actor, target_user_id, &req).await?;

    if let Some(display_name) = normalized_display_name(req.display_name.as_deref())? {
        sqlx::query("UPDATE users SET display_name = $1 WHERE id = $2")
            .bind(display_name)
            .bind(target_user_id)
            .execute(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    }

    if let Some(email) = normalized_email(req.email.as_deref())? {
        sqlx::query("UPDATE users SET email = $1 WHERE id = $2")
            .bind(email)
            .bind(target_user_id)
            .execute(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    }

    if let Some(entity_id) = req.entity_id {
        sqlx::query("UPDATE users SET entity_id = $1 WHERE id = $2")
            .bind(entity_id)
            .bind(target_user_id)
            .execute(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    }

    if let Some(ref role) = req.role {
        sqlx::query("UPDATE users SET role = $1 WHERE id = $2")
            .bind(role)
            .bind(target_user_id)
            .execute(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    }

    if req.lift_suspension == Some(true) {
        sqlx::query(
            "UPDATE users SET suspended_until = NULL, suspension_reason = NULL, rate_limit_until = NULL WHERE id = $1",
        )
        .bind(target_user_id)
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    } else if let Some(reason) = normalized_suspension_reason(req.suspend_reason.as_deref())? {
        // Indefinite suspension: `suspended_until` set far enough
        // in the future that the rate limiter treats it as
        // permanent. Lift via `lift_suspension=true`.
        let until = chrono::Utc::now() + chrono::Duration::days(365 * 100);
        sqlx::query("UPDATE users SET suspended_until = $1, suspension_reason = $2 WHERE id = $3")
            .bind(until)
            .bind(reason)
            .bind(target_user_id)
            .execute(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    }

    // Role / suspension may have changed — drop the cached live-auth
    // entry so this instance enforces it on the very next request.
    crate::routes::auth::invalidate_live_auth(target_user_id);
    let user = load_admin_user(&state.pool, &target_user_id.to_string(), Some(true)).await?;
    Ok(Json(ApiResponse::new(user)))
}

fn admin_user_select(where_sql: &str) -> String {
    format!(
        r#"
        SELECT u.id,
               u.entity_id,
               u.display_name,
               u.email,
               e.name AS entity_name,
               e.domain AS entity_domain,
               u.role,
               u.org_role,
               u.kind,
               u.suspended_until,
               u.suspension_reason,
               u.rate_limit_until,
               u.created_at,
               (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL) AS claim_count,
               (SELECT COUNT(*) FROM battles b WHERE b.author_id = u.id AND b.deleted_at IS NULL) AS battle_count,
               (
                   SELECT MAX(activity_at)
                   FROM (
                       SELECT MAX(c.created_at) AS activity_at FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL
                       UNION ALL
                       SELECT MAX(b.created_at) AS activity_at FROM battles b WHERE b.author_id = u.id AND b.deleted_at IS NULL
                       UNION ALL
                       SELECT MAX(v.created_at) AS activity_at FROM verifications v WHERE v.author_id = u.id
                   ) activity
               ) AS last_activity_at
          FROM users u
          JOIN entities e ON e.id = u.entity_id
        {where_sql}"#
    )
}

fn validate_admin_user_filters(q: &ListUsersQuery) -> Result<(), ApiError> {
    if let Some(ref role) = q.role
        && !VALID_ROLES.contains(&role.as_str())
    {
        return Err(ApiError::ValidationError(format!(
            "role must be one of {VALID_ROLES:?}"
        )));
    }
    if let Some(ref org_role) = q.org_role
        && !VALID_ORG_ROLES.contains(&org_role.as_str())
    {
        return Err(ApiError::ValidationError(format!(
            "org_role must be one of {VALID_ORG_ROLES:?}"
        )));
    }
    if let Some(ref kind) = q.kind
        && !VALID_USER_KINDS.contains(&kind.as_str())
    {
        return Err(ApiError::ValidationError(format!(
            "kind must be one of {VALID_USER_KINDS:?}"
        )));
    }
    if let Some(ref digest) = q.digest
        && digest != "eligible"
    {
        return Err(ApiError::ValidationError(
            "digest must be 'eligible'".to_string(),
        ));
    }
    Ok(())
}

/// `Some(true)` when the caller asked for `digest=eligible` (excludes
/// users who unsubscribed via `user_email_preferences.digest_frequency
/// = 'off'`), `None` otherwise — bound straight into the `$10::bool IS
/// NULL OR NOT EXISTS (...)` clause in `list_users`'s WHERE. Call only
/// after `validate_admin_user_filters` has rejected anything other
/// than "eligible".
fn digest_eligible_filter(q: &ListUsersQuery) -> Option<bool> {
    q.digest.as_deref().map(|_| true)
}

async fn load_admin_user(
    pool: &sqlx::PgPool,
    id_or_email: &str,
    updated: Option<bool>,
) -> Result<AdminUserResponse, ApiError> {
    let row: Option<AdminUserRow> = if let Ok(uuid) = Uuid::parse_str(id_or_email) {
        let sql = admin_user_select(" WHERE u.id = $1");
        sqlx::query_as(&sql)
            .bind(uuid)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    } else {
        let sql = admin_user_select(" WHERE u.email = $1");
        sqlx::query_as(&sql)
            .bind(id_or_email)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    };
    let row =
        row.ok_or_else(|| ApiError::ValidationError(format!("user '{id_or_email}' not found")))?;
    let claims = load_admin_user_claims(pool, row.id).await?;
    let battles = load_admin_user_battles(pool, row.id).await?;
    Ok(admin_user_response(row, claims, battles, updated))
}

#[derive(sqlx::FromRow)]
struct AdminUserClaimRow {
    id: Uuid,
    slug: Option<String>,
    claim: String,
    topic: Option<String>,
    battle_id: Option<Uuid>,
    battle_slug: Option<String>,
    created_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct AdminUserBattleRow {
    id: Uuid,
    slug: Option<String>,
    title: String,
    status: String,
    created_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

async fn load_admin_user_claims(
    pool: &sqlx::PgPool,
    user_id: Uuid,
) -> Result<Vec<AdminUserClaimItem>, ApiError> {
    let rows: Vec<AdminUserClaimRow> = sqlx::query_as(
        r#"
        SELECT c.id, c.slug, c.claim, c.topic, c.battle_id, b.slug AS battle_slug,
               c.created_at, c.deleted_at
          FROM claims c
          LEFT JOIN battles b ON b.id = c.battle_id
         WHERE c.author_id = $1
         ORDER BY c.created_at DESC
         LIMIT 20
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(rows
        .into_iter()
        .map(|row| AdminUserClaimItem {
            id: row.id,
            slug: row.slug,
            title: extract_claim_title(&row.claim, row.topic.as_deref()),
            battle_id: row.battle_id,
            battle_slug: row.battle_slug,
            created_at: row.created_at,
            deleted_at: row.deleted_at,
        })
        .collect())
}

async fn load_admin_user_battles(
    pool: &sqlx::PgPool,
    user_id: Uuid,
) -> Result<Vec<AdminUserBattleItem>, ApiError> {
    let rows: Vec<AdminUserBattleRow> = sqlx::query_as(
        r#"
        SELECT id, slug, title, status, created_at, deleted_at
          FROM battles
         WHERE author_id = $1
         ORDER BY created_at DESC
         LIMIT 20
        "#,
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(rows
        .into_iter()
        .map(|row| AdminUserBattleItem {
            id: row.id,
            slug: row.slug,
            title: row.title,
            status: row.status,
            created_at: row.created_at,
            deleted_at: row.deleted_at,
        })
        .collect())
}

fn admin_user_response(
    row: AdminUserRow,
    claims: Vec<AdminUserClaimItem>,
    battles: Vec<AdminUserBattleItem>,
    updated: Option<bool>,
) -> AdminUserResponse {
    let warnings = admin_user_warnings(&row);
    let risk_signals = admin_user_risk_signals(&row);
    let available_admin_actions = admin_user_available_actions(&row);
    AdminUserResponse {
        user_id: row.id,
        entity_id: row.entity_id,
        display_name: row.display_name,
        email: row.email,
        entity_name: Some(row.entity_name),
        entity_domain: Some(row.entity_domain),
        role: row.role,
        org_role: row.org_role,
        kind: row.kind,
        suspended_until: row.suspended_until,
        suspension_reason: row.suspension_reason,
        rate_limit_until: row.rate_limit_until,
        created_at: row.created_at,
        claim_count: row.claim_count,
        battle_count: row.battle_count,
        last_activity_at: row.last_activity_at,
        claims,
        battles,
        warnings,
        risk_signals,
        available_admin_actions,
        updated,
    }
}

fn admin_user_warnings(row: &AdminUserRow) -> Vec<String> {
    let mut warnings = Vec::new();
    let now = Utc::now();
    if row.suspended_until.is_some_and(|until| until > now) {
        warnings.push("User is currently suspended.".into());
    }
    if row.rate_limit_until.is_some_and(|until| until > now) {
        warnings.push("User is currently rate limited.".into());
    }
    warnings
}

fn admin_user_risk_signals(row: &AdminUserRow) -> Vec<String> {
    let mut signals = Vec::new();
    if row.role == "admin" {
        signals.push("User has platform admin access.".into());
    }
    if row.kind == "bot" {
        signals.push("User is a bot/service account.".into());
    }
    if row.claim_count > 0 || row.battle_count > 0 {
        signals.push("User has authored public platform content.".into());
    }
    signals
}

fn admin_user_available_actions(row: &AdminUserRow) -> Vec<String> {
    let mut actions = vec![
        "update".to_string(),
        "role_change".to_string(),
        "suspend".to_string(),
    ];
    let now = Utc::now();
    if row.suspended_until.is_some_and(|until| until > now)
        || row.rate_limit_until.is_some_and(|until| until > now)
    {
        actions.push("lift_suspension".to_string());
    }
    actions
}

async fn validate_update_user_request(
    pool: &sqlx::PgPool,
    actor: Uuid,
    target_user_id: Uuid,
    req: &UpdateUserRequest,
) -> Result<(), ApiError> {
    let current = load_admin_user(pool, &target_user_id.to_string(), None).await?;
    let has_suspend = normalized_suspension_reason(req.suspend_reason.as_deref())?.is_some()
        && req.lift_suspension != Some(true);
    let has_change = req.display_name.is_some()
        || req.email.is_some()
        || req.entity_id.is_some()
        || req.role.is_some()
        || req.lift_suspension == Some(true)
        || has_suspend;
    if !has_change {
        return Err(ApiError::ValidationError(
            "at least one user field must be provided".into(),
        ));
    }

    if let Some(ref role) = req.role
        && !VALID_ROLES.contains(&role.as_str())
    {
        return Err(ApiError::ValidationError(format!(
            "role must be one of {VALID_ROLES:?}"
        )));
    }

    normalized_display_name(req.display_name.as_deref())?;
    if let Some(email) = normalized_email(req.email.as_deref())? {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND id <> $2)")
                .bind(email)
                .bind(target_user_id)
                .fetch_one(pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
        if exists {
            return Err(ApiError::ValidationError(
                "email is already used by another user".into(),
            ));
        }
    }

    if let Some(entity_id) = req.entity_id {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM entities WHERE id = $1)")
                .bind(entity_id)
                .fetch_one(pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
        if !exists {
            return Err(ApiError::ValidationError("entity_id not found".into()));
        }
    }

    let role_change_touches_self = actor == target_user_id && req.role.is_some();
    let suspension_touches_self = actor == target_user_id && has_suspend;
    if (role_change_touches_self || suspension_touches_self)
        && req.self_change_confirmed != Some(true)
    {
        return Err(ApiError::ValidationError(
            "self_change_confirmed=true is required for self role or suspension changes".into(),
        ));
    }

    let demotes_admin =
        current.role == "admin" && req.role.as_deref().is_some_and(|role| role != "admin");
    let suspends_admin = current.role == "admin" && has_suspend;
    if demotes_admin || suspends_admin {
        let other_active_admin_exists: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM users
                WHERE role = 'admin'
                  AND id <> $1
                  AND NOT COALESCE(suspended_until > NOW(), FALSE)
                  AND NOT COALESCE(rate_limit_until > NOW(), FALSE)
            )
            "#,
        )
        .bind(target_user_id)
        .fetch_one(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
        if !other_active_admin_exists {
            return Err(ApiError::LastAdmin);
        }
    }

    Ok(())
}

fn normalized_display_name(value: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    Field::UsersDisplayName.check(trimmed)?;
    Ok(Some(trimmed.to_string()))
}

fn normalized_email(value: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if !trimmed.contains('@') {
        return Err(ApiError::InvalidEmail);
    }
    Ok(Some(trimmed.to_string()))
}

fn normalized_suspension_reason(value: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::ValidationError(
            "suspend_reason must not be empty".into(),
        ));
    }
    validate_admin_reason(Some(trimmed))?;
    Ok(Some(trimmed.to_string()))
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MintJwtResponse {
    pub user_id: Uuid,
    pub display_name: String,
    pub role: String,
    pub token: String,
}

/// POST /v1/admin/users/{user_id}/mint-jwt
///
/// Mint a fresh JWT for an existing user. Admin-only. Returns the
/// token in plaintext exactly once — the caller is expected to stash
/// it in Secret Manager + redeploy whoever consumes it.
///
/// Originally added to bootstrap the merged `krabagent` bot user
/// (migration 049): the merge migration creates the row but can't
/// mint a token (no JWT_SECRET access from psql), and the existing
/// `POST /v1/orgs/{id}/bots` endpoint only mints for NEW rows it
/// creates itself. This endpoint closes the gap.
///
/// Security: the token carries the user's CURRENT `role` from the
/// users table. If the operator wants to mint an elevated token,
/// they PATCH the role first (existing endpoint) then mint.
#[utoipa::path(
    post,
    path = "/v1/admin/users/{user_id}/mint-jwt",
    tag = "admin",
    params(("user_id" = Uuid, Path, description = "User UUID")),
    responses(
        (status = 200, description = "Freshly minted JWT", body = MintJwtResponse),
        (status = 403, description = "Caller is not an admin"),
        (status = 404, description = "User not found"),
    ),
)]
pub async fn mint_user_jwt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;

    #[derive(sqlx::FromRow)]
    struct UserRow {
        id: Uuid,
        display_name: String,
        entity_id: Uuid,
        role: String,
    }

    let row: Option<UserRow> =
        sqlx::query_as("SELECT id, display_name, entity_id, role FROM users WHERE id = $1")
            .bind(target_user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

    let user = row.ok_or_else(|| ApiError::ValidationError("user not found".into()))?;

    let token = crate::services::auth_service::create_jwt(
        user.id,
        user.entity_id,
        &user.role,
        &state.jwt_secret,
    )?;

    Ok(Json(ApiResponse::new(MintJwtResponse {
        user_id: user.id,
        display_name: user.display_name,
        role: user.role,
        token,
    })))
}

// --- Battle management endpoints ---
