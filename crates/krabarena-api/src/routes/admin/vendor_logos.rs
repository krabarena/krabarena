// --- Vendor logo map (data-driven, since migration 055) ---
//
// Replaces the old hardcoded `solution_names::VENDOR_PATTERNS` const.
// The map lives in the `vendor_logos` Neon table: each row is a
// case-insensitive regex `pattern` matched against a solution name and
// the `domain` its favicon is built from. Battle-solutions backfill
// reads it; render reads `battles.solutions[*].logo_url`. Adding a row
// here (then re-running a battle's backfill) gives a new competitor a
// logo with no deploy.
//
// Included into `admin/mod.rs`, so it shares that module's imports
// (State, HeaderMap, Json, StatusCode, ApiError, ApiResponse, AppState,
// require_admin, Serialize/Deserialize).

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct VendorLogoRow {
    pub pattern: String,
    pub domain: String,
    pub sort_order: i32,
    /// Self-hosted, content-versioned logo URL (`...png?v=<hash>`).
    pub stored_url: Option<String>,
    pub brand_color: Option<String>,
    /// True when `brand_color` was pinned by an admin (migration 071) —
    /// favicon auto-extraction then never overwrites it.
    pub brand_color_locked: bool,
}

/// GET /v1/admin/vendor-logos — list the vendor→domain map (ordered).
pub async fn list_vendor_logos(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let rows: Vec<VendorLogoRow> = sqlx::query_as(
        "SELECT pattern, domain, sort_order, stored_url, brand_color, brand_color_locked \
         FROM vendor_logos ORDER BY sort_order, pattern",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(Json(ApiResponse::new(rows)))
}

#[derive(Debug, Deserialize)]
pub struct UpsertVendorLogoRequest {
    pub pattern: String,
    pub domain: String,
    #[serde(default)]
    pub sort_order: Option<i32>,
    /// Optional admin override (since 0.43.3): `#rrggbb` pins the brand
    /// color (locked against auto-extraction); `"auto"` / `""` unpins it so
    /// the next favicon extraction takes over again. Omitted = unchanged.
    #[serde(default)]
    pub brand_color: Option<String>,
}

/// What an upsert does to the row's brand color.
#[derive(Debug, PartialEq, Eq)]
enum BrandColorChange {
    /// Field omitted — leave color + lock as they are.
    Keep,
    /// Pin this normalized `#rrggbb` and lock it.
    Pin(String),
    /// Unlock; auto-extraction may overwrite the color again.
    Unpin,
}

fn parse_brand_color_change(raw: Option<&str>) -> Result<BrandColorChange, ApiError> {
    match raw.map(str::trim) {
        None => Ok(BrandColorChange::Keep),
        Some("") => Ok(BrandColorChange::Unpin),
        Some(s) if s.eq_ignore_ascii_case("auto") => Ok(BrandColorChange::Unpin),
        Some(s) => crate::services::logo_assets::normalize_brand_color(s)
            .map(BrandColorChange::Pin)
            .ok_or_else(|| {
                ApiError::ValidationError(format!(
                    "brand_color must be a hex color like '#f97316' (or 'auto' to unpin), got '{s}'"
                ))
            }),
    }
}

/// POST /v1/admin/vendor-logos — add or update a vendor row by `pattern`.
/// Re-run the affected battle's `backfill-solutions` afterward to apply
/// the new logo to its stored solutions.
pub async fn upsert_vendor_logo(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpsertVendorLogoRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let pattern = req.pattern.trim();
    let domain = req.domain.trim();
    if pattern.is_empty() || domain.is_empty() {
        return Err(ApiError::ValidationError(
            "pattern and domain are required".into(),
        ));
    }
    // Validate the regex compiles before storing — a bad pattern would
    // otherwise be silently skipped at load time.
    regex::Regex::new(&format!("(?i){pattern}"))
        .map_err(|e| ApiError::ValidationError(format!("invalid regex pattern: {e}")))?;
    let color_change = parse_brand_color_change(req.brand_color.as_deref())?;
    // `$4` = pinned color (NULL unless Pin), `$5` = new lock state (NULL =
    // keep). An omitted `sort_order` keeps an existing row's order (it used
    // to silently reset to 100 on every update).
    let (pin_color, lock): (Option<String>, Option<bool>) = match &color_change {
        BrandColorChange::Keep => (None, None),
        BrandColorChange::Pin(c) => (Some(c.clone()), Some(true)),
        BrandColorChange::Unpin => (None, Some(false)),
    };
    let row: VendorLogoRow = sqlx::query_as(
        "INSERT INTO vendor_logos (pattern, domain, sort_order, brand_color, brand_color_locked) \
         VALUES ($1, $2, COALESCE($3, 100), $4, COALESCE($5, false)) \
         ON CONFLICT (pattern) DO UPDATE SET domain = EXCLUDED.domain, \
         sort_order = COALESCE($3, vendor_logos.sort_order), \
         brand_color = COALESCE($4, vendor_logos.brand_color), \
         brand_color_locked = COALESCE($5, vendor_logos.brand_color_locked), \
         updated_at = now() \
         RETURNING pattern, domain, sort_order, stored_url, brand_color, brand_color_locked",
    )
    .bind(pattern)
    .bind(domain)
    .bind(req.sort_order)
    .bind(&pin_color)
    .bind(lock)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    // Auto-fetch this domain's logo asset (favicon → brand color, and a
    // self-hosted GCS copy when configured) so a freshly-curated vendor
    // gets a brand-colored pie slice without a separate manual
    // refresh-assets call. Fire-and-forget; COALESCE so a partial
    // resolution never nulls out an already-stored value, and an
    // admin-pinned color (`brand_color_locked`) is never overwritten.
    {
        let vp_pool = state.pool.clone();
        let vp_http = state.proxy_http.clone();
        let vp_domain = domain.to_string();
        tokio::spawn(async move {
            let (stored_url, brand_color) =
                crate::services::logo_assets::resolve_domain_asset(&vp_http, &vp_domain).await;
            if stored_url.is_some() || brand_color.is_some() {
                if let Err(e) = sqlx::query(
                    "UPDATE vendor_logos SET stored_url = COALESCE($1, stored_url), \
                     brand_color = CASE WHEN brand_color_locked THEN brand_color \
                                        ELSE COALESCE($2, brand_color) END, \
                     updated_at = now() WHERE domain = $3",
                )
                .bind(&stored_url)
                .bind(&brand_color)
                .bind(&vp_domain)
                .execute(&vp_pool)
                .await
                {
                    tracing::warn!(domain = %vp_domain, error = %e, "vendor logo asset write failed");
                }
            }
        });
    }

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "pattern": row.pattern,
            "domain": row.domain,
            "sort_order": row.sort_order,
            "brand_color": row.brand_color,
            "brand_color_locked": row.brand_color_locked,
            "stored_url": row.stored_url,
            "message": "vendor logo upserted — asset fetch queued; re-run battle backfill-solutions to apply",
        }))),
    ))
}

#[derive(Debug, Deserialize)]
pub struct DeleteVendorLogoRequest {
    pub pattern: String,
}

/// DELETE /v1/admin/vendor-logos — remove a vendor row by exact `pattern`.
pub async fn delete_vendor_logo(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DeleteVendorLogoRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let deleted = sqlx::query("DELETE FROM vendor_logos WHERE pattern = $1")
        .bind(req.pattern.trim())
        .execute(&state.pool)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .rows_affected();
    Ok(Json(ApiResponse::new(serde_json::json!({ "deleted": deleted }))))
}

/// POST /v1/admin/vendor-logos/refresh-assets — download every distinct
/// vendor domain's logo into our GCS bucket, extract its dominant brand
/// color, and write `stored_url`/`brand_color` back to `vendor_logos`
/// (migration 057). Best-effort per domain; returns a `BackfillSummary`.
/// No-op (`skipped_dev`) when GCS isn't configured. Run once post-deploy
/// to flip logo resolution off the Google favicons.
pub async fn refresh_vendor_logo_assets(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    let summary =
        crate::services::logo_assets::backfill_all(&state.pool, &state.proxy_http)
            .await
            .map_err(ApiError::Internal)?;
    Ok(Json(ApiResponse::new(summary)))
}
