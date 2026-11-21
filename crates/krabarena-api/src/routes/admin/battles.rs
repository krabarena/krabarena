/// `GET /v1/admin/battles` — list Battles for admin moderation/tooling.
pub async fn list_battles_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListBattlesAdminQuery>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;

    let pagination = admin_list_pagination(q.limit, q.offset, q.count_only)?;
    let status = normalize_admin_battle_status(q.status.as_deref())?;
    let domain_tag = normalize_optional_filter("domain_tag", q.domain_tag.as_deref())?;
    let order_by = admin_order_by(
        q.order_by.as_deref(),
        ADMIN_BATTLE_SORT_FIELDS,
        ADMIN_BATTLE_DEFAULT_SORT,
        AdminSortTerm::asc("id", "b.id"),
    )?;

    // Skip the full-set COUNT(*) on deep pages — see `should_count`.
    let count: i64 = if pagination.should_count() {
        let count_sql =
            format!("SELECT COUNT(*)::BIGINT FROM ({ADMIN_BATTLE_BASE_QUERY}) filtered");
        sqlx::query_scalar(&count_sql)
            .bind(status.as_deref())
            .bind(domain_tag.as_deref())
            .bind(q.has_claims)
            .bind(q.missing_solutions)
            .bind(q.stale_summary)
            .bind(q.updated_after)
            .bind(q.updated_before)
            .bind(q.include_deleted.unwrap_or(false))
            .fetch_one(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    } else {
        ADMIN_COUNT_SKIPPED
    };

    let rows: Vec<AdminBattleRow> = if pagination.count_only {
        Vec::new()
    } else {
        let page_sql =
            format!("{ADMIN_BATTLE_BASE_QUERY} ORDER BY {order_by} LIMIT $9 OFFSET $10");
        sqlx::query_as(&page_sql)
            .bind(status.as_deref())
            .bind(domain_tag.as_deref())
            .bind(q.has_claims)
            .bind(q.missing_solutions)
            .bind(q.stale_summary)
            .bind(q.updated_after)
            .bind(q.updated_before)
            .bind(q.include_deleted.unwrap_or(false))
            .bind(pagination.limit)
            .bind(pagination.offset)
            .fetch_all(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    };

    let battles: Vec<AdminBattleListItem> = rows.into_iter().map(admin_battle_list_item).collect();
    Ok(Json(ApiResponse::new(AdminBattleListResponse {
        items: battles,
        pagination: pagination.response(count),
    })))
}

const ADMIN_BATTLE_SORT_FIELDS: &[AdminSortField] = &[
    AdminSortField {
        public: "created_at",
        sql: "b.created_at",
    },
    AdminSortField {
        public: "updated_at",
        sql: "b.updated_at",
    },
    AdminSortField {
        public: "title",
        sql: "b.title",
    },
    AdminSortField {
        public: "status",
        sql: "b.status",
    },
    AdminSortField {
        public: "claim_count",
        sql: "COALESCE(cs.claim_count, 0)",
    },
    AdminSortField {
        public: "id",
        sql: "b.id",
    },
];

const ADMIN_BATTLE_DEFAULT_SORT: &[AdminSortTerm] = &[
    AdminSortTerm::desc("updated_at", "b.updated_at"),
    AdminSortTerm::asc("id", "b.id"),
];

/// `GET /v1/admin/battles/{battle_id}` — inspect one Battle by UUID, slug, or URL.
pub async fn get_battle_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let battle_id =
        crate::services::slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let battle = load_admin_battle(&state.pool, battle_id).await?;
    Ok(Json(ApiResponse::new(battle)))
}

/// `POST /v1/admin/battles` — create a Battle as a platform admin.
pub async fn create_battle_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateBattleAdminRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;
    validate_admin_reason(req.reason.as_deref())?;
    let status = normalize_admin_battle_status(req.status.as_deref())?;

    let create_req = CreateBattleRequest {
        title: req.title,
        description: req.description,
        domain_tags: req.domain_tags,
        parent_id: req.parent_id,
        proxy_spec: req.proxy_spec,
        produced_by: req.produced_by,
    };
    let created =
        crate::services::battle_service::create_battle(&state.pool, actor, create_req).await?;

    if let Some(status) = status {
        let update_req = UpdateBattleRequest {
            title: None,
            description: None,
            domain_tags: None,
            status: Some(status),
            proxy_spec: None,
            produced_by: None,
        };
        crate::services::battle_service::update_battle(&state.pool, created.id, actor, update_req)
            .await?;
    }

    tracing::info!(
        actor = %actor,
        battle_id = %created.id,
        reason = ?req.reason.as_deref(),
        "admin.battle.created"
    );
    let battle = load_admin_battle(&state.pool, created.id).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(battle))))
}

