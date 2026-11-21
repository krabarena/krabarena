// --- Claim management endpoints ---

/// `GET /v1/admin/claims` — list Claims for moderation/admin tooling.
pub async fn list_claims_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListClaimsAdminQuery>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;

    let pagination = admin_list_pagination(q.limit, q.offset, q.count_only)?;
    let battle_id = match q.battle.as_deref() {
        Some(raw) => Some(crate::services::slug_resolver::resolve_battle(&state.pool, raw).await?),
        None => None,
    };
    let (author_id, author_email) = normalize_admin_claim_author_filter(q.author.as_deref())?;
    validate_admin_claim_visibility(q.visibility.as_deref())?;
    validate_admin_claim_review_verdict(q.review_verdict.as_deref())?;
    let order_by = admin_order_by(
        q.order_by.as_deref(),
        ADMIN_CLAIM_SORT_FIELDS,
        ADMIN_CLAIM_DEFAULT_SORT,
        AdminSortTerm::asc("id", "c.id"),
    )?;

    // Skip the full-set COUNT(*) on deep pages — see `should_count`.
    let count: i64 = if pagination.should_count() {
        let count_sql = format!("SELECT COUNT(*)::BIGINT FROM ({ADMIN_CLAIM_BASE_QUERY}) filtered");
        sqlx::query_scalar(&count_sql)
            .bind(battle_id)
            .bind(author_id)
            .bind(author_email.as_deref())
            .bind(q.visibility.as_deref())
            .bind(q.created_after)
            .bind(q.created_before)
            .bind(q.review_verdict.as_deref())
            .bind(q.standalone)
            .bind(q.has_warnings)
            .bind(q.without_verifications)
            .bind(q.include_deleted.unwrap_or(false))
            .fetch_one(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    } else {
        ADMIN_COUNT_SKIPPED
    };

    let rows: Vec<AdminClaimRow> = if pagination.count_only {
        Vec::new()
    } else {
        let page_sql = format!("{ADMIN_CLAIM_BASE_QUERY} ORDER BY {order_by} LIMIT $12 OFFSET $13");
        sqlx::query_as(&page_sql)
            .bind(battle_id)
            .bind(author_id)
            .bind(author_email.as_deref())
            .bind(q.visibility.as_deref())
            .bind(q.created_after)
            .bind(q.created_before)
            .bind(q.review_verdict.as_deref())
            .bind(q.standalone)
            .bind(q.has_warnings)
            .bind(q.without_verifications)
            .bind(q.include_deleted.unwrap_or(false))
            .bind(pagination.limit)
            .bind(pagination.offset)
            .fetch_all(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    };

    let claims: Vec<AdminClaimListItem> = rows.into_iter().map(admin_claim_list_item).collect();
    Ok(Json(ApiResponse::new(AdminClaimListResponse {
        items: claims,
        pagination: pagination.response(count),
    })))
}

const ADMIN_CLAIM_SORT_FIELDS: &[AdminSortField] = &[
    AdminSortField {
        public: "created_at",
        sql: "c.created_at",
    },
    AdminSortField {
        public: "updated_at",
        sql: "c.updated_at",
    },
    AdminSortField {
        public: "title",
        sql: "c.claim",
    },
    AdminSortField {
        public: "visibility",
        sql: "c.visibility",
    },
    AdminSortField {
        public: "review_verdict",
        sql: "r.verdict",
    },
    AdminSortField {
        public: "battle_title",
        sql: "b.title",
    },
    AdminSortField {
        public: "author",
        sql: "COALESCE(u.display_name, u.email, u.id::text)",
    },
    AdminSortField {
        public: "id",
        sql: "c.id",
    },
];

const ADMIN_CLAIM_DEFAULT_SORT: &[AdminSortTerm] = &[
    AdminSortTerm::desc("created_at", "c.created_at"),
    AdminSortTerm::asc("id", "c.id"),
];

/// `GET /v1/admin/claims/{claim_id}` — inspect one Claim by UUID, slug, or URL.
pub async fn get_claim_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let claim_id =
        crate::services::slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let claim = load_admin_claim(&state.pool, claim_id).await?;
    Ok(Json(ApiResponse::new(claim)))
}

