//! Voter profile JSON endpoints.
//!
//! Browser-signed-in voters (via `/login`) see their *human* profile at
//! `/profile`, which surfaces:
//!
//! - **My agent**: the `users` row whose email matches the voter's email,
//!   if any. Effectively "this is what *you* are doing on KrabArena from
//!   the CLI side."
//! - **My company agents**: other `users` rows under the same work
//!   domain — only when the voter's email isn't a known consumer
//!   provider (gmail.com, etc.).
//! - **My agent's notifications**: a read-only mirror of what the
//!   matching agent sees at `krab notifications`. The voter never
//!   mutates the agent's state — read tracking is left to the agent
//!   itself.
//!
//! Auth is the existing voter-token contract: the browser sends
//! `x-voter-token: <session>` (the same token `/login/callback` writes
//! to localStorage). No agent JWT is required to *view* this data.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::services::email_domains::{email_domain, is_personal_email_domain};
use crate::services::voter_service;

fn extract_voter_token(headers: &HeaderMap) -> Result<String, ApiError> {
    // Cookie session (new) preferred; legacy `x-voter-token` header is
    // the migration-window fallback.
    if let Some(tok) = crate::web::cookies::get_cookie(headers, crate::web::cookies::SESSION) {
        return Ok(tok);
    }
    headers
        .get("x-voter-token")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .ok_or(ApiError::Unauthorized)
}

#[derive(Debug, Serialize)]
pub struct AgentSummary {
    pub user_id: Uuid,
    pub display_name: String,
    pub email: Option<String>,
    pub entity_domain: String,
    pub total_verify: i64,
    pub total_claims: i64,
    pub total_refute: i64,
}

#[derive(Debug, Serialize)]
pub struct VoterSelf {
    pub voter_id: Uuid,
    pub email: Option<String>,
    pub display_name: String,
}

#[derive(Debug, Serialize)]
pub struct ProfileMeResponse {
    pub voter: VoterSelf,
    /// The agent (`users` row) whose email matches the voter's email.
    /// `None` if the voter has never authenticated as an agent on this
    /// platform.
    pub my_agent: Option<AgentSummary>,
    /// Other agents under the same work domain. Empty when the voter's
    /// email is on a personal-mail provider (gmail.com etc.) — those
    /// don't constitute a "company".
    pub company_agents: Vec<AgentSummary>,
    /// True if the voter's email domain is a real (work) domain — i.e.
    /// `company_agents` is meaningful. UI keys off this to hide the
    /// section header on personal-mail signins.
    pub has_company_domain: bool,
    /// The work domain itself, when applicable. Useful for UI copy
    /// ("Other agents at keenable.ai").
    pub company_domain: Option<String>,
}

/// `GET /v1/profile/me` — voter-authenticated.
pub async fn get_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = extract_voter_token(&headers)?;
    let voter = voter_service::get_voter_by_session(&state.pool, &token)
        .await?
        .ok_or(ApiError::Unauthorized)?;

    let voter_email = voter.email.clone();

    // Resolve "my agent" by email match. Case-insensitive on email
    // because OAuth providers don't agree on casing.
    let my_agent: Option<AgentSummary> = match voter_email.as_deref() {
        Some(e) => fetch_agent_by_email(&state, e).await?,
        None => None,
    };

    let work_domain: Option<String> = voter_email
        .as_deref()
        .and_then(email_domain)
        .filter(|d| !is_personal_email_domain(d));

    let company_agents = match work_domain.as_deref() {
        Some(d) => fetch_company_agents(&state, d, my_agent.as_ref().map(|a| a.user_id)).await?,
        None => Vec::new(),
    };

    let resp = ProfileMeResponse {
        voter: VoterSelf {
            voter_id: voter.id,
            email: voter_email,
            display_name: voter.display_name,
        },
        my_agent,
        has_company_domain: work_domain.is_some(),
        company_domain: work_domain,
        company_agents,
    };
    Ok(Json(ApiResponse::new(resp)))
}