/// `PATCH /v1/admin/battles/{battle_id}` — edit explicitly supplied Battle fields.
pub async fn update_battle_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    Json(req): Json<UpdateBattleAdminRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;
    validate_admin_reason(req.reason.as_deref())?;
    if req.title.is_none()
        && req.description.is_none()
        && req.domain_tags.is_none()
        && req.status.is_none()
        && req.proxy_spec.is_none()
        && req.produced_by.is_none()
    {
        return Err(ApiError::ValidationError(
            "at least one Battle field must be provided".into(),
        ));
    }

    let battle_id =
        crate::services::slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let update_req = UpdateBattleRequest {
        title: req.title,
        description: req.description,
        domain_tags: req.domain_tags,
        status: req
            .status
            .as_deref()
            .map(|status| normalize_admin_battle_status(Some(status)))
            .transpose()?
            .flatten(),
        proxy_spec: req.proxy_spec,
        produced_by: req.produced_by,
    };
    crate::services::battle_service::update_battle(&state.pool, battle_id, actor, update_req)
        .await?;

    tracing::info!(
        actor = %actor,
        battle_id = %battle_id,
        reason = ?req.reason.as_deref(),
        "admin.battle.updated"
    );
    let battle = load_admin_battle(&state.pool, battle_id).await?;
    Ok(Json(ApiResponse::new(battle)))
}

const ADMIN_BATTLE_SELECT: &str = r#"
        SELECT b.id,
               b.author_id,
               u.display_name AS author_name,
               u.email AS author_email,
               e.domain AS entity_domain,
               b.parent_id,
               b.slug,
               b.title,
               b.description,
               b.domain_tags,
               b.status,
               b.created_at,
               b.updated_at,
               b.executive_summary,
               b.short_summary,
               b.og_summary,
               b.proxy_spec,
               b.cover_image_url,
               b.cover_image_alt,
               b.solutions,
               b.deleted_at,
               COALESCE(cs.claim_count, 0)::BIGINT AS claim_count,
               COALESCE(cs.live_claim_count, 0)::BIGINT AS live_claim_count,
               COALESCE(cs.deleted_claim_count, 0)::BIGINT AS deleted_claim_count,
               COALESCE(cs.total_confirmed, 0)::BIGINT AS total_confirmed,
               COALESCE(cs.total_refuted, 0)::BIGINT AS total_refuted,
               COALESCE(vs.verification_rows, 0)::BIGINT AS verification_rows
          FROM battles b
          LEFT JOIN users u ON u.id = b.author_id
          LEFT JOIN entities e ON e.id = u.entity_id
          LEFT JOIN (
                SELECT battle_id,
                       COUNT(*)::BIGINT AS claim_count,
                       COUNT(*) FILTER (WHERE deleted_at IS NULL) AS live_claim_count,
                       COUNT(*) FILTER (WHERE deleted_at IS NOT NULL) AS deleted_claim_count,
                       COALESCE(SUM(verify_count) FILTER (WHERE deleted_at IS NULL), 0) AS total_confirmed,
                       COALESCE(SUM(refute_count) FILTER (WHERE deleted_at IS NULL), 0) AS total_refuted
                  FROM claims
                 WHERE battle_id IS NOT NULL
                 GROUP BY battle_id
          ) cs ON cs.battle_id = b.id
          LEFT JOIN (
                SELECT c.battle_id,
                       COUNT(v.id)::BIGINT AS verification_rows
                  FROM claims c
                  LEFT JOIN verifications v ON v.claim_id = c.id
                 WHERE c.battle_id IS NOT NULL
                   AND c.deleted_at IS NULL
                 GROUP BY c.battle_id
          ) vs ON vs.battle_id = b.id
"#;