/// `PATCH /v1/admin/claims/{claim_id}` — edit explicitly supplied Claim fields.
pub async fn update_claim_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    Json(req): Json<UpdateClaimAdminRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;
    validate_admin_reason(req.reason.as_deref())?;
    let claim_id =
        crate::services::slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;

    #[derive(sqlx::FromRow)]
    struct CurrentClaim {
        claim: String,
        battle_id: Option<Uuid>,
        claim_structure: Option<Value>,
        topic: Option<String>,
        topic_tags: Vec<String>,
        #[sqlx(default)]
        produced_by: Option<Vec<String>>,
    }

    let current: CurrentClaim = sqlx::query_as(
        r#"
        SELECT claim, battle_id, claim_structure, topic, topic_tags, produced_by
          FROM claims
         WHERE id = $1
        "#,
    )
    .bind(claim_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .ok_or(ApiError::ClaimNotFound)?;

    let touches_standalone_fields = req.topic.is_some() || req.topic_tags.is_some();
    if current.battle_id.is_some() && touches_standalone_fields {
        return Err(ApiError::ValidationError(
            "topic and topic_tags can be updated only for standalone Claims".into(),
        ));
    }

    let mut changed = false;
    let mut next_claim = req.claim.unwrap_or_else(|| current.claim.clone());
    if next_claim != current.claim {
        changed = true;
    }
    if let Some(title) = req.title.as_deref() {
        next_claim = set_markdown_title(&next_claim, title)?;
        changed = true;
    }
    Field::ClaimsClaim.check(&next_claim)?;

    let next_structure = match req.claim_structure {
        Some(Value::Null) => {
            changed = true;
            None
        }
        Some(value) => {
            validate_claim_structure_json(&value)?;
            if Some(value.clone()) != current.claim_structure {
                changed = true;
            }
            Some(value)
        }
        None => current.claim_structure,
    };

    let next_topic = match req.topic {
        Some(topic) => {
            let normalized = normalize_optional_text("topic", &topic)?;
            if normalized != current.topic {
                changed = true;
            }
            normalized
        }
        None => current.topic,
    };

    let next_topic_tags = match req.topic_tags {
        Some(tags) => {
            let normalized = normalize_topic_tags(tags)?;
            if normalized != current.topic_tags {
                changed = true;
            }
            normalized
        }
        None => current.topic_tags,
    };

    // Producer labels are admin-only (this handler already requires admin
    // via `require_admin`). A provided array is validated against the
    // producer registry + de-duplicated; an empty array clears the label;
    // an absent field leaves it unchanged.
    let next_produced_by: Option<Vec<String>> = match req.produced_by {
        Some(raw) => {
            let mut slugs: Vec<String> = Vec::new();
            for s in &raw {
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
            if current.produced_by.as_deref() != Some(slugs.as_slice()) {
                changed = true;
            }
            Some(slugs)
        }
        None => current.produced_by.clone(),
    };

    if !changed {
        return Err(ApiError::ValidationError(
            "at least one Claim field must change".into(),
        ));
    }

    sqlx::query(
        r#"
        UPDATE claims
           SET claim = $1,
               claim_structure = $2,
               topic = $3,
               topic_tags = $4,
               produced_by = $5,
               updated_at = NOW()
         WHERE id = $6
        "#,
    )
    .bind(&next_claim)
    .bind(&next_structure)
    .bind(next_topic.as_deref())
    .bind(&next_topic_tags)
    .bind(&next_produced_by)
    .bind(claim_id)
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        actor = %actor,
        claim_id = %claim_id,
        reason = ?req.reason.as_deref(),
        "admin.claim.updated"
    );

    let claim = load_admin_claim(&state.pool, claim_id).await?;
    Ok(Json(ApiResponse::new(claim)))
}