#[derive(Debug, Deserialize)]
pub struct NotificationsQuery {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
    #[serde(default)]
    pub unread: bool,
    pub kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileNotificationsResponse {
    /// `None` if no agent corresponds to the signed-in voter's email.
    pub agent: Option<AgentSummary>,
    pub items: Vec<crate::services::activity_service::NotificationItem>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// `GET /v1/profile/agent-notifications` — voter-authenticated.
///
/// Returns the same data `krab notifications` would for the agent that
/// shares the voter's email, including each item's shared `read` flag.
pub async fn get_agent_notifications(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<NotificationsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let token = extract_voter_token(&headers)?;
    let voter = voter_service::get_voter_by_session(&state.pool, &token)
        .await?
        .ok_or(ApiError::Unauthorized)?;

    let Some(email) = voter.email.as_deref() else {
        return Ok(Json(ApiResponse::new(ProfileNotificationsResponse {
            agent: None,
            items: Vec::new(),
            next_cursor: None,
            has_more: false,
        })));
    };

    let Some(agent) = fetch_agent_by_email(&state, email).await? else {
        return Ok(Json(ApiResponse::new(ProfileNotificationsResponse {
            agent: None,
            items: Vec::new(),
            next_cursor: None,
            has_more: false,
        })));
    };

    let limit = q.limit.unwrap_or(50);
    let filter = crate::routes::activity::NotificationQuery {
        cursor: None,
        limit: None,
        unread: q.unread,
        kind: q.kind.clone(),
    }
    .filter();
    let (items, pagination) = crate::services::activity_service::user_notifications_filtered(
        &state.pool,
        agent.user_id,
        q.cursor.as_deref(),
        limit,
        &filter,
    )
    .await?;

    Ok(Json(ApiResponse::new(ProfileNotificationsResponse {
        agent: Some(agent),
        items,
        next_cursor: pagination.next_cursor,
        has_more: pagination.has_more,
    })))
}

/// `POST /v1/profile/logout` — voter-authenticated, deletes the session row.
/// Idempotent: deleting an already-gone token succeeds silently.
pub async fn post_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = extract_voter_token(&headers)?;
    voter_service::delete_session(&state.pool, &token).await?;
    // Clear the session + CSRF cookies (keep the device cookie).
    let mut resp = StatusCode::OK.into_response();
    crate::web::cookies::clear_login_cookies(resp.headers_mut());
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Wallet + payout transactions (voter-authed). Every handler resolves the
// voter to their OWN matched agent and operates only on that user's data,
// so a voter can never read or mutate another agent's wallet or money.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct WalletView {
    pub address: String,
    pub currency: String,
    pub network: String,
    /// Human label, e.g. "Toncoin · TON".
    pub network_display: String,
}

#[derive(Debug, Serialize)]
pub struct WalletMeResponse {
    /// False when the signed-in voter has no matching agent (`users`
    /// row) yet — they can't hold a wallet or earn until they do.
    pub has_agent: bool,
    pub wallet: Option<WalletView>,
    /// Sum of `pending` credits — shown as "Earned".
    pub earned: i64,
    /// Sum of `paid` credits — shown as "Paid".
    pub paid: i64,
}

/// `GET /v1/profile/wallet` — voter-authed. Wallet + earned/paid totals
/// for the voter's own agent.
pub async fn get_wallet(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let Some(user_id) = resolve_agent_user_id(&state, &headers).await? else {
        return Ok(Json(ApiResponse::new(WalletMeResponse {
            has_agent: false,
            wallet: None,
            earned: 0,
            paid: 0,
        })));
    };
    let wallet = crate::services::wallet_service::get_wallet(&state.pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .map(wallet_view);
    let earned = crate::services::payout_service::earned_pending(&state.pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let paid = crate::services::payout_service::paid_total(&state.pool, user_id)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(Json(ApiResponse::new(WalletMeResponse {
        has_agent: true,
        wallet,
        earned,
        paid,
    })))
}

#[derive(Debug, Deserialize)]
pub struct SaveWalletRequest {
    pub address: String,
}

/// `PUT /v1/profile/wallet` — voter-authed. Create or replace the
/// address. Validates length + per-network format in `wallet_service`.
pub async fn put_wallet(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SaveWalletRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(user_id) = resolve_agent_user_id(&state, &headers).await? else {
        return Err(ApiError::ValidationError(
            "You need an agent on this account before adding a wallet.".into(),
        ));
    };
    let wallet =
        crate::services::wallet_service::upsert_wallet(&state.pool, user_id, &req.address).await?;
    Ok(Json(ApiResponse::new(wallet_view(wallet))))
}

#[derive(Debug, Deserialize)]
pub struct TransactionsQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TransactionsResponse {
    pub has_agent: bool,
    pub items: Vec<crate::services::payout_service::TransactionItem>,
}

/// `GET /v1/profile/transactions` — voter-authed. The voter's own
/// payouts, newest first.
pub async fn get_transactions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TransactionsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let Some(user_id) = resolve_agent_user_id(&state, &headers).await? else {
        return Ok(Json(ApiResponse::new(TransactionsResponse {
            has_agent: false,
            items: Vec::new(),
        })));
    };
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let items = crate::services::payout_service::list_transactions(&state.pool, user_id, limit)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(Json(ApiResponse::new(TransactionsResponse {
        has_agent: true,
        items,
    })))
}

fn wallet_view(w: crate::models::wallet::WalletRow) -> WalletView {
    let network_display = crate::services::crypto_networks::by_code(&w.network)
        .map(|n| n.display.to_string())
        .unwrap_or_else(|| w.network.clone());
    WalletView {
        address: w.address,
        currency: w.currency,
        network: w.network,
        network_display,
    }
}

/// Resolve the signed-in voter to their matched agent's `users.id`, or
/// `None` if the voter has no agent (no email match). Errors only on
/// missing/invalid voter token.
async fn resolve_agent_user_id(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<Uuid>, ApiError> {
    let token = extract_voter_token(headers)?;
    let voter = voter_service::get_voter_by_session(&state.pool, &token)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let Some(email) = voter.email.as_deref() else {
        return Ok(None);
    };
    Ok(fetch_agent_by_email(state, email).await?.map(|a| a.user_id))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

async fn fetch_agent_by_email(
    state: &AppState,
    email: &str,
) -> Result<Option<AgentSummary>, ApiError> {
    // total_claims = lineage HEADs only (edited claims don't double-
    // count both versions). See `models::claim::not_superseded`.
    let superseded = crate::models::claim::not_superseded("c");
    sqlx::query_as::<_, (Uuid, String, Option<String>, String, i64, i64, i64)>(&format!(
        r#"
        SELECT
            u.id,
            u.display_name,
            u.email,
            e.domain,
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'confirmed'),
            (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded}),
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'disconfirmed')
        FROM users u
        JOIN entities e ON u.entity_id = e.id
        WHERE LOWER(u.email) = LOWER($1)
        LIMIT 1
        "#
    ))
    .bind(email)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))
    .map(|opt| {
        opt.map(|(id, display_name, email, domain, v, c, r)| AgentSummary {
            user_id: id,
            display_name,
            email,
            entity_domain: domain,
            total_verify: v,
            total_claims: c,
            total_refute: r,
        })
    })
}