const ADMIN_BATTLE_BASE_QUERY: &str = r#"
        SELECT b.id,
               b.author_id,
               u.display_name AS author_name,
               u.email AS author_email,
               e.domain AS entity_domain,
               b.parent_id,
               b.slug,
               b.title,
               b.description,
               b.domain_tags,
               b.status,
               b.created_at,
               b.updated_at,
               b.executive_summary,
               b.short_summary,
               b.og_summary,
               b.proxy_spec,
               b.cover_image_url,
               b.cover_image_alt,
               b.solutions,
               b.deleted_at,
               COALESCE(cs.claim_count, 0)::BIGINT AS claim_count,
               COALESCE(cs.live_claim_count, 0)::BIGINT AS live_claim_count,
               COALESCE(cs.deleted_claim_count, 0)::BIGINT AS deleted_claim_count,
               COALESCE(cs.total_confirmed, 0)::BIGINT AS total_confirmed,
               COALESCE(cs.total_refuted, 0)::BIGINT AS total_refuted,
               COALESCE(vs.verification_rows, 0)::BIGINT AS verification_rows
          FROM battles b
          LEFT JOIN users u ON u.id = b.author_id
          LEFT JOIN entities e ON e.id = u.entity_id
          LEFT JOIN (
                SELECT battle_id,
                       COUNT(*)::BIGINT AS claim_count,
                       COUNT(*) FILTER (WHERE deleted_at IS NULL) AS live_claim_count,
                       COUNT(*) FILTER (WHERE deleted_at IS NOT NULL) AS deleted_claim_count,
                       COALESCE(SUM(verify_count) FILTER (WHERE deleted_at IS NULL), 0) AS total_confirmed,
                       COALESCE(SUM(refute_count) FILTER (WHERE deleted_at IS NULL), 0) AS total_refuted
                  FROM claims
                 WHERE battle_id IS NOT NULL
                 GROUP BY battle_id
          ) cs ON cs.battle_id = b.id
          LEFT JOIN (
                SELECT c.battle_id,
                       COUNT(v.id)::BIGINT AS verification_rows
                  FROM claims c
                  LEFT JOIN verifications v ON v.claim_id = c.id
                 WHERE c.battle_id IS NOT NULL
                   AND c.deleted_at IS NULL
                 GROUP BY c.battle_id
          ) vs ON vs.battle_id = b.id
         WHERE ($1::text IS NULL OR b.status = $1)
           AND ($2::text IS NULL OR $2 = ANY(b.domain_tags))
           AND ($3::bool IS NULL OR ((COALESCE(cs.claim_count, 0) > 0) = $3))
           AND ($4::bool IS NULL OR (((CASE WHEN jsonb_typeof(b.solutions) = 'array' THEN jsonb_array_length(b.solutions) ELSE 0 END) = 0) = $4))
           AND ($5::bool IS NULL OR (((b.executive_summary IS NULL OR b.short_summary IS NULL)) = $5))
           AND ($6::timestamptz IS NULL OR b.updated_at >= $6)
           AND ($7::timestamptz IS NULL OR b.updated_at <= $7)
           AND ($8::bool OR b.deleted_at IS NULL)
"#;

