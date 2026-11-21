// --- Voter listing (migration 068) ---
//
// Voters (OAuth readers, distinct from `users`/KrabAgent entities) can
// now be a digest-send audience. This mirrors the `?digest=eligible`
// shape added to `GET /v1/admin/users` (routes/admin/users.rs), but as
// its own endpoint since voters are a separate table with a simpler
// boolean opt-out (`voters.digest_opt_out`) rather than the
// three-state `user_email_preferences.digest_frequency`.

const ADMIN_VOTERS_DEFAULT_LIMIT: i64 = 200;
const ADMIN_VOTERS_MAX_LIMIT: i64 = 1000;

#[derive(Debug, Deserialize)]
pub struct ListVotersQuery {
    /// 1..1000, default 200.
    pub limit: Option<i64>,
    /// When `Some("eligible")`, restricts to voters with a non-null
    /// email who have not opted out of the digest. Any other
    /// non-empty value is a 400.
    pub digest: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AdminVoterResponse {
    pub id: Uuid,
    pub email: Option<String>,
    pub display_name: String,
    pub provider: String,
    pub created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct AdminVoterRow {
    id: Uuid,
    email: Option<String>,
    display_name: String,
    provider: String,
    created_at: DateTime<Utc>,
}

impl From<AdminVoterRow> for AdminVoterResponse {
    fn from(row: AdminVoterRow) -> Self {
        Self {
            id: row.id,
            email: row.email,
            display_name: row.display_name,
            provider: row.provider,
            created_at: row.created_at,
        }
    }
}

/// Caps + defaults `limit`. Voters has no `count_only`/`offset`
/// pagination yet (nothing consumes it beyond the digest-send job,
/// which pages through in `id` order client-side if it ever needs
/// to) — kept minimal on purpose.
fn voters_limit(limit: Option<i64>) -> Result<i64, ApiError> {
    let limit = limit.unwrap_or(ADMIN_VOTERS_DEFAULT_LIMIT);
    if !(1..=ADMIN_VOTERS_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::ValidationError(format!(
            "limit must be between 1 and {ADMIN_VOTERS_MAX_LIMIT}"
        )));
    }
    Ok(limit)
}

fn validate_voters_digest_filter(digest: &Option<String>) -> Result<(), ApiError> {
    if let Some(digest) = digest
        && digest != "eligible"
    {
        return Err(ApiError::ValidationError(
            "digest must be 'eligible'".to_string(),
        ));
    }
    Ok(())
}

/// `Some(true)` when the caller asked for `digest=eligible`, `None`
/// otherwise — bound straight into the `$1::bool IS NULL OR (...)`
/// clause in `list_voters`'s WHERE. Call only after
/// `validate_voters_digest_filter` has rejected anything other than
/// "eligible".
fn voters_digest_eligible_filter(digest: &Option<String>) -> Option<bool> {
    digest.as_deref().map(|_| true)
}

/// GET /v1/admin/voters
///
/// List voters (OAuth readers), optionally filtered to those eligible
/// for the digest send (`email IS NOT NULL AND digest_opt_out =
/// FALSE`). Admin-gated.
#[utoipa::path(
    get,
    path = "/v1/admin/voters",
    tag = "admin",
    params(
        ("limit" = Option<i64>, Query, description = "Page size (1..1000, default 200)"),
        ("digest" = Option<String>, Query, description = "When 'eligible', restricts to voters with an email who haven't opted out"),
    ),
    responses(
        (status = 200, description = "Voter list", body = Vec<AdminVoterResponse>),
        (status = 403, description = "Not an admin"),
    ),
)]
pub async fn list_voters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListVotersQuery>,
) -> Result<impl IntoResponse, ApiError> {
    require_admin(&headers, &state.jwt_secret)?;

    let limit = voters_limit(q.limit)?;
    validate_voters_digest_filter(&q.digest)?;
    let digest_filter = voters_digest_eligible_filter(&q.digest);

    let rows: Vec<AdminVoterRow> = sqlx::query_as(
        r#"
        SELECT id, email, display_name, provider, created_at
          FROM voters
         WHERE (
             $1::bool IS NULL
             OR (email IS NOT NULL AND digest_opt_out = FALSE)
         )
         ORDER BY created_at DESC
         LIMIT $2
        "#,
    )
    .bind(digest_filter)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let voters: Vec<AdminVoterResponse> = rows.into_iter().map(AdminVoterResponse::from).collect();

    Ok(Json(ApiResponse::new(voters)))
}
