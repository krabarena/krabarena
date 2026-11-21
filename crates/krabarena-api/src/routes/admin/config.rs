// --- Config endpoints ---

/// GET /v1/admin/config/{key}
pub async fn get_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;

    let value = config_service::get_config(&state.pool, &key)
        .await?
        .ok_or_else(|| ApiError::ValidationError(format!("config key '{key}' not found")))?;

    Ok(Json(ApiResponse::new(serde_json::json!({
        "key": key,
        "value": value,
    }))))
}

#[derive(Debug, Deserialize)]
pub struct SetConfigRequest {
    pub value: String,
}

/// PUT /v1/admin/config/{key}
pub async fn set_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
    Json(req): Json<SetConfigRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = require_admin(&headers, &state.jwt_secret)?;

    if req.value.trim().is_empty() {
        return Err(ApiError::ValidationError(
            "config value cannot be empty".into(),
        ));
    }

    config_service::set_config(&state.pool, &key, &req.value, user_id).await?;
    // CMS-block writes need cache invalidation so an admin edit is
    // visible on the next request instead of after the 60s TTL. The
    // call is cheap and unconditional — non-cms keys just no-op.
    crate::services::cms_service::invalidate(&key).await;

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "key": key,
            "message": "config updated",
        }))),
    ))
}

/// POST /v1/admin/cms/reload — drop the in-process CMS cache on
/// this replica and re-fetch every block from the DB. Each
/// replica's cache is independent, so for prod (n>1 instances)
/// you need to call this once per replica — or `krab admin
/// cms-reload` which fans out for you in a future iteration.
pub async fn reload_cms(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;
    crate::services::cms_service::reload_all(&state.pool).await;
    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "message": "cms cache reloaded",
            "keys": crate::services::cms_service::ALL_KEYS,
        }))),
    ))
}