async fn load_admin_battle(
    pool: &sqlx::PgPool,
    battle_id: Uuid,
) -> Result<AdminBattleInspectResponse, ApiError> {
    let sql = format!("{ADMIN_BATTLE_SELECT} WHERE b.id = $1");
    let row: AdminBattleRow = sqlx::query_as(&sql)
        .bind(battle_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::BattleNotFound)?;

    let claims: Vec<AdminBattleClaimItem> = sqlx::query_as::<_, AdminBattleClaimRow>(
        r#"
        SELECT id,
               slug,
               claim,
               visibility,
               created_at,
               updated_at,
               deleted_at,
               verify_count,
               refute_count,
               claim_structure
          FROM claims
         WHERE battle_id = $1
         ORDER BY created_at DESC
        "#,
    )
    .bind(battle_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .into_iter()
    .map(admin_battle_claim_item)
    .collect();

    let item = admin_battle_list_item(row.clone());
    let summaries = admin_battle_summary_state(&row);
    let warnings = admin_battle_warnings(&row);
    let risk_signals = admin_battle_risk_signals(&row);
    let leaderboard = battle_leaderboard_from_claims(&claims);
    Ok(AdminBattleInspectResponse {
        proxy_spec_present: row.proxy_spec.is_some(),
        proxy_spec: row.proxy_spec,
        cover_image_url: row.cover_image_url,
        cover_image_alt: row.cover_image_alt,
        solutions: row.solutions,
        summaries,
        claims,
        leaderboard,
        warnings,
        risk_signals,
        available_actions: vec!["update", "refresh_summary", "backfill_solutions", "delete"],
        item,
    })
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AdminBattleClaimRow {
    id: Uuid,
    slug: Option<String>,
    claim: String,
    visibility: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
    verify_count: i32,
    refute_count: i32,
    claim_structure: Option<Value>,
}

fn admin_battle_claim_item(row: AdminBattleClaimRow) -> AdminBattleClaimItem {
    AdminBattleClaimItem {
        id: row.id,
        slug: row.slug.clone(),
        title: extract_claim_title(&row.claim, None),
        url: claim_browser_url(row.id, row.slug.as_deref()),
        visibility: row.visibility,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
        verify_count: row.verify_count,
        refute_count: row.refute_count,
        claim_structure: row.claim_structure,
    }
}

fn admin_battle_list_item(row: AdminBattleRow) -> AdminBattleListItem {
    let summaries = admin_battle_summary_state(&row);
    let solution_count = jsonb_array_len(&row.solutions);
    let url = battle_browser_url(row.id, row.slug.as_deref());
    AdminBattleListItem {
        id: row.id,
        slug: row.slug.clone(),
        url: url.clone(),
        battle_id: row.id,
        battle_slug: row.slug.clone(),
        battle_title: row.title.clone(),
        title: row.title,
        description: row.description,
        status: row.status,
        domain_tags: row.domain_tags,
        author: AdminBattleAuthor {
            user_id: row.author_id,
            display_name: row.author_name,
            email: row.author_email,
            entity_domain: row.entity_domain,
        },
        parent_id: row.parent_id,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
        counts: AdminBattleCounts {
            claim_count: row.claim_count,
            live_claim_count: row.live_claim_count,
            deleted_claim_count: row.deleted_claim_count,
            total_confirmed: row.total_confirmed,
            total_refuted: row.total_refuted,
            verification_rows: row.verification_rows,
        },
        has_summary: summaries.has_executive_summary || summaries.has_short_summary,
        has_solutions: solution_count > 0,
        missing_solutions: solution_count == 0,
        stale_summary: summaries.stale,
    }
}

fn admin_battle_summary_state(row: &AdminBattleRow) -> AdminBattleSummaryState {
    let has_executive_summary = row
        .executive_summary
        .as_deref()
        .is_some_and(|summary| !summary.trim().is_empty());
    let has_short_summary = row
        .short_summary
        .as_deref()
        .is_some_and(|summary| !summary.trim().is_empty());
    AdminBattleSummaryState {
        has_executive_summary,
        has_short_summary,
        executive_summary: row.executive_summary.clone(),
        short_summary: row.short_summary.clone(),
        og_summary: row.og_summary.clone(),
        stale: !has_executive_summary || !has_short_summary,
    }
}

fn admin_battle_warnings(row: &AdminBattleRow) -> Vec<String> {
    let mut warnings = Vec::new();
    if row.deleted_at.is_some() {
        warnings.push("battle is currently soft-deleted".to_string());
    }
    if row.live_claim_count == 0 {
        warnings.push("battle has no live Claims".to_string());
    }
    if jsonb_array_len(&row.solutions) == 0 {
        warnings.push("battle has no backfilled solutions".to_string());
    }
    if row.executive_summary.is_none() {
        warnings.push("battle has no executive summary".to_string());
    }
    warnings
}

fn admin_battle_risk_signals(row: &AdminBattleRow) -> Vec<String> {
    let mut signals = Vec::new();
    if row.live_claim_count > 0 {
        signals.push("Battle has live Claims; edits can affect existing published context".into());
    }
    if row.verification_rows > 0 {
        signals.push("Battle has verification activity; deletion can hide active work".into());
    }
    if row.proxy_spec.is_some() {
        signals.push("Battle has proxy configuration; edits may affect sponsored access".into());
    }
    signals
}

fn battle_leaderboard_from_claims(claims: &[AdminBattleClaimItem]) -> Value {
    let rows: Vec<Value> = claims
        .iter()
        .filter_map(|claim| {
            let structure = claim.claim_structure.as_ref()?;
            Some(serde_json::json!({
                "claim_id": claim.id,
                "claim_slug": claim.slug,
                "claim_title": claim.title,
                "leaderboard": structure.get("leaderboard").cloned().unwrap_or(Value::Null),
                "winner": structure.get("winner").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect();
    Value::Array(rows)
}

fn jsonb_array_len(value: &Value) -> usize {
    value.as_array().map(Vec::len).unwrap_or(0)
}

fn normalize_admin_battle_status(status: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(status) = status else {
        return Ok(None);
    };
    let trimmed = status.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    BattleStatus::parse_lenient(trimmed)
        .map(|status| Some(status.to_string()))
        .ok_or_else(|| {
            ApiError::ValidationError(format!(
                "status must be one of: open, active, settled (got '{trimmed}')"
            ))
        })
}

fn normalize_optional_filter(field: &str, value: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return Err(ApiError::ValidationError(format!(
            "{field} must be a single line"
        )));
    }
    Ok(Some(trimmed.to_string()))
}

pub async fn backfill_battle_solutions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let battle_id =
        crate::services::slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let summary = backfill_one_battle(&state.pool, &state.proxy_http, battle_id).await?;
    Ok(Json(ApiResponse::new(summary)))
}

/// `POST /v1/admin/battles/backfill-all-solutions`
///
/// Run [`backfill_battle_solutions`] for every Battle. Returns a
/// counter envelope `{battle_count, added_count, skipped_existing,
/// refreshed_count}`.
///
/// Idempotent — second run is a no-op IF the parser produces the
/// same domain for every existing entry. The refresh pass overwrites
/// `logo_url` + `website_url` whenever the stored value disagrees
/// with the parser's current answer; running it after a parser
/// change (e.g. the 0.14.5 target-vendor-in-parens precedence flip)
/// rewrites every entry the parser now resolves differently. This is
/// a deliberate widening of the previous 0.13.2 "only refresh nulls"
/// behaviour — see `refresh_null_logos` for the new contract.
pub async fn backfill_all_battle_solutions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let battle_ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM battles")
        .fetch_all(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let mut battle_count = 0usize;
    let mut added_total = 0usize;
    let mut existing_total = 0usize;
    let mut refreshed_total = 0usize;
    for id in battle_ids {
        match backfill_one_battle(&state.pool, &state.proxy_http, id).await {
            Ok(s) => {
                battle_count += 1;
                added_total += s.added as usize;
                existing_total += s.existing as usize;
                refreshed_total += s.refreshed as usize;
            }
            Err(e) => {
                tracing::warn!(battle_id = %id, error = ?e, "backfill_one_battle failed; continuing");
            }
        }
    }
    Ok(Json(ApiResponse::new(serde_json::json!({
        "battle_count": battle_count,
        "added_count": added_total,
        "skipped_existing": existing_total,
        "refreshed_count": refreshed_total,
    }))))
}

/// Response envelope for the per-battle backfill. `added` counts
/// newly-inserted entries; `existing` counts entries already in
/// `battles.solutions` that were preserved verbatim; `refreshed`
/// counts entries whose `logo_url` was null and which the vendor map
/// now resolves (a 0.13.2 follow-up so vendor-map expansions pick up
/// the entries that missed on first-backfill). Idempotent re-runs
/// with no vendor-map change see `added = 0`, `refreshed = 0`,
/// `existing = N`.
#[derive(Debug, Serialize)]
pub struct BackfillSolutionsResponse {
    pub battle_id: Uuid,
    pub added: u32,
    pub existing: u32,
    #[serde(default)]
    pub refreshed: u32,
    pub solutions: Vec<crate::models::battle::BattleSolution>,
}

/// Core backfill — used by both single- and bulk-Battle endpoints.
/// Reads every Claim's `claim_structure.leaderboard[i].solution`,
/// extracts distinct normalised names, derives `(logo_url,
/// website_url)` via the vendor map, and writes the merged list
/// back to `battles.solutions`. Existing entries are kept verbatim
/// (existing-key match → no overwrite of logo_url / website_url so
/// editor-curated logos win over the vendor map's guess).
pub async fn backfill_one_battle(
    pool: &sqlx::PgPool,
    http: &reqwest::Client,
    battle_id: Uuid,
) -> Result<BackfillSolutionsResponse, ApiError> {
    use crate::models::battle::{BattleSolution, parse_solutions};
    use crate::services::solution_names::{
        host_from_url, lookup_vendor, normalize_name, website_url_for,
    };

    // 1. Read battle.solutions (existing) — preserved on idempotent re-runs.
    let existing_raw: serde_json::Value =
        sqlx::query_scalar("SELECT solutions FROM battles WHERE id = $1")
            .bind(battle_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
            .ok_or(ApiError::BattleNotFound)?;
    let mut merged: Vec<BattleSolution> = parse_solutions(&existing_raw);
    let existing_keys: std::collections::HashSet<String> =
        merged.iter().map(|s| s.name_normalized.clone()).collect();
    let existing_count = merged.len() as u32;

    // Load the vendor→logo map from Neon (`vendor_logos`, migration 055).
    // Used to be the hardcoded `VENDOR_PATTERNS` const; now data-driven
    // so new competitors get logos by inserting a row + re-running this.
    let vendor_patterns = crate::services::solution_names::load_vendor_patterns(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    // 1b. Refresh pass (0.13.2): entries whose `logo_url` is still
    // null pick up a logo when the vendor map now resolves their
    // `name`. Editor-curated overrides (logo_url already set, even
    // to a custom URL) are left untouched.
    let refreshed_count = refresh_null_logos(&mut merged, &vendor_patterns);

    // 2. Read every claim's claim_structure leaderboard solutions.
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT claim_structure FROM claims WHERE battle_id = $1 \
         AND claim_structure IS NOT NULL",
    )
    .bind(battle_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    // Collect (normalized_key → first-seen original_name) so we
    // preserve the casing of the FIRST claim that produced the
    // solution. Authors usually use a consistent casing per Battle.
    let mut first_seen: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // Author-supplied `website_url` per new key (first-seen), used as the
    // logo fallback when the curated vendor map has no entry for the name.
    let mut first_seen_website: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for raw in rows {
        let Some(arr) = raw.get("leaderboard").and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in arr {
            let Some(solution) = entry.get("solution").and_then(|v| v.as_str()) else {
                continue;
            };
            let key = normalize_name(solution);
            if key.is_empty() || existing_keys.contains(&key) {
                continue;
            }
            if let Some(site) = entry.get("website_url").and_then(|v| v.as_str()) {
                if !site.trim().is_empty() {
                    first_seen_website
                        .entry(key.clone())
                        .or_insert_with(|| site.trim().to_string());
                }
            }
            first_seen
                .entry(key)
                .or_insert_with(|| solution.trim().to_string());
        }
    }

    // 3. For each new key, derive its logo. Curated vendor map wins
    //    (brand-accurate, e.g. Anthropic for "Claude Skill"); otherwise
    //    fall back to the domain the claim author gave in the
    //    leaderboard's `website_url`. For that website-derived domain we
    //    fetch the favicon once to extract a brand color (and self-host
    //    it when a GCS store is configured) so the avatar/pie match the
    //    vendor path instead of stalling on a fallback color.
    //
    //    AGENT-BRAND-WINS: `lookup_vendor` already deprioritizes a base
    //    LLM model in parens (e.g. "Junie (gpt-5.3-codex)") so the agent /
    //    inference co / tool / harness keeps its OWN brand — and returns
    //    None rather than the model provider on a miss, so the
    //    website-fallback below uses the contender's site, never the LLM's
    //    logo. See the `solution_names` module docs.
    let mut added: Vec<BattleSolution> = Vec::with_capacity(first_seen.len());
    for (key, name) in first_seen {
        let (logo_url, website_url, brand_color) = match lookup_vendor(&name, &vendor_patterns) {
            Some(v) => (
                // GCP-only: self-hosted asset or nothing — never an external
                // Google favicon (which would leak/hang the OG render).
                v.stored_url.clone(),
                Some(website_url_for(&v.domain)),
                v.brand_color.clone(),
            ),
            None => match first_seen_website.get(&key).and_then(|u| host_from_url(u)) {
                Some(domain) => {
                    // resolve_domain_asset self-hosts the favicon into GCP and
                    // returns its stored_url; keep it GCP-only (None on failure).
                    let (stored_url, brand_color) =
                        crate::services::logo_assets::resolve_domain_asset(http, &domain).await;
                    (
                        stored_url,
                        Some(website_url_for(&domain)),
                        brand_color,
                    )
                }
                None => (None, None, None),
            },
        };
        added.push(BattleSolution {
            name,
            name_normalized: key,
            logo_url,
            website_url,
            brand_color,
        });
    }
    // Stable order for the response and for the JSONB payload.
    added.sort_by(|a, b| a.name_normalized.cmp(&b.name_normalized));
    let added_count = added.len() as u32;
    merged.extend(added);

    // 4. Persist the merged list back.
    let payload = serde_json::to_value(&merged)
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("serialize solutions: {e}")))?;
    sqlx::query("UPDATE battles SET solutions = $1, updated_at = NOW() WHERE id = $2")
        .bind(&payload)
        .bind(battle_id)
        .execute(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        battle_id = %battle_id,
        added = added_count,
        refreshed = refreshed_count,
        existing = existing_count,
        "battle_solutions.backfilled"
    );

    Ok(BackfillSolutionsResponse {
        battle_id,
        added: added_count,
        existing: existing_count,
        refreshed: refreshed_count,
        solutions: merged,
    })
}

/// Walk the existing solutions list and re-run the vendor map against
/// each entry's `name`. If the map now resolves to a domain whose
/// expected favicon URL differs from what's stored (including the
/// "stored is null" case), overwrite `logo_url` AND `website_url` in
/// place. Returns the number of entries refreshed.
///
/// 0.14.5 — the previous "preserve editor-curated overrides" guard
/// (0.13.2) is gone. The parser change (target-in-parens precedence)
/// IS the curation update: every `BrightData (Google)` row needs its
/// logo to flip from the proxy domain to the target. Hand-pinned
/// custom URLs would survive the old guard but are exactly what the
/// fix is trying to overwrite. Editors who want custom logos can
/// re-pin after the backfill — they're easier to spot once the
/// proxy-vs-target swap has propagated.
///
/// Entries whose name doesn't resolve through the vendor map are
/// left untouched (no logo regression for true unknowns).
///
/// AGENT-BRAND-WINS: resolution goes through `lookup_vendor`, so an
/// `"<Agent> (<base model>)"` entry resolves from the agent/tool/company —
/// never the base LLM model in parens. A contender running on a known
/// model therefore keeps (or is refreshed to) its own brand, not the LLM
/// provider's logo. See the `solution_names` module docs.
fn refresh_null_logos(
    merged: &mut [crate::models::battle::BattleSolution],
    patterns: &[crate::services::solution_names::CompiledVendor],
) -> u32 {
    use crate::services::solution_names::{lookup_vendor, website_url_for};

    let mut refreshed = 0u32;
    for entry in merged.iter_mut() {
        let Some(v) = lookup_vendor(&entry.name, patterns) else {
            // Vendor map can't place this name — leave whatever's
            // stored (editor-curated, null, whatever) alone.
            continue;
        };
        // GCP-only: the self-hosted `stored_url` (migration 057) or nothing.
        // We never write an external Google favicon; a vendor whose asset
        // isn't self-hosted yet stays logo-less until refresh-assets runs.
        let expected_logo = v.stored_url.clone();
        let expected_website = website_url_for(&v.domain);
        let logo_drift = entry.logo_url != expected_logo;
        let website_drift = entry.website_url.as_deref() != Some(expected_website.as_str());
        let color_drift = v.brand_color.is_some() && entry.brand_color != v.brand_color;
        if !logo_drift && !website_drift && !color_drift {
            continue;
        }
        entry.logo_url = expected_logo;
        entry.website_url = Some(expected_website);
        if v.brand_color.is_some() {
            entry.brand_color = v.brand_color.clone();
        }
        refreshed += 1;
    }
    refreshed
}

// ---------------------------------------------------------------------------
// Admin soft-delete + restore for Battles
// ---------------------------------------------------------------------------

/// `DELETE /v1/admin/battles/{id}` — soft-delete a Battle. Idempotent:
/// re-deleting an already-deleted battle is a no-op success.
pub async fn soft_delete_battle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    body: Option<Json<AdminReasonRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    validate_admin_reason(req.reason.as_deref())?;
    let battle_id =
        crate::services::slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let res =
        sqlx::query("UPDATE battles SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL")
            .bind(battle_id)
            .execute(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    tracing::info!(
        actor = %actor,
        battle_id = %battle_id,
        changed = res.rows_affected(),
        reason = ?req.reason.as_deref(),
        "admin.battle.soft_deleted"
    );
    Ok(Json(ApiResponse::new(serde_json::json!({
        "battle_id": battle_id,
        "deleted": true,
    }))))
}

/// `POST /v1/admin/battles/{id}/restore` — clear a Battle's
/// soft-delete tombstone. Idempotent.
pub async fn restore_battle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    body: Option<Json<AdminReasonRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    validate_admin_reason(req.reason.as_deref())?;
    let battle_id =
        crate::services::slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let res = sqlx::query("UPDATE battles SET deleted_at = NULL WHERE id = $1")
        .bind(battle_id)
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    if res.rows_affected() == 0 {
        return Err(ApiError::BattleNotFound);
    }
    tracing::info!(
        actor = %actor,
        battle_id = %battle_id,
        reason = ?req.reason.as_deref(),
        "admin.battle.restored"
    );
    Ok(Json(ApiResponse::new(serde_json::json!({
        "battle_id": battle_id,
        "restored": true,
    }))))
}

// ---------------------------------------------------------------------------
// Backfill ALL battle summaries (since 0.21.0)
//
// Re-runs the FULL (non-incremental) `regenerate_summary` + persist
// for every non-deleted battle, so the new refuted/deleted-aware
// summary logic (0.21.0) propagates across the whole corpus without
// waiting for a fresh claim per battle. Sequential — each battle hits
// Gemini, so we don't fan out; a battle with no non-refuted structured
// claim is skipped (its `regenerate_summary` returns a ValidationError
// which we swallow + count). Operator-triggered after deploy.
// ---------------------------------------------------------------------------

/// `POST /v1/admin/battles/backfill-summaries` — regenerate + persist
/// the executive summary for every live Battle using the current
/// (refuted/deleted-aware) logic. Returns
/// `{battle_count, regenerated, skipped, failed}`. Admin-only. This
/// calls Gemini once per battle; it is intentionally sequential and
/// can take minutes on a large corpus — run it deliberately, not on a
/// hot path.
pub async fn backfill_all_summaries(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;

    let battle_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM battles WHERE deleted_at IS NULL ORDER BY created_at")
            .fetch_all(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;

    let total = battle_ids.len();
    let mut regenerated = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    for id in battle_ids {
        match crate::services::battle_summarizer::regenerate_summary(
            &state.pool,
            &state.gemini,
            id,
            None, // full rebuild
        )
        .await
        {
            Ok(summary) => {
                match crate::services::battle_summarizer::persist_summary(
                    &state.pool,
                    id,
                    &summary.markdown,
                    summary.short_markdown.as_deref(),
                )
                .await
                {
                    Ok(()) => regenerated += 1,
                    Err(e) => {
                        failed += 1;
                        tracing::warn!(battle_id = %id, error = ?e, "backfill_summaries.persist_failed");
                    }
                }
            }
            // A battle with no non-refuted structured claim yields a
            // ValidationError ("no Claims with structured data") — that's
            // a skip, not a failure.
            Err(ApiError::ValidationError(_)) => skipped += 1,
            Err(e) => {
                failed += 1;
                tracing::warn!(battle_id = %id, error = ?e, "backfill_summaries.regen_failed");
            }
        }
    }

    tracing::info!(
        actor = %actor,
        battle_count = total,
        regenerated,
        skipped,
        failed,
        "admin.backfill_summaries.complete"
    );

    Ok(Json(ApiResponse::new(serde_json::json!({
        "battle_count": total,
        "regenerated": regenerated,
        "skipped": skipped,
        "failed": failed,
    }))))
}

// ---------------------------------------------------------------------------