const ADMIN_CLAIM_SELECT: &str = r#"
        SELECT c.id,
               c.slug,
               c.claim,
               c.author_id,
               u.display_name AS author_name,
               u.email AS author_email,
               e.domain AS entity_domain,
               c.battle_id,
               b.title AS battle_title,
               b.slug AS battle_slug,
               c.topic,
               c.topic_tags,
               c.visibility,
               c.created_at,
               c.updated_at,
               c.deleted_at,
               c.verify_count,
               c.refute_count,
               c.upvote_count,
               c.downvote_count,
               c.comment_count,
               COALESCE(v.verification_rows, 0)::BIGINT AS verification_rows,
               c.review_id,
               r.verdict AS review_verdict,
               r.summary AS review_summary,
               r.moderation_flags AS review_flags,
               r.created_at AS review_created_at,
               c.artifact_hash,
               c.artifact_url,
               c.artifact_size,
               c.artifact_filename,
               c.claim_structure,
               c.warnings_ignored
          FROM claims c
          LEFT JOIN users u ON u.id = c.author_id
          LEFT JOIN entities e ON e.id = u.entity_id
          LEFT JOIN battles b ON b.id = c.battle_id
          LEFT JOIN claim_reviews r ON r.id = c.review_id
          LEFT JOIN (
                SELECT claim_id, COUNT(*)::BIGINT AS verification_rows
                  FROM verifications
                 GROUP BY claim_id
          ) v ON v.claim_id = c.id
"#;

const ADMIN_CLAIM_BASE_QUERY: &str = r#"
        SELECT c.id,
               c.slug,
               c.claim,
               c.author_id,
               u.display_name AS author_name,
               u.email AS author_email,
               e.domain AS entity_domain,
               c.battle_id,
               b.title AS battle_title,
               b.slug AS battle_slug,
               c.topic,
               c.topic_tags,
               c.visibility,
               c.created_at,
               c.updated_at,
               c.deleted_at,
               c.verify_count,
               c.refute_count,
               c.upvote_count,
               c.downvote_count,
               c.comment_count,
               COALESCE(v.verification_rows, 0)::BIGINT AS verification_rows,
               c.review_id,
               r.verdict AS review_verdict,
               r.summary AS review_summary,
               r.moderation_flags AS review_flags,
               r.created_at AS review_created_at,
               c.artifact_hash,
               c.artifact_url,
               c.artifact_size,
               c.artifact_filename,
               c.claim_structure,
               c.warnings_ignored
          FROM claims c
          LEFT JOIN users u ON u.id = c.author_id
          LEFT JOIN entities e ON e.id = u.entity_id
          LEFT JOIN battles b ON b.id = c.battle_id
          LEFT JOIN claim_reviews r ON r.id = c.review_id
          LEFT JOIN (
                SELECT claim_id, COUNT(*)::BIGINT AS verification_rows
                  FROM verifications
                 GROUP BY claim_id
          ) v ON v.claim_id = c.id
         WHERE ($1::uuid IS NULL OR c.battle_id = $1)
           AND ($2::uuid IS NULL OR c.author_id = $2)
           AND ($3::text IS NULL OR u.email = $3)
           AND ($4::text IS NULL OR c.visibility = $4)
           AND ($5::timestamptz IS NULL OR c.created_at >= $5)
           AND ($6::timestamptz IS NULL OR c.created_at <= $6)
           AND ($7::text IS NULL OR r.verdict = $7)
           AND ($8::bool IS NULL OR ((c.battle_id IS NULL) = $8))
           AND ($9::bool IS NULL OR ((c.warnings_ignored OR COALESCE(jsonb_array_length(r.moderation_flags), 0) > 0) = $9))
           AND ($10::bool IS NULL OR (NOT $10 OR COALESCE(v.verification_rows, 0) = 0))
           AND ($11::bool OR c.deleted_at IS NULL)
"#;

async fn load_admin_claim(
    pool: &sqlx::PgPool,
    claim_id: Uuid,
) -> Result<AdminClaimInspectResponse, ApiError> {
    let sql = format!("{ADMIN_CLAIM_SELECT} WHERE c.id = $1");
    let row: AdminClaimRow = sqlx::query_as(&sql)
        .bind(claim_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::ClaimNotFound)?;
    Ok(admin_claim_inspect_response(row))
}

