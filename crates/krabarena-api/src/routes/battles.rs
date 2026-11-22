use axum::Json;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use crate::AppState;
use crate::envelope::{ApiResponse, PaginatedResponse, ResponseMeta};
use crate::error::ApiError;
use crate::models::battle::{CreateBattleRequest, ListBattlesQuery, UpdateBattleRequest};
use crate::routes::auth::{extract_user_id, require_editor};
use crate::services::{battle_service, image_service, slug_resolver};

pub async fn create_battle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateBattleRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // Only editors and admins may open a Battle. This is the structural
    // enforcement of "curation creates density" from the PRFAQ: the
    // community may suggest Battles, but only the editorial team opens one.
    let user_id = require_editor(&headers, &state.jwt_secret)?;
    let battle = battle_service::create_battle(&state.pool, user_id, req).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::new(battle))))
}

pub async fn update_battle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    Json(req): Json<UpdateBattleRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let battle = battle_service::update_battle(&state.pool, battle_id, user_id, req).await?;
    Ok(Json(ApiResponse::new(battle)))
}

#[utoipa::path(
    get,
    path = "/v1/battles/{id}",
    tag = "battles",
    params(("id" = String, Path, description = "Battle UUID or slug")),
    responses(
        (status = 200, description = "Battle found"),
        (status = 404, description = "Battle not found"),
    ),
)]
pub async fn get_battle(
    State(state): State<AppState>,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let battle = battle_service::get_battle(&state.pool, battle_id).await?;
    Ok(Json(ApiResponse::new(battle)))
}

#[utoipa::path(
    get,
    path = "/v1/battles",
    tag = "battles",
    params(
        ("status" = Option<String>, Query, description = "Filter by status (open|active|settled)"),
        ("domain" = Option<String>, Query, description = "Filter by domain tag"),
        ("limit"  = Option<i64>,    Query, description = "Page size (default 20)"),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from pagination.next_cursor"),
    ),
    responses((status = 200, description = "Battle list page")),
)]
pub async fn list_battles(
    State(state): State<AppState>,
    Query(query): Query<ListBattlesQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = query.limit.unwrap_or(20);
    let status = battle_service::normalize_status_filter(query.status.as_deref())?;
    let (items, pagination) = battle_service::list_battles(
        &state.pool,
        status.as_deref(),
        query.domain.as_deref(),
        query.cursor.as_deref(),
        limit,
    )
    .await?;

    Ok(Json(PaginatedResponse {
        data: items,
        pagination,
        meta: ResponseMeta::generate(),
    }))
}

pub async fn list_sub_battles(
    State(state): State<AppState>,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let items = battle_service::list_sub_battles(&state.pool, battle_id).await?;
    Ok(Json(ApiResponse::new(items)))
}

// --- Proxy spec management ---

use crate::models::proxy::ProxySpec;
use crate::services::battle_review_service;
use crate::services::proxy_service;

/// PUT /v1/battles/{battle_id}/proxy-spec — set proxy config (battle author or admin).
///
/// Not editor-wide: the spec holds sponsors' API keys, and an editor of
/// one Battle must not be able to read or repoint another sponsor's
/// upstream (security review 2026-11-19).
pub async fn set_proxy_spec(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    Json(spec): Json<ProxySpec>,
) -> Result<impl IntoResponse, ApiError> {
    require_editor(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    require_battle_author_or_admin(&state, &headers, battle_id).await?;

    if spec.services.is_empty() {
        return Err(ApiError::ValidationError(
            "proxy spec must have at least one service".into(),
        ));
    }
    for (name, svc) in &spec.services {
        if svc.upstream.is_empty() {
            return Err(ApiError::ValidationError(format!(
                "service '{}' has no upstream URL",
                name
            )));
        }
    }
    spec.validate_billing().map_err(ApiError::ValidationError)?;
    // Same full round-trip check as the create / PATCH paths (https,
    // public upstream host, docs_url, aws_sigv4 shape, ...).
    let raw = serde_json::to_value(&spec).map_err(|e| ApiError::Internal(e.into()))?;
    ProxySpec::validate(&raw).map_err(ApiError::ValidationError)?;

    proxy_service::set_proxy_spec(&state.pool, battle_id, &spec).await?;

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "message": "proxy spec saved",
            "services": spec.services.keys().collect::<Vec<_>>(),
        }))),
    ))
}