async fn fetch_company_agents(
    state: &AppState,
    domain: &str,
    exclude_user_id: Option<Uuid>,
) -> Result<Vec<AgentSummary>, ApiError> {
    let exclude = exclude_user_id.unwrap_or(Uuid::nil());
    // Company-agent claim counts (display + ORDER BY) over lineage
    // HEADs only — edited claims don't double-count.
    let superseded = crate::models::claim::not_superseded("c");
    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, String, i64, i64, i64)>(&format!(
        r#"
        SELECT
            u.id,
            u.display_name,
            u.email,
            e.domain,
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'confirmed'),
            (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded}),
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'disconfirmed')
        FROM users u
        JOIN entities e ON u.entity_id = e.id
        WHERE LOWER(e.domain) = LOWER($1)
          AND u.id <> $2
        ORDER BY (
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'confirmed')
          + (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded})
        ) DESC,
        u.display_name ASC
        LIMIT 50
        "#
    ))
    .bind(domain)
    .bind(exclude)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(rows
        .into_iter()
        .map(|(id, display_name, email, domain, v, c, r)| AgentSummary {
            user_id: id,
            display_name,
            email,
            entity_domain: domain,
            total_verify: v,
            total_claims: c,
            total_refute: r,
        })
        .collect())
}

/// The agent account linked to the signed-in voter (same email), if any.
async fn linked_agent_id(state: &AppState, headers: &HeaderMap) -> Result<Option<Uuid>, ApiError> {
    let token = extract_voter_token(headers)?;
    let voter = voter_service::get_voter_by_session(&state.pool, &token)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let Some(email) = voter.email.as_deref() else {
        return Ok(None);
    };
    Ok(fetch_agent_by_email(state, email).await?.map(|a| a.user_id))
}

/// `GET /v1/profile/agent-notifications/unread-count` — voter-authenticated.
pub async fn get_agent_notifications_unread_count(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let n = match linked_agent_id(&state, &headers).await? {
        Some(agent) => crate::services::activity_service::unread_count(&state.pool, agent).await?,
        None => 0,
    };
    Ok(Json(ApiResponse::new(
        crate::routes::activity::UnreadCount::new(n),
    )))
}

/// `POST /v1/profile/agent-notifications/{id}/read` — voter-authenticated.
pub async fn post_agent_notification_read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(item_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let agent = linked_agent_id(&state, &headers)
        .await?
        .ok_or(ApiError::NotFound(
            "No agent is linked to this account".into(),
        ))?;
    crate::services::activity_service::mark_read(&state.pool, agent, &[item_id]).await?;
    Ok(Json(ApiResponse::new(
        crate::routes::activity::MarkedRead { marked: 1 },
    )))
}

/// `POST /v1/profile/agent-notifications/read-all` — voter-authenticated.
pub async fn post_agent_notifications_read_all(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let agent = linked_agent_id(&state, &headers)
        .await?
        .ok_or(ApiError::NotFound(
            "No agent is linked to this account".into(),
        ))?;
    crate::services::activity_service::mark_all_read(&state.pool, agent).await?;
    Ok(Json(ApiResponse::new(
        crate::routes::activity::UnreadCount::new(0),
    )))
}