fn normalize_admin_claim_author_filter(
    raw: Option<&str>,
) -> Result<(Option<Uuid>, Option<String>), ApiError> {
    let Some(raw) = raw else {
        return Ok((None, None));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok((None, None));
    }
    if let Ok(uuid) = trimmed.parse::<Uuid>() {
        Ok((Some(uuid), None))
    } else {
        Ok((None, Some(trimmed.to_string())))
    }
}

fn validate_admin_claim_visibility(visibility: Option<&str>) -> Result<(), ApiError> {
    if let Some(value) = visibility
        && !["public", "private", "adopted"].contains(&value)
    {
        return Err(ApiError::ValidationError(
            "visibility must be one of public, private, adopted".into(),
        ));
    }
    Ok(())
}

fn validate_admin_claim_review_verdict(verdict: Option<&str>) -> Result<(), ApiError> {
    if let Some(value) = verdict
        && !["pass", "warn", "block"].contains(&value)
    {
        return Err(ApiError::ValidationError(
            "review_verdict must be one of pass, warn, block".into(),
        ));
    }
    Ok(())
}

const MAX_RECHECK_REASON_LEN: usize = 500;

fn validate_admin_reason(reason: Option<&str>) -> Result<(), ApiError> {
    if let Some(reason) = reason {
        let trimmed = reason.trim();
        if trimmed.is_empty() {
            return Err(ApiError::ValidationError(
                "reason must not be empty".into(),
            ));
        }
        if trimmed.chars().count() > MAX_RECHECK_REASON_LEN {
            return Err(ApiError::ValidationError(format!(
                "reason must be ≤{MAX_RECHECK_REASON_LEN} chars"
            )));
        }
    }
    Ok(())
}

fn validate_claim_structure_json(value: &Value) -> Result<(), ApiError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(ApiError::ValidationError(
            "claim_structure must be a JSON object or null".into(),
        ))
    }
}

fn normalize_optional_text(field: &str, value: &str) -> Result<Option<String>, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::ValidationError(format!(
            "{field} must not be empty"
        )));
    }
    Ok(Some(trimmed.to_string()))
}

fn normalize_topic_tags(tags: Vec<String>) -> Result<Vec<String>, ApiError> {
    let mut normalized = Vec::with_capacity(tags.len());
    for tag in tags {
        let trimmed = tag.trim();
        if trimmed.is_empty() {
            return Err(ApiError::ValidationError(
                "topic_tags must not contain empty strings".into(),
            ));
        }
        normalized.push(trimmed.to_string());
    }
    Ok(normalized)
}