/// GET /v1/battles/{battle_id}/proxy-spec — read proxy config with secrets (battle author or admin)
pub async fn get_proxy_spec(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_editor(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    require_battle_author_or_admin(&state, &headers, battle_id).await?;
    let spec = proxy_service::get_proxy_spec(&state.pool, battle_id).await?;
    Ok(Json(ApiResponse::new(spec)))
}

/// GET /v1/battles/{battle_id}/proxy-info — public info (no secrets)
pub async fn get_proxy_info(
    State(state): State<AppState>,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let info = proxy_service::get_proxy_info(&state.pool, battle_id).await?;
    Ok(Json(ApiResponse::new(info)))
}

/// POST /v1/battles/{battle_id}/review — review battle quality + validate proxy spec
pub async fn review_battle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_editor(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;

    let battle = battle_service::get_battle(&state.pool, battle_id).await?;
    let spec = proxy_service::get_proxy_spec(&state.pool, battle_id)
        .await
        .ok();

    let result = battle_review_service::review_battle(
        &state.gemini,
        &battle.title,
        &battle.description,
        &battle.domain_tags,
        spec.as_ref(),
    )
    .await?;

    Ok(Json(ApiResponse::new(result)))
}

/// Query for `POST /v1/battles/{id}/regenerate-summary`.
///
/// - `dry_run=true` returns the new markdown without persisting;
///   anything else (or missing) commits to the Battle and appends a
///   `battle_summary_history` row.
/// - `include_prompt=true` includes the full `system_prompt` and
///   `user_message` in the response. This is the GEPA-style trace —
///   the (input, output) pair a downstream prompt-iteration tool
///   needs to evaluate prompt changes against past data. Off by
///   default because the user message can run 50-100 KB on
///   battles with 10 claims of structure + README.
#[derive(serde::Deserialize, Default)]
pub struct RegenerateSummaryQuery {
    #[serde(default)]
    pub dry_run: Option<bool>,
    #[serde(default)]
    pub include_prompt: Option<bool>,
}

/// `POST /v1/battles/{battle_id}/regenerate-summary` — re-synthesize
/// the executive summary from every Claim's `claim_structure`.
/// Available to the Battle author and admins only — this endpoint
/// can rewrite the running editorial, so it's gated tighter than
/// the public read paths but more openly than `editor`-only
/// operations (the original Battle creator should be able to refresh
/// their own summary without escalating to editor / admin).
pub async fn regenerate_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    Query(query): Query<RegenerateSummaryQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let (caller_id, role) =
        crate::routes::auth::extract_user_with_role(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;

    if role != "admin" {
        // Author check — re-fetch to confirm the caller is the
        // Battle's author. Admins skip the check.
        let row: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT author_id FROM battles WHERE id = $1")
                .bind(battle_id)
                .fetch_optional(&state.pool)
                .await
                .map_err(|e| ApiError::Internal(e.into()))?;
        let author_id = row.ok_or(ApiError::BattleNotFound)?.0;
        if author_id != caller_id {
            return Err(ApiError::Forbidden);
        }
    }

    // /regenerate-summary is the explicit "rebuild from scratch"
    // path — pass `None` to load all top-10 claims, not just one.
    let regenerated = crate::services::battle_summarizer::regenerate_summary(
        &state.pool,
        &state.gemini,
        battle_id,
        None,
    )
    .await?;

    let dry_run = query.dry_run.unwrap_or(false);
    if !dry_run {
        crate::services::battle_summarizer::persist_summary(
            &state.pool,
            battle_id,
            &regenerated.markdown,
            regenerated.short_markdown.as_deref(),
        )
        .await?;
        // Eager OG cache-warm: the summary just changed, so the
        // battle card's content hash changed. Proactively re-render +
        // re-cache so `/og/battles/{id}` serves the fresh card
        // immediately instead of waiting for the next scraper to hit a
        // lazy cache miss. Fire-and-forget — no-op when OG_RENDER_URL
        // is unset, never blocks or fails the response.
        crate::routes::og::spawn_warm_battle_og_by_id(&state, battle_id);
    }

    let mut body = serde_json::json!({
        "summary": regenerated.markdown,
        "short_summary": regenerated.short_markdown,
        "claims_with_structure": regenerated.claims_with_structure,
        "previous_summary": regenerated.previous_summary,
        "dry_run": dry_run,
        "persisted": !dry_run,
    });
    if query.include_prompt.unwrap_or(false) {
        body["system_prompt"] = serde_json::json!(regenerated.system_prompt);
        body["user_message"] = serde_json::json!(regenerated.user_message);
    }
    Ok(Json(ApiResponse::new(body)))
}

/// Hard cap on freeform `presentation_guidance` length (chars). Long
/// enough for a paragraph or two of agent steering, short enough that it
/// can't bloat the summary prompt or a battle row.
const MAX_PRESENTATION_GUIDANCE_CHARS: usize = 4000;

/// `PUT /v1/battles/{battle_id}/presentation` body. Full-replace
/// semantics: each field is written verbatim, so `null` *clears* that
/// setting (reverts to the house default), and omitting a field also
/// clears it. Read the current values with `GET` first if you want to
/// preserve one while changing the other.
#[derive(Debug, serde::Deserialize)]
pub struct SetPresentationRequest {
    /// Freeform natural-language steering for the summary agents.
    /// `null` ⇒ revert to the house-default guidance.
    #[serde(default)]
    pub guidance: Option<String>,
    /// Structured knobs. Must deserialize into `PresentationConfig`
    /// (unknown keys / wrong types are rejected with 400). A partial
    /// object is fine — omitted knobs fall back to defaults at read
    /// time. `null` ⇒ revert every knob to the default.
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

/// Confirm the caller may edit this battle's presentation: admins always,
/// otherwise the battle's own author. Returns `BattleNotFound` for a
/// missing battle (so the endpoint doesn't leak existence to non-authors
/// any more than the public battle page already does).
async fn require_battle_author_or_admin(
    state: &AppState,
    headers: &HeaderMap,
    battle_id: uuid::Uuid,
) -> Result<(), ApiError> {
    let (caller_id, role) =
        crate::routes::auth::extract_user_with_role(headers, &state.jwt_secret)?;
    if role == "admin" {
        return Ok(());
    }
    let row: Option<(uuid::Uuid,)> = sqlx::query_as("SELECT author_id FROM battles WHERE id = $1")
        .bind(battle_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let author_id = row.ok_or(ApiError::BattleNotFound)?.0;
    if author_id != caller_id {
        return Err(ApiError::Forbidden);
    }
    Ok(())
}

/// `GET /v1/battles/{battle_id}/presentation` — the battle's stored
/// presentation guidance + knobs, plus the *resolved* knobs (raw config
/// merged over the house defaults) and the defaults themselves, so an
/// author UI can show "effective" values without re-implementing the
/// merge. Author-or-admin gated.
pub async fn get_presentation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    require_battle_author_or_admin(&state, &headers, battle_id).await?;

    let row: Option<(Option<String>, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT presentation_guidance, presentation_config FROM battles WHERE id = $1",
    )
    .bind(battle_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    let (guidance, config_json) = row.ok_or(ApiError::BattleNotFound)?;
    let resolved =
        krabarena_shared::presentation::PresentationConfig::from_json(config_json.as_ref());

    Ok(Json(ApiResponse::new(serde_json::json!({
        "guidance": guidance,
        "config": config_json,
        "resolved_config": resolved,
        "default_config": krabarena_shared::presentation::PresentationConfig::default(),
    }))))
}

/// `PUT /v1/battles/{battle_id}/presentation` — set the battle's freeform
/// guidance + structured knobs (full-replace; see [`SetPresentationRequest`]).
/// Author-or-admin gated. Validates the guidance length and that the
/// config blob deserializes into `PresentationConfig` (so a typo'd knob
/// is a 400, not a silently-ignored override at render time).
pub async fn set_presentation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    Json(req): Json<SetPresentationRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    require_battle_author_or_admin(&state, &headers, battle_id).await?;

    if let Some(g) = &req.guidance {
        let n = g.chars().count();
        if n > MAX_PRESENTATION_GUIDANCE_CHARS {
            return Err(ApiError::ValidationError(format!(
                "presentation_guidance is {n} chars; max {MAX_PRESENTATION_GUIDANCE_CHARS}"
            )));
        }
    }
    if let Some(c) = &req.config {
        // Reject unknown keys / wrong types up front (the struct is
        // `deny_unknown_fields`) so a typo can't masquerade as a no-op.
        serde_json::from_value::<krabarena_shared::presentation::PresentationConfig>(c.clone())
            .map_err(|e| ApiError::ValidationError(format!("invalid presentation_config: {e}")))?;
    }

    sqlx::query(
        "UPDATE battles SET presentation_guidance = $1, presentation_config = $2 WHERE id = $3",
    )
    .bind(&req.guidance)
    .bind(&req.config)
    .bind(battle_id)
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let resolved =
        krabarena_shared::presentation::PresentationConfig::from_json(req.config.as_ref());
    Ok(Json(ApiResponse::new(serde_json::json!({
        "guidance": req.guidance,
        "config": req.config,
        "resolved_config": resolved,
    }))))
}

/// `POST /v1/battles/{battle_id}/relabel` — (re)generate the crisp
/// benchmark `short_label` for every live claim on the battle, via the
/// Gemini labeler, steered by the battle's `presentation_guidance` when
/// set. Persists the labels onto each claim's `claim_structure`; the
/// Claims Wins matrix reads them on the next render. Author-or-admin
/// gated. Warms the battle OG card afterward since the labels feed it.
pub async fn relabel_battle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    require_battle_author_or_admin(&state, &headers, battle_id).await?;

    let guidance: Option<(Option<String>,)> =
        sqlx::query_as("SELECT presentation_guidance FROM battles WHERE id = $1")
            .bind(battle_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
    let guidance = guidance.and_then(|r| r.0);

    let outcome = crate::services::benchmark_labeler::relabel_battle(
        &state.pool,
        &state.gemini,
        battle_id,
        guidance.as_deref(),
    )
    .await?;

    // Labels feed the battle's OG card — warm it so the social preview
    // picks up the fresh column names. Fire-and-forget.
    crate::routes::og::spawn_warm_battle_og_by_id(&state, battle_id);

    Ok(Json(ApiResponse::new(outcome)))
}

/// `POST /v1/battles/{battle_id}/regenerate-og` — eagerly re-render +
/// re-cache the dynamic OG card for a battle AND each of its live
/// (non-deleted, non-superseded) claims, under the exact GCS key the
/// `/og/...` serve path reads. The on-demand counterpart to the
/// automatic warm that fires after a summary regeneration: useful when
/// an operator wants the social cards refreshed without waiting for a
/// summary change or a scraper re-fetch.
///
/// Admin-gated (`require_admin`). Gated on `OG_RENDER_URL`: when the
/// render service is not configured there's nothing to warm, so we
/// return a clear `{enabled:false}` message rather than silently
/// no-op. Each render is sequential and best-effort — a single failure
/// is counted in `failed` and does not abort the rest.
///
/// Response: `{ battle, claims_warmed, failed }`.
pub async fn regenerate_og(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    crate::routes::auth::require_admin(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;

    // No render service → nothing to warm. Be explicit so the operator
    // knows the OG_RENDER_URL env isn't set on this instance.
    if std::env::var("OG_RENDER_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Ok(Json(ApiResponse::new(serde_json::json!({
            "enabled": false,
            "message": "OG_RENDER_URL is not set on this instance; nothing to warm.",
            "battle": false,
            "claims_warmed": 0,
            "failed": 0,
        }))));
    }

    // Fetch the battle row (the warm path needs the full row to build
    // the card + compute the leaderboard cache key).
    let by_id = format!(
        "SELECT {} FROM battles WHERE id = $1 AND deleted_at IS NULL",
        crate::models::battle::BATTLE_COLUMNS
    );
    let battle = sqlx::query_as::<_, crate::models::battle::BattleRow>(&by_id)
        .bind(battle_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .ok_or(ApiError::BattleNotFound)?;

    let battle_warmed = crate::routes::og::warm_battle_og(&state, &battle).await;

    // Live claims for this battle: non-deleted, and non-superseded
    // (a claim is superseded when a newer, non-deleted claim names it
    // as its `predecessor_id`). Explicit CLAIM_COLUMNS — cached-plan
    // trap (see AGENTS.md). The unqualified column list is unambiguous:
    // the EXISTS subquery aliases its own `claims s`, so the outer
    // `claims c` columns resolve cleanly.
    let claims_query = format!(
        "SELECT {cols} FROM claims c \
         WHERE c.battle_id = $1 AND c.deleted_at IS NULL \
         AND c.visibility IS DISTINCT FROM 'private' \
         AND NOT EXISTS ( \
             SELECT 1 FROM claims s \
             WHERE s.predecessor_id = c.id AND s.deleted_at IS NULL \
         ) \
         ORDER BY c.created_at DESC",
        cols = crate::models::claim::CLAIM_COLUMNS,
    );
    let claims = sqlx::query_as::<_, crate::models::claim::ClaimRow>(&claims_query)
        .bind(battle_id)
        .fetch_all(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let mut claims_warmed = 0u32;
    let mut failed = 0u32;
    if !battle_warmed {
        failed += 1;
    }
    for claim in &claims {
        if crate::routes::og::warm_claim_og(&state, claim).await {
            claims_warmed += 1;
        } else {
            failed += 1;
        }
    }

    Ok(Json(ApiResponse::new(serde_json::json!({
        "enabled": true,
        "battle": battle_warmed,
        "claims_warmed": claims_warmed,
        "failed": failed,
    }))))
}

/// `POST /v1/battles/{battle_id}/cover-image` — upload (or replace)
/// the Battle's hero cover image. Migration 032 / image_service.
///
/// Multipart fields:
///   - `image` (required) — PNG / JPEG / WebP, 2 MB cap.
///
/// Auth: `require_editor` (same gate as create_battle and the proxy
/// spec endpoints). Battle ownership doesn't grant cover-edit on its
/// own — only editors / admins set the public hero so the
/// editorially-curated look stays consistent.
///
/// Response: `{ url, alt_text }`. The alt text is generated by
/// Gemini Vision when configured, falling back to a filename-
/// derived caption.
pub async fn upload_cover_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
    mut multipart: Multipart,
) -> Result<axum::response::Response, ApiError> {
    require_editor(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;

    let mut image_bytes: Option<Vec<u8>> = None;
    let mut image_filename: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::ValidationError(e.to_string()))?
    {
        if field.name() == Some("image") {
            image_filename = field.file_name().map(|s| s.to_string());
            image_bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::ValidationError(e.to_string()))?
                    .to_vec(),
            );
        }
    }
    let bytes =
        image_bytes.ok_or_else(|| ApiError::ValidationError("missing field: image".into()))?;

    let uploaded = image_service::upload_image(
        &state.blob_store_url,
        Some(&state.gemini),
        &bytes,
        image_filename.as_deref(),
    )
    .await?;

    // Single-row update — replaces any prior cover. We don't keep
    // history on the battle row; the editorial pattern is "set
    // once, replace occasionally" and a stale cover GCS object can
    // be reaped by a sweep without affecting reads.
    let rows =
        sqlx::query("UPDATE battles SET cover_image_url = $1, cover_image_alt = $2 WHERE id = $3")
            .bind(&uploaded.url)
            .bind(&uploaded.suggested_alt)
            .bind(battle_id)
            .execute(&state.pool)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
            .rows_affected();
    if rows == 0 {
        return Err(ApiError::BattleNotFound);
    }

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "url": uploaded.url,
            "alt_text": uploaded.suggested_alt,
        }))),
    )
        .into_response())
}

/// `DELETE /v1/battles/{battle_id}/cover-image` — clear the cover
/// image (no-op when none is set).
pub async fn delete_cover_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(battle_id_raw): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_editor(&headers, &state.jwt_secret)?;
    let battle_id = slug_resolver::resolve_battle(&state.pool, &battle_id_raw).await?;
    let rows = sqlx::query(
        "UPDATE battles SET cover_image_url = NULL, cover_image_alt = NULL WHERE id = $1",
    )
    .bind(battle_id)
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .rows_affected();
    if rows == 0 {
        return Err(ApiError::BattleNotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