fn set_markdown_title(markdown: &str, title: &str) -> Result<String, ApiError> {
    let title = title.trim();
    if title.is_empty() || title.contains('\n') || title.contains('\r') {
        return Err(ApiError::ValidationError(
            "title must be a single non-empty line".into(),
        ));
    }

    let mut replaced = false;
    let mut lines = Vec::new();
    for line in markdown.lines() {
        if !replaced && line.starts_with("# ") {
            lines.push(format!("# {title}"));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if replaced {
        let mut next = lines.join("\n");
        if markdown.ends_with('\n') {
            next.push('\n');
        }
        Ok(next)
    } else if markdown.trim().is_empty() {
        Ok(format!("# {title}\n"))
    } else {
        Ok(format!("# {title}\n\n{markdown}"))
    }
}

fn extract_claim_title(markdown: &str, fallback: Option<&str>) -> String {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .or_else(|| {
            markdown
                .lines()
                .find(|line| !line.trim().is_empty())
                .map(str::trim)
        })
        .or(fallback)
        .unwrap_or("(untitled Claim)")
        .to_string()
}

fn claim_preview(markdown: &str) -> String {
    markdown
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with('|')
                || trimmed.starts_with("```")
            {
                None
            } else {
                Some(trimmed)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(280)
        .collect()
}

fn claim_sections(markdown: &str) -> Value {
    let mut summary = None;
    let mut methodology = None;
    let mut conclusion = None;
    let mut current: Option<&str> = None;
    let mut buffer = String::new();

    let flush = |target: Option<&str>,
                 buffer: &mut String,
                 summary: &mut Option<String>,
                 methodology: &mut Option<String>,
                 conclusion: &mut Option<String>| {
        let text = buffer.trim();
        if text.is_empty() {
            buffer.clear();
            return;
        }
        let snippet: String = text.chars().take(300).collect();
        match target {
            Some("summary") => *summary = Some(snippet),
            Some("methodology") => *methodology = Some(snippet),
            Some("conclusion") => *conclusion = Some(snippet),
            _ => {}
        }
        buffer.clear();
    };

    for line in markdown.lines() {
        let lower = line.trim().to_ascii_lowercase();
        let next = if lower == "## summary" {
            Some("summary")
        } else if lower == "## methodology" {
            Some("methodology")
        } else if lower == "## conclusion" {
            Some("conclusion")
        } else if lower.starts_with("## ") {
            Some("other")
        } else {
            current
        };

        if next != current && line.trim_start().starts_with("## ") {
            flush(
                current,
                &mut buffer,
                &mut summary,
                &mut methodology,
                &mut conclusion,
            );
            current = next;
            continue;
        }

        if matches!(current, Some("summary" | "methodology" | "conclusion")) {
            buffer.push_str(line);
            buffer.push('\n');
        }
    }
    flush(
        current,
        &mut buffer,
        &mut summary,
        &mut methodology,
        &mut conclusion,
    );

    serde_json::json!({
        "summary": summary,
        "methodology": methodology,
        "conclusion": conclusion,
    })
}

fn admin_claim_list_item(row: AdminClaimRow) -> AdminClaimListItem {
    let title = extract_claim_title(&row.claim, row.topic.as_deref());
    let url = claim_browser_url(row.id, row.slug.as_deref());
    let battle = row.battle_id.map(|battle_id| AdminClaimBattle {
        battle_id,
        title: row.battle_title.clone(),
        slug: row.battle_slug.clone(),
        url: battle_browser_url(battle_id, row.battle_slug.as_deref()),
    });
    AdminClaimListItem {
        id: row.id,
        slug: row.slug,
        title,
        preview: claim_preview(&row.claim),
        author: AdminClaimAuthor {
            user_id: row.author_id,
            display_name: row.author_name,
            email: row.author_email,
            entity_domain: row.entity_domain,
        },
        battle,
        topic: row.topic,
        topic_tags: row.topic_tags,
        visibility: row.visibility,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
        counts: AdminClaimCounts {
            verify_count: row.verify_count,
            refute_count: row.refute_count,
            upvote_count: row.upvote_count,
            downvote_count: row.downvote_count,
            comment_count: row.comment_count,
            verification_rows: row.verification_rows,
        },
        latest_review: row.review_id.map(|review_id| AdminClaimReviewSummary {
            review_id,
            verdict: row.review_verdict,
            summary: row.review_summary,
            moderation_flags: row.review_flags.unwrap_or_else(|| serde_json::json!([])),
            created_at: row.review_created_at,
        }),
        url,
    }
}

fn admin_claim_inspect_response(row: AdminClaimRow) -> AdminClaimInspectResponse {
    let warnings = admin_claim_warnings(&row);
    let risk_signals = admin_claim_risk_signals(&row);
    let artifact = serde_json::json!({
        "hash": row.artifact_hash,
        "url": row.artifact_url,
        "size": row.artifact_size,
        "filename": row.artifact_filename,
    });
    let sections = claim_sections(&row.claim);
    AdminClaimInspectResponse {
        item: admin_claim_list_item(row.clone()),
        claim: row.claim,
        sections,
        artifact,
        claim_structure: row.claim_structure,
        warnings,
        risk_signals,
    }
}

fn claim_browser_url(id: Uuid, slug: Option<&str>) -> String {
    let key = slug.map(str::to_owned).unwrap_or_else(|| id.to_string());
    format!("https://krabarena.com/claims/{key}")
}

fn battle_browser_url(id: Uuid, slug: Option<&str>) -> String {
    let key = slug.map(str::to_owned).unwrap_or_else(|| id.to_string());
    format!("https://krabarena.com/battles/{key}")
}

fn admin_claim_warnings(row: &AdminClaimRow) -> Vec<String> {
    let mut warnings = Vec::new();
    if row.warnings_ignored {
        warnings.push("review warnings were ignored during publish".to_string());
    }
    if row.review_flags.as_ref().is_some_and(|flags| {
        flags
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(false)
    }) {
        warnings.push("latest review has moderation flags".to_string());
    }
    if row.deleted_at.is_some() {
        warnings.push("claim is currently soft-deleted".to_string());
    }
    warnings
}

fn admin_claim_risk_signals(row: &AdminClaimRow) -> Vec<String> {
    let mut signals = Vec::new();
    if row.battle_id.is_some() {
        signals.push("battle-attached Claim: visibility cannot be changed through promote".into());
    }
    if row.verification_rows > 0
        || row.comment_count > 0
        || row.upvote_count > 0
        || row.downvote_count > 0
    {
        signals.push(
            "Claim has user engagement; moderation changes may affect existing activity".into(),
        );
    }
    signals
}

// Solutions registry endpoints removed in 0.12.0 — per-row competitor
// logos now live inline in each claim's `structure.json.leaderboard[i].logo_url`.
// Migration 036 drops the `solutions` table. See CHANGELOG.md.

// ---------------------------------------------------------------------------
// Battle-owned solutions backfill (since 0.13.0)
//
// Walks every claim attached to a Battle, extracts the distinct
// `leaderboard[i].solution` strings, normalises each (lowercase +
// trim + strip-parens + collapse-whitespace), and writes a curated
// `[{name, name_normalized, logo_url, website_url}]` JSONB to
// `battles.solutions`. The vendor map lives in
// `services::solution_names::VENDOR_PATTERNS` — first-match-wins,
// regex against the ORIGINAL pre-normalised string. Solutions
// that don't match any pattern still land in the array (so editors
// see them in the row) but with `logo_url = None`.
//
// Idempotent: existing entries are preserved (only NEW normalised
// keys are appended). Re-running the backfill is a no-op — with one
// exception (since 0.14.5): for every existing entry whose `name`
// the vendor map now resolves, `logo_url` + `website_url` are
// rewritten to match the parser's current answer — including when
// the stored URL was editor-pinned. The "preserve editor overrides"
// guarantee that shipped in 0.13.2 is gone: the parser update that
// motivated this PR (target-vendor-in-parens precedence) IS the
// curation update — we want `BrightData (Google)` to flip from
// brightdata.com → google.com everywhere, even where an editor had
// pinned the proxy by hand. Truly unknown vendors (vendor map
// returns None) are still left untouched.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct ClaimRecheckRequest {
    /// Optional free-text audit note. Logged via `tracing::info!` for
    /// post-hoc grep; deliberately NOT transmitted in the webhook
    /// payload (subscribers shouldn't have to handle admin-only
    /// metadata, and the recheck is supposed to look identical to a
    /// fresh post on the wire).
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /v1/admin/claims/{claim_id}/recheck` — re-fire the
/// `claim.posted` webhook event for a specific Claim so KrabChecker
/// (or any other active subscriber) re-runs its workflow against it.
///
/// Path: UUID, slug, or full `krabarena.com/claims/...` URL.
/// Body: `{"reason": "<text ≤500 chars>"}` (optional).
/// Response: `{"delivered": true, "claim_id": "<uuid>", "subscriber_count": N}`.
///
/// Returns 403 if the caller is not a platform admin, 404 if the
/// Claim doesn't exist, 400 if `reason` is over 500 chars.
pub async fn recheck_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    body: Option<Json<ClaimRecheckRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let actor_id = require_admin(&headers, &state.jwt_secret)?;

    let req = body.map(|Json(b)| b).unwrap_or_default();
    validate_recheck_reason(req.reason.as_deref())?;

    let claim_id =
        crate::services::slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let claim = crate::services::claim_service::get_claim(&state.pool, claim_id).await?;

    // Count active subs synchronously so the response can say how many
    // downstream receivers got pinged. The actual dispatch is still
    // spawn-internal (so admin's response isn't blocked by subscriber
    // latency); a sub that flips active between the count and the
    // dispatch's own lookup rounds to the same window — both queries
    // run against the same `webhook_subscriptions` table.
    // Private claims are only re-announced to their author's / admins'
    // subscriptions (same audience as the API read).
    let audience = crate::services::webhook_dispatch::Audience::for_claim(
        Some(&claim.visibility),
        claim.author_id,
    );
    let subscriber_count = crate::services::webhook_dispatch::count_subscriptions_for_event(
        &state.pool,
        krabarena_shared::webhook_events::CLAIM_POSTED,
        audience,
    )
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        target: "admin_claim_recheck",
        actor = %actor_id,
        claim = %claim.id,
        reason = ?req.reason.as_deref(),
        "admin_claim_recheck"
    );

    crate::services::webhook_dispatch::dispatch_event_to(
        state.pool.clone(),
        krabarena_shared::webhook_events::CLAIM_POSTED,
        crate::routes::claims::build_claim_webhook_payload(&claim),
        audience,
    );

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "delivered": true,
            "claim_id": claim.id.to_string(),
            "subscriber_count": subscriber_count,
        }))),
    ))
}

/// `POST /v1/admin/claims/{claim_id}/regenerate-presentation` — backfill
/// path for the reviewer-generated viral-share layer (`og_summary` +
/// `presentation`) on a Claim that predates the feature, or whose
/// presentation an operator wants to refresh without paying for a
/// full re-review.
///
/// Runs a single Gemini call via
/// `services::presentation_gen::generate_presentation_for_claim` (NOT
/// the agentic review loop — no moderation, no artifact re-download).
/// That function already runs the result through
/// `presentation_check::sanitize_presentation`, so a hallucinated
/// numeral silently drops the `presentation` block rather than
/// failing the request.
///
/// Path: UUID, slug, or full `krabarena.com/claims/...` URL. Admin-
/// gated exactly like `routes::battles::regenerate_og`. Loads the
/// claim via the explicit `CLAIM_COLUMNS` list (never `SELECT *` —
/// see the cached-plan trap note on that constant).
///
/// On success: persists `og_summary` + `presentation` to `claims`,
/// warms the claim's OG card in the background (fire-and-forget —
/// never blocks or fails this response), and returns
/// `{og_summary, presentation}`.
///
/// Returns 403 if the caller is not a platform admin, 404 if the
/// Claim doesn't exist (or is soft-deleted), 500 (via `ApiError::Internal`)
/// if the Gemini call itself fails (e.g. `GEMINI_API_KEY` unset,
/// transport error, malformed model output).
pub async fn regenerate_presentation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let actor_id = require_admin(&headers, &state.jwt_secret)?;

    let claim_id =
        crate::services::slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;

    let by_id = format!(
        "SELECT {} FROM claims WHERE id = $1 AND deleted_at IS NULL",
        crate::models::claim::CLAIM_COLUMNS
    );
    let claim = sqlx::query_as::<_, crate::models::claim::ClaimRow>(&by_id)
        .bind(claim_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::ClaimNotFound)?;

    let (og_summary, presentation) =
        crate::services::presentation_gen::generate_presentation_for_claim(&state, &claim).await?;

    let presentation_json = presentation
        .as_ref()
        .and_then(|p| serde_json::to_value(p).ok());

    sqlx::query(
        "UPDATE claims SET og_summary = $2, presentation = $3, updated_at = now() WHERE id = $1",
    )
    .bind(claim.id)
    .bind(og_summary.as_deref())
    .bind(&presentation_json)
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    tracing::info!(
        target: "admin_claim_regenerate_presentation",
        actor = %actor_id,
        claim = %claim.id,
        has_og_summary = og_summary.is_some(),
        has_presentation = presentation.is_some(),
        "admin_claim_regenerate_presentation"
    );

    // Fire-and-forget: refresh the cached OG card so the social
    // preview picks up the new presentation without waiting for the
    // next scrape.
    crate::routes::og::spawn_warm_claim_og_by_id(&state, claim.id);

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "og_summary": og_summary,
            "presentation": presentation,
        }))),
    ))
}

/// Validate the optional audit reason. Returns `ValidationError` when
/// the reason exceeds `MAX_RECHECK_REASON_LEN`; otherwise `Ok(())`
/// (empty / `None` is allowed — the note is for admin convenience,
/// not a required field).
fn validate_recheck_reason(reason: Option<&str>) -> Result<(), ApiError> {
    if let Some(r) = reason {
        let trimmed = r.trim();
        if trimmed.is_empty() {
            return Err(ApiError::ValidationError(
                "reason must not be empty".into(),
            ));
        }
        if trimmed.chars().count() > MAX_RECHECK_REASON_LEN {
            return Err(ApiError::ValidationError(format!(
                "reason must be ≤{MAX_RECHECK_REASON_LEN} chars"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin soft-delete + restore for Battles and Claims (since 0.21.0)
//
// Reversible moderation tool. `DELETE` sets `deleted_at = now()` and
// the row drops out of every public read path (home feed, lists,
// detail → 404, search, leaderboards, summary inputs). `restore` sets
// it back to NULL. Unlike the author-facing `delete_claim` (which
// REFUSES when a claim has verifications/comments/votes), admin
// soft-delete always succeeds — the row + its engagement stay in the
// DB, only the public surface hides it. Migration 053 adds the column
// + partial indexes.
//
// Auth: platform `users.role = 'admin'` (require_admin) — moderation,
// not org management. Path accepts UUID, slug, or full URL via the
// slug_resolver (so an admin can paste the page URL straight from a
// moderation ping). Note the resolver matches by UUID/slug regardless
// of `deleted_at`, so `restore` can find an already-soft-deleted row.
// ---------------------------------------------------------------------------

/// `DELETE /v1/admin/claims/{id}` — soft-delete a Claim. Idempotent:
/// re-deleting an already-deleted claim is a no-op success (the
/// `deleted_at IS NULL` guard means the UPDATE touches 0 rows but we
/// still return 200 — the desired end state holds).
pub async fn soft_delete_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    body: Option<Json<AdminReasonRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    validate_admin_reason(req.reason.as_deref())?;
    let claim_id =
        crate::services::slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let res =
        sqlx::query("UPDATE claims SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL")
            .bind(claim_id)
            .execute(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    tracing::info!(
        actor = %actor,
        claim_id = %claim_id,
        changed = res.rows_affected(),
        reason = ?req.reason.as_deref(),
        "admin.claim.soft_deleted"
    );
    Ok(Json(ApiResponse::new(serde_json::json!({
        "claim_id": claim_id,
        "deleted": true,
    }))))
}

/// `POST /v1/admin/claims/{id}/restore` — clear a Claim's soft-delete
/// tombstone. Idempotent.
pub async fn restore_claim(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(claim_id_raw): Path<String>,
    body: Option<Json<AdminReasonRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = require_admin(&headers, &state.jwt_secret)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    validate_admin_reason(req.reason.as_deref())?;
    let claim_id =
        crate::services::slug_resolver::resolve_claim(&state.pool, &claim_id_raw).await?;
    let res = sqlx::query("UPDATE claims SET deleted_at = NULL WHERE id = $1")
        .bind(claim_id)
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    if res.rows_affected() == 0 {
        return Err(ApiError::ClaimNotFound);
    }
    tracing::info!(
        actor = %actor,
        claim_id = %claim_id,
        reason = ?req.reason.as_deref(),
        "admin.claim.restored"
    );
    Ok(Json(ApiResponse::new(serde_json::json!({
        "claim_id": claim_id,
        "restored": true,
    }))))
}
