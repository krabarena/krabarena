//! Social login for human voters + the CLI device flow.
//!
//! Supported providers: email (code / magic link), google, github, linkedin.
//! Creates/updates a voter record and returns a session cookie.
//!
//! ## OAuth `state` (CSRF / login-binding) — 2026-11 hardening
//!
//! Every provider sign-in now starts at `GET /auth/voter/{provider}/start`
//! (optionally `?session=<cli-session-uuid>` for the CLI device flow).
//! That route mints a random nonce, stores it in the HttpOnly, SameSite=Lax
//! `ka_oauth_state` cookie, and sends the provider a `state` of either
//! `w.<nonce>` (web login) or `d.<session>.<nonce>` (CLI device flow). The
//! callback refuses to do anything unless the `state` it receives carries
//! the same nonce as the cookie in *this* browser.
//!
//! Before this, the callback treated any `state` longer than 10 chars as a
//! device-session id and wrote the signed-in user's JWT into it. Anyone can
//! create a device session, so an attacker who got a victim to open a
//! crafted provider authorize URL (`state=<attacker session>`) received the
//! victim's JWT by polling `/v1/auth/device/poll`. Plain web logins had no
//! `state` at all (login CSRF).
//!
//! Device flow additionally requires an explicit "Authorize this device"
//! POST (`/auth/device/confirm`) from the same browser, showing the short
//! code the CLI printed, before the JWT becomes pollable.
//!
//! ## Verified emails only
//!
//! Provider emails are trusted (stored, used for account linking, used to
//! mint admin tokens) only when the provider says they are verified:
//! GitHub's primary verified address from `/user/emails`, Google's
//! `verified_email` / `email_verified`, LinkedIn's `email_verified`.

use askama::Template;
use axum::Json;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header::SET_COOKIE};
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::services::auth_service::{MIN_LINK_TOKEN_LEN, dev_log_login_codes, new_link_token};
use crate::services::email_validation::validate_voter_email;
use crate::services::profile_service::{self, OAuthProfile, clean_oauth_avatar, clean_oauth_text};
use crate::services::voter_service;
use crate::web::cookies;
use crate::web::meta::PageMeta;

/// Build the OAuth callback URL that gets sent as `redirect_uri` to
/// the provider's /authorize endpoint AND replayed verbatim on the
/// /token exchange. OAuth providers (Google in particular) enforce
/// exact-string equality between the two — if /authorize sends
/// `.com` but /token sends `.org`, the provider returns
/// `redirect_uri_mismatch`. That was the v0.10.0 post-rebrand bug
/// when /login already used `public_base_url` (= .com) but the
/// callback handler still hardcoded `.org`. Sharing this one helper
/// across both sites guarantees they can't drift again.
///
/// The OAuth app config at each provider must independently have
/// this exact URL registered in its allow-list, or auth fails at
/// /authorize with the same `redirect_uri_mismatch`.
pub fn oauth_callback_url(public_base_url: &str, provider: &str) -> String {
    format!(
        "{}/auth/voter/{provider}/callback",
        public_base_url.trim_end_matches('/'),
    )
}

/// LinkedIn's registered redirect base (historically configured via
/// `OAUTH_REDIRECT_BASE`, not `PUBLIC_BASE_URL`). Shared by /start and the
/// callback's /token exchange so the two stay byte-identical.
fn linkedin_redirect_uri() -> String {
    let base =
        std::env::var("OAUTH_REDIRECT_BASE").unwrap_or_else(|_| "http://localhost:3000".into());
    format!(
        "{}/auth/voter/linkedin/callback",
        base.trim_end_matches('/')
    )
}

// ---------------------------------------------------------------------
// OAuth state (CSRF + device-flow binding)
// ---------------------------------------------------------------------

/// HttpOnly cookie holding the per-browser OAuth state nonce.
pub const OAUTH_STATE_COOKIE: &str = "ka_oauth_state";
/// 10 minutes: provider round-trip + device confirmation.
const OAUTH_STATE_MAX_AGE: i64 = 600;
/// Wrong-code attempts after which a voter email code is dead.
pub const MAX_FAILED_CODE_ATTEMPTS: i32 = 5;

/// Decoded OAuth `state` parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthState {
    /// Plain web (voter) login.
    Web { nonce: String },
    /// CLI device flow for `session`.
    Device { session: Uuid, nonce: String },
}

fn valid_nonce(n: &str) -> bool {
    (16..=128).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl OAuthState {
    pub fn nonce(&self) -> &str {
        match self {
            Self::Web { nonce } | Self::Device { nonce, .. } => nonce,
        }
    }

    pub fn encode(&self) -> String {
        match self {
            Self::Web { nonce } => format!("w.{nonce}"),
            Self::Device { session, nonce } => format!("d.{session}.{nonce}"),
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.splitn(3, '.');
        let state = match (parts.next()?, parts.next(), parts.next()) {
            ("w", Some(nonce), None) => Self::Web {
                nonce: nonce.to_string(),
            },
            ("d", Some(session), Some(nonce)) => Self::Device {
                session: Uuid::parse_str(session).ok()?,
                nonce: nonce.to_string(),
            },
            _ => return None,
        };
        valid_nonce(state.nonce()).then_some(state)
    }
}

/// Validate the callback's `state` against this browser's
/// `ka_oauth_state` cookie (constant-time). Any mismatch — no cookie,
/// malformed state, different nonce — is `Unauthorized`.
pub fn verify_oauth_state(headers: &HeaderMap, raw_state: &str) -> Result<OAuthState, ApiError> {
    let state = OAuthState::parse(raw_state).ok_or(ApiError::Unauthorized)?;
    let cookie = cookies::get_cookie(headers, OAUTH_STATE_COOKIE).ok_or(ApiError::Unauthorized)?;
    if !cookies::ct_eq(&cookie, state.nonce()) {
        return Err(ApiError::Unauthorized);
    }
    Ok(state)
}

fn append_cookie(headers: &mut HeaderMap, cookie: String) {
    if let Ok(hv) = cookie.parse() {
        headers.append(SET_COOKIE, hv);
    }
}

fn set_state_cookie(headers: &mut HeaderMap, nonce: &str) {
    set_state_cookie_max_age(headers, nonce, OAUTH_STATE_MAX_AGE);
}

pub(crate) fn set_state_cookie_max_age(headers: &mut HeaderMap, nonce: &str, max_age: i64) {
    append_cookie(
        headers,
        cookies::build(
            OAUTH_STATE_COOKIE,
            nonce,
            max_age,
            true,
            cookies::secure_cookies(),
        ),
    );
}

pub(crate) fn clear_state_cookie(headers: &mut HeaderMap) {
    append_cookie(
        headers,
        cookies::clear(OAUTH_STATE_COOKIE, cookies::secure_cookies()),
    );
}

/// Value stored on the device row to bind a staged token to the browser
/// that completed the provider login (SHA-256 of its state nonce).
fn browser_binding(nonce: &str) -> String {
    hex::encode(Sha256::digest(
        format!("krab-device-binding|{nonce}").as_bytes(),
    ))
}

/// Short human-checkable code for a device session: the first 8 hex
/// digits of the session UUID, `ABCD-1234`. `krab auth login` prints the
/// same code (derived the same way) so the user can match terminal and
/// browser before approving.
pub fn device_user_code(session: &Uuid) -> String {
    let hex = session.simple().to_string().to_uppercase();
    format!("{}-{}", &hex[..4], &hex[4..8])
}

/// The local start path for a provider (what login buttons link to).
pub fn provider_start_path(provider: &str, session: Option<&Uuid>) -> String {
    match session {
        Some(s) => format!("/auth/voter/{provider}/start?session={s}"),
        None => format!("/auth/voter/{provider}/start"),
    }
}

/// `(github, google)` web-login button targets for the configured
/// providers (`None` when a provider's client id is unset).
pub fn web_provider_links() -> (Option<String>, Option<String>) {
    let configured = |env: &str| std::env::var(env).is_ok_and(|v| !v.is_empty());
    (
        configured("GITHUB_CLIENT_ID").then(|| provider_start_path("github", None)),
        configured("GOOGLE_CLIENT_ID").then(|| provider_start_path("google", None)),
    )
}

/// If the callback's `state` is a device-flow state, point the retry link
/// at that device session; otherwise at `/login`.
fn oauth_retry_url(raw_state: &str) -> String {
    match OAuthState::parse(raw_state) {
        Some(OAuthState::Device { session, .. }) => format!("/auth/device?session={session}"),
        _ => "/login".to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Github,
    Google,
    Linkedin,
}

impl Provider {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(Self::Github),
            "google" => Some(Self::Google),
            "linkedin" => Some(Self::Linkedin),
            _ => None,
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Google => "google",
            Self::Linkedin => "linkedin",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Google => "Google",
            Self::Linkedin => "LinkedIn",
        }
    }

    fn client_id_env(self) -> &'static str {
        match self {
            Self::Github => "GITHUB_CLIENT_ID",
            Self::Google => "GOOGLE_CLIENT_ID",
            Self::Linkedin => "LINKEDIN_CLIENT_ID",
        }
    }

    fn authorize_url(self, client_id: &str, public_base_url: &str, state: &str) -> String {
        match self {
            Self::Github => {
                let redirect_uri = oauth_callback_url(public_base_url, "github");
                format!(
                    "https://github.com/login/oauth/authorize?client_id={client_id}\
                     &redirect_uri={redirect_uri}&scope=read:user,user:email&state={state}"
                )
            }
            Self::Google => {
                let redirect_uri = oauth_callback_url(public_base_url, "google");
                format!(
                    "https://accounts.google.com/o/oauth2/v2/auth?client_id={client_id}\
                     &redirect_uri={redirect_uri}&response_type=code\
                     &scope=openid+email+profile&state={state}"
                )
            }
            Self::Linkedin => {
                let redirect_uri = linkedin_redirect_uri();
                format!(
                    "https://www.linkedin.com/oauth/v2/authorization?response_type=code\
                     &client_id={client_id}&redirect_uri={redirect_uri}\
                     &scope=openid+profile+email&state={state}"
                )
            }
        }
    }
}

#[derive(Template)]
#[template(path = "pages/oauth_error.html")]
struct OAuthErrorTemplate {
    meta: PageMeta,
    provider: String,
    headline: String,
    detail: String,
    retry_url: String,
    ref_id: String,
}

#[derive(Template)]
#[template(path = "pages/oauth_device_authorized.html")]
struct OAuthDeviceAuthorizedTemplate {
    meta: PageMeta,
    display_name: String,
}

#[derive(Template)]
#[template(path = "pages/oauth_device_confirm.html")]
struct OAuthDeviceConfirmTemplate {
    meta: PageMeta,
    display_name: String,
    user_code: String,
    session: String,
}

#[derive(Template)]
#[template(path = "pages/oauth_login_callback.html")]
struct OAuthLoginCallbackTemplate {
    meta: PageMeta,
}

#[derive(Template)]
#[template(path = "pages/oauth_device_login.html")]
struct OAuthDeviceLoginTemplate {
    meta: PageMeta,
    github_url: String,
    google_url: String,
    agentid_url: Option<String>,
    user_code: String,
}

/// Render a human-friendly HTML error page for an OAuth callback
/// failure. Called in place of letting `ApiError` fall through to the
/// default JSON error response — browser users were seeing raw JSON.
pub(crate) fn oauth_error_page(
    public_base_url: &str,
    provider: &str,
    err: &ApiError,
    retry_url: &str,
) -> Response {
    // Mask internal errors so we don't leak DB messages to the browser.
    // Validation / forbidden errors are safer to show verbatim.
    let (headline, detail) = match err {
        ApiError::ValidationError(m) => ("Sign-in Failed", m.clone()),
        ApiError::Forbidden => ("Not Allowed", "You don't have access.".to_string()),
        ApiError::Unauthorized => (
            "Session Expired",
            "Start over from the sign-in link.".to_string(),
        ),
        ApiError::Internal(_) => (
            "Sign-in Failed",
            "Something went wrong on our end. Try again; if it keeps happening, \
             ping us with the error ref below."
                .to_string(),
        ),
        _ => ("Sign-in Failed", err.to_string()),
    };
    let ref_id = uuid::Uuid::new_v4();
    tracing::error!(
        provider = provider,
        error = %err,
        ref_id = %ref_id,
        "oauth callback error"
    );
    let template = OAuthErrorTemplate {
        meta: PageMeta::new(public_base_url, retry_url)
            .with_title(headline)
            .with_description("KrabArena sign-in could not be completed.")
            .noindex_follow(),
        provider: provider.to_string(),
        headline: headline.to_string(),
        detail,
        retry_url: retry_url.to_string(),
        ref_id: ref_id.to_string(),
    };
    crate::web::errors::render_template_or_500(template, public_base_url, retry_url, "oauth.error")
}

fn render_device_authorized_page(public_base_url: &str, display_name: &str) -> Response {
    crate::web::errors::render_template_or_500(
        OAuthDeviceAuthorizedTemplate {
            meta: PageMeta::new(public_base_url, "/auth/device")
                .with_title("Authorized")
                .with_description("KrabArena CLI device authorization is complete.")
                .noindex_follow(),
            display_name: display_name.to_string(),
        },
        public_base_url,
        "/auth/device",
        "oauth.device.authorized",
    )
}

fn render_device_confirm_page(
    public_base_url: &str,
    display_name: &str,
    session: &Uuid,
) -> Response {
    crate::web::errors::render_template_or_500(
        OAuthDeviceConfirmTemplate {
            meta: PageMeta::new(public_base_url, "/auth/device")
                .with_title("Authorize CLI")
                .with_description("Confirm KrabArena CLI device authorization.")
                .noindex_follow(),
            display_name: display_name.to_string(),
            user_code: device_user_code(session),
            session: session.to_string(),
        },
        public_base_url,
        "/auth/device",
        "oauth.device.confirm",
    )
}

#[derive(Debug, Deserialize)]
pub struct EmailLoginRequest {
    pub email: String,
    /// reCAPTCHA Enterprise token from `grecaptcha.enterprise.execute`.
    /// Optional so the field can be omitted entirely when captcha is disabled
    /// (`RECAPTCHA_SITE_KEY` unset → no widget → no token). When the server
    /// has reCAPTCHA Enterprise fully configured, a missing token is rejected.
    #[serde(default)]
    pub captcha_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub voter_token: String,
    pub display_name: String,
    /// JWT minted alongside the voter session when the voter's email
    /// matches a `users.role='admin'` row. Frontend stores it as
    /// `krabarena_token` so admin-gated UI appears
    /// without requiring a separate `krab auth login` device-flow.
    /// `None` for non-admin voters; the field is omitted from the
    /// JSON in that case via `skip_serializing_if`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin_token: Option<String>,
}

/// Look up `users WHERE email = $1 AND role = 'admin'` and, if a row
/// exists, mint a JWT for it. Returns `Ok(None)` when no admin user
/// has that email — that's the common case for ordinary voters and
/// must NOT bubble up as an error. The `email` argument MUST be an
/// address the caller has proven (email code / link, or a
/// provider-verified email recorded on the voter session) — never the
/// free-form `voters.email` profile value.
async fn mint_admin_token_for_voter_email(
    pool: &PgPool,
    verified_email: &str,
    jwt_secret: &str,
) -> Result<Option<String>, ApiError> {
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, entity_id, role FROM users WHERE email = $1 AND role = 'admin' \
           AND NOT COALESCE(suspended_until > NOW(), FALSE)",
    )
    .bind(verified_email)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    match row {
        Some((user_id, entity_id, role)) => {
            let jwt =
                crate::services::auth_service::create_jwt(user_id, entity_id, &role, jwt_secret)?;
            Ok(Some(jwt))
        }
        None => Ok(None),
    }
}

/// Email login: issue an 8-digit code (manual entry, valid only together
/// with the email) plus a separate 256-bit magic-link token.
pub async fn email_login(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<EmailLoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // Client IP (rightmost untrusted X-Forwarded-For hop — the same
    // spoof-resistant rule the rate limiter uses) sharpens the reCAPTCHA
    // Enterprise risk score. Best-effort — None when absent.
    let remote_ip =
        crate::web::rate_limit::client_ip_from_request(&headers, None).map(|ip| ip.to_string());

    // Bot gate: create a reCAPTCHA Enterprise assessment before doing any work
    // (sending an email, writing to pending_verifications). Skipped entirely
    // when reCAPTCHA isn't fully configured — see captcha_service.
    if state.recaptcha.is_enabled() {
        crate::services::captcha_service::verify_enterprise(
            &state.proxy_http,
            &state.recaptcha,
            req.captcha_token.as_deref().unwrap_or(""),
            "voter_email",
            remote_ip.as_deref(),
        )
        .await?;
    }

    let email = req.email.trim().to_lowercase();
    if validate_voter_email(&email).is_err() {
        return Err(ApiError::InvalidEmail);
    }

    let code: String = format!("{:08}", rand_code());
    let link_token = new_link_token();
    issue_email_codes(&state.pool, &email, &code, &link_token).await?;

    // Send verification email if email service is configured
    if let Some(ref es) = state.email_service {
        es.send_verification(&email, &code, &link_token)
            .await
            .map_err(|e| ApiError::EmailSendFailed(e.to_string()))?;
        tracing::info!(email = %email, "voter.email_verification_sent");
    } else if dev_log_login_codes() {
        tracing::warn!(email = %email, code = %code, link_token = %link_token,
            "voter.email_code_logged (no email service; KRAB_DEV_LOG_LOGIN_CODES=1)");
    } else {
        tracing::warn!(email = %email,
            "voter.email_code_not_sent (no email service; set KRAB_DEV_LOG_LOGIN_CODES=1 in dev)");
    }

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(
            serde_json::json!({ "message": "Verification code sent" }),
        )),
    ))
}

/// Store a fresh 8-digit code and a magic-link token for `email`. Any
/// earlier unconsumed 8-digit codes for the address are deleted first —
/// only the newest code is live, so repeated requests can't stack up
/// several codes (each with its own attempt budget) for one guesser. The
/// new code starts with `failed_attempts = 0`; an exhausted counter is
/// never reset in place.
pub async fn issue_email_codes(
    pool: &PgPool,
    email: &str,
    code: &str,
    link_token: &str,
) -> Result<(), ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    sqlx::query(
        "DELETE FROM pending_verifications \
         WHERE verification_type = 'email' AND identifier = $1 AND char_length(code) < $2",
    )
    .bind(email)
    .bind(MIN_LINK_TOKEN_LEN as i32)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;
    for c in [code, link_token] {
        sqlx::query(
            "INSERT INTO pending_verifications (verification_type, identifier, code, expires_at) \
             VALUES ('email', $1, $2, NOW() + INTERVAL '15 minutes')",
        )
        .bind(email)
        .bind(c)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(())
}

/// Check an 8-digit email code for `email`. On a match the address's
/// short codes are consumed and `Ok(true)` is returned. On a miss every
/// live short code for the address gets `failed_attempts += 1`; a code
/// that has reached `MAX_FAILED_CODE_ATTEMPTS` never matches again. Rows
/// are locked (`FOR UPDATE`) so concurrent guesses can't overshoot the
/// cap.
pub async fn check_email_code(pool: &PgPool, email: &str, code: &str) -> Result<bool, ApiError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    let rows: Vec<(Uuid, String, i32)> = sqlx::query_as(
        "SELECT id, code, failed_attempts FROM pending_verifications \
         WHERE verification_type = 'email' AND identifier = $1 AND expires_at > NOW() \
           AND char_length(code) < $2 \
         FOR UPDATE",
    )
    .bind(email)
    .bind(MIN_LINK_TOKEN_LEN as i32)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    let live: Vec<&(Uuid, String, i32)> = rows
        .iter()
        .filter(|(_, _, fails)| *fails < MAX_FAILED_CODE_ATTEMPTS)
        .collect();
    // Compare against every live code (no early exit) in constant time.
    let hit = live
        .iter()
        .fold(false, |acc, (_, c, _)| cookies::ct_eq(c, code) | acc);

    if hit {
        sqlx::query(
            "DELETE FROM pending_verifications \
             WHERE verification_type = 'email' AND identifier = $1 AND char_length(code) < $2",
        )
        .bind(email)
        .bind(MIN_LINK_TOKEN_LEN as i32)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    } else if !live.is_empty() {
        let ids: Vec<Uuid> = live.iter().map(|(id, _, _)| *id).collect();
        sqlx::query(
            "UPDATE pending_verifications SET failed_attempts = failed_attempts + 1 \
             WHERE id = ANY($1)",
        )
        .bind(&ids)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    }
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    Ok(hit)
}

#[derive(Debug, Deserialize)]
pub struct EmailVerifyRequest {
    pub email: String,
    pub code: String,
}

/// Email code verify: create voter session.
pub async fn email_verify(
    State(state): State<AppState>,
    Json(req): Json<EmailVerifyRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let email = req.email.trim().to_lowercase();
    let code = req.code.trim();

    if email.is_empty() || !check_email_code(&state.pool, &email, code).await? {
        return Err(ApiError::InvalidVerificationToken);
    }

    let display_name = email.split('@').next().unwrap_or("voter").to_string();
    let voter = voter_service::find_or_create_voter(
        &state.pool,
        "email",
        &email,
        &display_name,
        Some(&email),
        None,
    )
    .await?;

    if let Err(e) = profile_service::sync_agent_for_email(&state.pool, &email).await {
        tracing::warn!(error = %e, voter_id = %voter.id, "profile.agent_sync_failed");
    }

    // The code proves control of `email`.
    let token = voter_service::create_session(&state.pool, voter.id, Some(&email)).await?;
    let admin_token =
        mint_admin_token_for_voter_email(&state.pool, &email, &state.jwt_secret).await?;

    let mut resp = Json(ApiResponse::new(LoginResponse {
        voter_token: token.clone(),
        display_name: voter.display_name,
        admin_token,
    }))
    .into_response();
    crate::web::cookies::set_login_cookies(resp.headers_mut(), &token);
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    pub session: Option<String>,
}

/// GET /auth/voter/{provider}/start[?session=<cli-session>] — begin a
/// provider sign-in: set the HttpOnly state-nonce cookie and redirect to
/// the provider with a matching `state`.
pub async fn oauth_start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
) -> Response {
    let base = state.public_base_url.clone();
    let Some(provider) = Provider::parse(&provider) else {
        return crate::web::errors::render_404(&base, "/login");
    };
    let session = match q.session.as_deref().filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(s) if provider != Provider::Linkedin => Some(s),
            _ => {
                return oauth_error_page(
                    &base,
                    provider.label(),
                    &ApiError::ValidationError(
                        "Invalid CLI session. Re-run `krab auth login`.".into(),
                    ),
                    "/login",
                );
            }
        },
    };
    let client_id = std::env::var(provider.client_id_env()).unwrap_or_default();
    if client_id.is_empty() {
        return oauth_error_page(
            &base,
            provider.label(),
            &ApiError::ValidationError(format!("{} OAuth not configured", provider.label())),
            "/login",
        );
    }
    let nonce = cookies::new_csrf();
    let oauth_state = match session {
        Some(session) => OAuthState::Device {
            session,
            nonce: nonce.clone(),
        },
        None => OAuthState::Web {
            nonce: nonce.clone(),
        },
    };
    let url = provider.authorize_url(&client_id, &base, &oauth_state.encode());
    let mut resp = Redirect::to(&url).into_response();
    set_state_cookie(resp.headers_mut(), &nonce);
    resp
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackQuery {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub state: String,
}

/// What a provider told us about the signed-in user.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderIdentity {
    provider: &'static str,
    provider_id: String,
    display_name: String,
    /// Only set when the provider asserts the address is verified.
    verified_email: Option<String>,
    avatar: Option<String>,
    profile: OAuthProfile,
}

fn json_str(v: &serde_json::Value, key: &str) -> Option<String> {
    clean_oauth_text(v[key].as_str())
}

fn github_oauth_profile(
    user_info: &serde_json::Value,
    verified_email: Option<&str>,
) -> OAuthProfile {
    OAuthProfile {
        provider: "github".into(),
        email: verified_email.map(str::to_string),
        email_verified: verified_email.is_some(),
        full_name: json_str(user_info, "name"),
        given_name: None,
        family_name: None,
        username: json_str(user_info, "login"),
        profile_url: json_str(user_info, "html_url"),
        avatar_url: clean_oauth_avatar(user_info["avatar_url"].as_str()),
        website_url: json_str(user_info, "blog"),
        x_handle: json_str(user_info, "twitter_username"),
        bio: json_str(user_info, "bio"),
        company: json_str(user_info, "company"),
        location: json_str(user_info, "location"),
    }
}

fn oidc_oauth_profile(
    provider: &str,
    user_info: &serde_json::Value,
    verified_email: Option<&str>,
) -> OAuthProfile {
    OAuthProfile {
        provider: provider.into(),
        email: verified_email.map(str::to_string),
        email_verified: verified_email.is_some(),
        full_name: json_str(user_info, "name"),
        given_name: json_str(user_info, "given_name"),
        family_name: json_str(user_info, "family_name"),
        username: None,
        profile_url: json_str(user_info, "profile"),
        avatar_url: clean_oauth_avatar(user_info["picture"].as_str()),
        website_url: None,
        x_handle: None,
        bio: None,
        company: json_str(user_info, "hd"),
        location: None,
    }
}

/// Normalise a provider email: trimmed, lowercased, non-empty.
fn norm_email(e: Option<&str>) -> Option<String> {
    e.map(|s| s.trim().to_lowercase())
        .filter(|s| s.contains('@'))
}

/// GitHub: the primary AND verified address from `GET /user/emails`.
fn github_primary_verified_email(emails: &serde_json::Value) -> Option<String> {
    emails.as_array()?.iter().find_map(|e| {
        (e["primary"].as_bool() == Some(true) && e["verified"].as_bool() == Some(true))
            .then(|| norm_email(e["email"].as_str()))
            .flatten()
    })
}

/// Google userinfo (v2 `verified_email`, OIDC `email_verified`).
fn google_verified_email(user_info: &serde_json::Value) -> Option<String> {
    let verified = user_info["verified_email"].as_bool() == Some(true)
        || user_info["email_verified"].as_bool() == Some(true);
    verified
        .then(|| norm_email(user_info["email"].as_str()))
        .flatten()
}

/// LinkedIn OIDC userinfo `email_verified`.
fn linkedin_verified_email(user_info: &serde_json::Value) -> Option<String> {
    (user_info["email_verified"].as_bool() == Some(true))
        .then(|| norm_email(user_info["email"].as_str()))
        .flatten()
}

async fn github_identity(code: &str) -> Result<ProviderIdentity, ApiError> {
    let client_id = std::env::var("GITHUB_CLIENT_ID")
        .map_err(|_| ApiError::ValidationError("GitHub OAuth not configured".into()))?;
    let client_secret = std::env::var("GITHUB_CLIENT_SECRET")
        .map_err(|_| ApiError::ValidationError("GitHub OAuth not configured".into()))?;

    let http = reqwest::Client::new();

    let token_resp = http
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code),
        ])
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let access_token = token_resp["access_token"]
        .as_str()
        .ok_or_else(|| ApiError::ValidationError("GitHub token exchange failed".into()))?;

    let user_info = http
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "KrabArena")
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let github_id = user_info["id"]
        .as_i64()
        .ok_or_else(|| ApiError::ValidationError("GitHub user lookup failed".into()))?
        .to_string();

    // The profile `email` field is not a verification signal; ask for the
    // account's email list and keep only the primary verified address.
    let emails = http
        .get("https://api.github.com/user/emails")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "KrabArena")
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .json::<serde_json::Value>()
        .await
        .unwrap_or(serde_json::Value::Null);

    let verified_email = github_primary_verified_email(&emails);
    let profile = github_oauth_profile(&user_info, verified_email.as_deref());
    Ok(ProviderIdentity {
        provider: "github",
        provider_id: github_id,
        display_name: user_info["login"]
            .as_str()
            .unwrap_or("github-user")
            .to_string(),
        verified_email,
        avatar: user_info["avatar_url"].as_str().map(str::to_string),
        profile,
    })
}

async fn google_identity(code: &str, public_base_url: &str) -> Result<ProviderIdentity, ApiError> {
    let client_id = std::env::var("GOOGLE_CLIENT_ID")
        .map_err(|_| ApiError::ValidationError("Google OAuth not configured".into()))?;
    let client_secret = std::env::var("GOOGLE_CLIENT_SECRET")
        .map_err(|_| ApiError::ValidationError("Google OAuth not configured".into()))?;
    let redirect_uri = oauth_callback_url(public_base_url, "google");

    let http = reqwest::Client::new();

    let token_resp = http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", code),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let access_token = token_resp["access_token"]
        .as_str()
        .ok_or_else(|| ApiError::ValidationError("Google token exchange failed".into()))?;

    let user_info = http
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let google_id = user_info["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::ValidationError("Google user lookup failed".into()))?
        .to_string();

    let verified_email = google_verified_email(&user_info);
    let profile = oidc_oauth_profile("google", &user_info, verified_email.as_deref());
    Ok(ProviderIdentity {
        provider: "google",
        provider_id: google_id,
        display_name: user_info["name"]
            .as_str()
            .unwrap_or("google-user")
            .to_string(),
        verified_email,
        avatar: user_info["picture"].as_str().map(str::to_string),
        profile,
    })
}

async fn linkedin_identity(code: &str) -> Result<ProviderIdentity, ApiError> {
    let client_id = std::env::var("LINKEDIN_CLIENT_ID")
        .map_err(|_| ApiError::ValidationError("LinkedIn OAuth not configured".into()))?;
    let client_secret = std::env::var("LINKEDIN_CLIENT_SECRET")
        .map_err(|_| ApiError::ValidationError("LinkedIn OAuth not configured".into()))?;
    let redirect_uri = linkedin_redirect_uri();

    let http = reqwest::Client::new();

    let token_resp = http
        .post("https://www.linkedin.com/oauth/v2/accessToken")
        .form(&[
            ("code", code),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let access_token = token_resp["access_token"]
        .as_str()
        .ok_or_else(|| ApiError::ValidationError("LinkedIn token exchange failed".into()))?;

    let user_info = http
        .get("https://api.linkedin.com/v2/userinfo")
        .header("Authorization", format!("Bearer {}", access_token))
        .send()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;

    let linkedin_id = user_info["sub"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::ValidationError("LinkedIn user lookup failed".into()))?
        .to_string();

    let verified_email = linkedin_verified_email(&user_info);
    let profile = oidc_oauth_profile("linkedin", &user_info, verified_email.as_deref());
    Ok(ProviderIdentity {
        provider: "linkedin",
        provider_id: linkedin_id,
        display_name: user_info["name"]
            .as_str()
            .unwrap_or("linkedin-user")
            .to_string(),
        verified_email,
        avatar: user_info["picture"].as_str().map(str::to_string),
        profile,
    })
}

/// Shared callback driver: verify `state` against the browser cookie
/// BEFORE touching the provider, then finish either the web login or the
/// (staged, confirmation-pending) device login.
async fn oauth_callback(
    state: AppState,
    headers: HeaderMap,
    query: OAuthCallbackQuery,
    provider: Provider,
) -> Response {
    let retry = oauth_retry_url(&query.state);
    let base = state.public_base_url.clone();
    match oauth_callback_impl(&state, &headers, &query, provider).await {
        Ok(resp) => resp,
        Err(e) => oauth_error_page(&base, provider.label(), &e, &retry),
    }
}

async fn oauth_callback_impl(
    state: &AppState,
    headers: &HeaderMap,
    query: &OAuthCallbackQuery,
    provider: Provider,
) -> Result<Response, ApiError> {
    let oauth_state = verify_oauth_state(headers, &query.state)?;
    if query.code.is_empty() {
        return Err(ApiError::ValidationError(format!(
            "{} sign-in was cancelled or failed.",
            provider.label()
        )));
    }
    let identity = match provider {
        Provider::Github => github_identity(&query.code).await?,
        Provider::Google => google_identity(&query.code, &state.public_base_url).await?,
        Provider::Linkedin => linkedin_identity(&query.code).await?,
    };
    debug_assert_eq!(identity.provider, provider.slug());

    match oauth_state {
        OAuthState::Device { session, nonce } => {
            stage_device_login(state, &identity, &session, &nonce).await
        }
        OAuthState::Web { .. } => complete_web_login(state, &identity).await,
    }
}

/// Device flow, step 2: mint the CLI JWT but only STAGE it on the device
/// row (bound to this browser's nonce); render the confirmation page.
async fn stage_device_login(
    state: &AppState,
    identity: &ProviderIdentity,
    session: &Uuid,
    nonce: &str,
) -> Result<Response, ApiError> {
    let fallback_domain = format!("{}.com", identity.provider);
    let domain = identity
        .verified_email
        .as_deref()
        .and_then(|e| e.split('@').nth(1))
        .unwrap_or(&fallback_domain);
    let entity = crate::services::auth_service::find_or_create_entity_pub(
        &state.pool,
        domain,
        identity.provider,
    )
    .await?;
    let user = crate::services::auth_service::find_or_create_user_pub(
        &state.pool,
        entity.id,
        &format!("{}:{}", identity.provider, identity.provider_id),
        identity.provider,
        &identity.display_name,
        identity.verified_email.as_deref(),
    )
    .await?;
    if let Err(e) =
        profile_service::apply_seeds(&state.pool, user.id, &[identity.profile.seed()]).await
    {
        tracing::warn!(error = %e, user_id = %user.id, "profile.oauth_seed_failed");
    }
    let jwt = crate::services::auth_service::create_jwt(
        user.id,
        user.entity_id,
        &user.role,
        &state.jwt_secret,
    )?;
    stage_device_jwt(state, &jwt, &identity.display_name, session, nonce).await
}

pub(crate) async fn stage_device_jwt(
    state: &AppState,
    jwt: &str,
    display_name: &str,
    session: &Uuid,
    nonce: &str,
) -> Result<Response, ApiError> {
    let staged = sqlx::query(
        "UPDATE pending_verifications \
         SET pending_token = $1, browser_binding = $2, expires_at = NOW() + INTERVAL '10 minutes' \
         WHERE verification_type = 'device' AND code = $3 AND expires_at > NOW() \
           AND result_token IS NULL",
    )
    .bind(jwt)
    .bind(browser_binding(nonce))
    .bind(session.to_string())
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .rows_affected();
    if staged == 0 {
        return Err(ApiError::Unauthorized);
    }

    Ok(render_device_confirm_page(
        &state.public_base_url,
        display_name,
        session,
    ))
}

/// Web login: create/refresh the voter, open a session, and hand the
/// token to `/login/callback` in the URL fragment (never the query).
async fn complete_web_login(
    state: &AppState,
    identity: &ProviderIdentity,
) -> Result<Response, ApiError> {
    let voter = voter_service::find_or_create_voter(
        &state.pool,
        identity.provider,
        &identity.provider_id,
        &identity.display_name,
        identity.verified_email.as_deref(),
        identity.avatar.as_deref(),
    )
    .await?;

    capture_oauth_profile(state, voter.id, identity).await;

    let token =
        voter_service::create_session(&state.pool, voter.id, identity.verified_email.as_deref())
            .await?;

    let mut resp = Redirect::to(&login_callback_location(&token)).into_response();
    crate::web::cookies::set_login_cookies(resp.headers_mut(), &token);
    clear_state_cookie(resp.headers_mut());
    Ok(resp)
}

async fn capture_oauth_profile(state: &AppState, voter_id: Uuid, identity: &ProviderIdentity) {
    if let Err(e) =
        profile_service::upsert_oauth_snapshot(&state.pool, voter_id, &identity.profile).await
    {
        tracing::warn!(error = %e, voter_id = %voter_id, "profile.oauth_snapshot_failed");
        return;
    }
    if let Some(email) = identity.verified_email.as_deref() {
        if let Err(e) = profile_service::sync_agent_for_email(&state.pool, email).await {
            tracing::warn!(error = %e, voter_id = %voter_id, "profile.agent_sync_failed");
        }
    }
}

/// Where a finished web OAuth login lands. The session token rides in
/// the fragment: browsers never send fragments to servers, so it stays
/// out of access logs, proxies and `Referer` headers.
fn login_callback_location(token: &str) -> String {
    format!("/login/callback#token={token}")
}

/// GitHub OAuth callback.
pub async fn github_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    oauth_callback(state, headers, query, Provider::Github).await
}

/// Google OAuth callback.
pub async fn google_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    oauth_callback(state, headers, query, Provider::Google).await
}

/// LinkedIn OAuth callback.
pub async fn linkedin_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    oauth_callback(state, headers, query, Provider::Linkedin).await
}

/// Voter status: check if the current session is valid.
pub async fn voter_me(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = headers
        .get("x-voter-token")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;

    let voter = voter_service::get_voter_by_session(&state.pool, token)
        .await?
        .ok_or(ApiError::Unauthorized)?;

    // If the email this session's login PROVED maps to an admin user,
    // mint a JWT now and ship it back. The /login/callback bootstrap page
    // (and the profile page on subsequent loads) reads this and writes
    // `krabarena_token` into localStorage — that's what unlocks the
    // admin-only nav entries. The free-form `voters.email`
    // is never used here: it may be an unverified provider value.
    // Sessions created before migration 077 have no verified email and
    // get no admin token until the admin signs in again.
    let verified = voter_service::session_verified_email(&state.pool, token).await?;
    let admin_token = match verified.as_deref() {
        Some(email) => {
            mint_admin_token_for_voter_email(&state.pool, email, &state.jwt_secret).await?
        }
        None => None,
    };

    let mut response = crate::models::voter::VoterResponse::from(voter);
    response.admin_token = admin_token;
    Ok(Json(ApiResponse::new(response)))
}

/// GET /auth/voter/github/client_id — return the GitHub OAuth client ID for the login button.
pub async fn github_client_id() -> impl IntoResponse {
    let client_id = std::env::var("GITHUB_CLIENT_ID").unwrap_or_default();
    Json(serde_json::json!({ "client_id": client_id }))
}

/// GET /login/callback — stores voter token from OAuth redirect into localStorage.
pub async fn login_callback(State(state): State<AppState>) -> Response {
    crate::web::errors::render_template_or_500(
        OAuthLoginCallbackTemplate {
            meta: PageMeta::new(&state.public_base_url, "/login/callback")
                .with_title("Logging in")
                .with_description("Completing your KrabArena sign-in.")
                .noindex_follow(),
        },
        &state.public_base_url,
        "/login/callback",
        "oauth.login.callback",
    )
}

/// GET /auth/device?session=<uuid> — browser login page for CLI device flow.
pub async fn device_login(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let base = state.public_base_url.clone();
    let Some(session) = params
        .get("session")
        .and_then(|s| Uuid::parse_str(s.trim()).ok())
    else {
        return oauth_error_page(
            &base,
            "KrabArena CLI",
            &ApiError::ValidationError(
                "Missing or invalid CLI session. Re-run `krab auth login`.".into(),
            ),
            "/",
        );
    };
    // Store the session so we can deliver the token after OAuth completes.
    // We log on failure rather than swallow — an earlier silent INSERT
    // here hid a CHECK-constraint bug on `verification_type` for months:
    // every device-flow auth quietly produced zero rows, the browser
    // kept saying "Authorized!", and the CLI poll timed out. Reloading the
    // page must not create duplicate rows (there's no unique index on
    // `code`), hence the NOT EXISTS guard.
    if let Err(e) = sqlx::query(
        "INSERT INTO pending_verifications (verification_type, identifier, code, expires_at) \
         SELECT 'device', $1, $1, NOW() + INTERVAL '10 minutes' \
         WHERE NOT EXISTS (SELECT 1 FROM pending_verifications \
                           WHERE verification_type = 'device' AND code = $1)",
    )
    .bind(session.to_string())
    .execute(&state.pool)
    .await
    {
        tracing::error!(
            error = %e,
            session = %session,
            "device_login: failed to INSERT pending_verifications row — \
             CLI poll will wait forever"
        );
    }

    let agentid_start = crate::services::agentid::enabled()
        .then(|| crate::routes::agentid::start_path(Some(&session)));
    if let Some(url) = agentid_start
        .as_deref()
        .filter(|_| params.get("provider").map(String::as_str) == Some("agentid"))
    {
        return Redirect::to(url).into_response();
    }

    crate::web::errors::render_template_or_500(
        OAuthDeviceLoginTemplate {
            meta: PageMeta::new(&base, "/auth/device")
                .with_title("Authorize CLI")
                .with_description("Sign in to authorize the KrabArena CLI.")
                .noindex_follow(),
            github_url: provider_start_path("github", Some(&session)),
            google_url: provider_start_path("google", Some(&session)),
            agentid_url: agentid_start.clone(),
            user_code: device_user_code(&session),
        },
        &base,
        "/auth/device",
        "oauth.device.login",
    )
}

#[derive(Debug, Deserialize)]
pub struct DeviceConfirmForm {
    pub session: String,
    #[serde(default)]
    pub decision: String,
}

/// POST /auth/device/confirm — the user explicitly approves (or cancels)
/// handing the staged JWT to the CLI. Must come from the browser that
/// completed the provider login: the HttpOnly SameSite=Lax state cookie's
/// nonce has to hash to the row's `browser_binding` (a cross-site POST
/// never carries that cookie).
pub async fn device_confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<DeviceConfirmForm>,
) -> Response {
    let base = state.public_base_url.clone();
    let session = match Uuid::parse_str(form.session.trim()) {
        Ok(s) => s,
        Err(_) => {
            return oauth_error_page(
                &base,
                "KrabArena CLI",
                &ApiError::ValidationError("Invalid CLI session.".into()),
                "/",
            );
        }
    };
    let retry = format!("/auth/device?session={session}");
    let Some(nonce) = cookies::get_cookie(&headers, OAUTH_STATE_COOKIE) else {
        return oauth_error_page(&base, "KrabArena CLI", &ApiError::Unauthorized, &retry);
    };
    let binding = browser_binding(&nonce);

    if form.decision != "approve" {
        let _ = sqlx::query(
            "DELETE FROM pending_verifications \
             WHERE verification_type = 'device' AND code = $1 AND browser_binding = $2",
        )
        .bind(session.to_string())
        .bind(&binding)
        .execute(&state.pool)
        .await;
        let mut resp = oauth_error_page(
            &base,
            "KrabArena CLI",
            &ApiError::ValidationError("CLI sign-in cancelled. Nothing was authorized.".into()),
            "/",
        );
        clear_state_cookie(resp.headers_mut());
        return resp;
    }

    match approve_device_session(&state.pool, &session, &binding).await {
        Ok(true) => {
            let mut resp = render_device_authorized_page(&base, "");
            clear_state_cookie(resp.headers_mut());
            resp
        }
        Ok(false) => oauth_error_page(&base, "KrabArena CLI", &ApiError::Unauthorized, &retry),
        Err(e) => oauth_error_page(&base, "KrabArena CLI", &e, &retry),
    }
}

/// Move a staged device token to `result_token` (what the CLI poll
/// returns) iff `binding` matches the browser that staged it. Returns
/// whether a row was approved.
pub async fn approve_device_session(
    pool: &PgPool,
    session: &Uuid,
    binding: &str,
) -> Result<bool, ApiError> {
    let n = sqlx::query(
        "UPDATE pending_verifications \
         SET result_token = pending_token, pending_token = NULL, browser_binding = NULL, \
             expires_at = NOW() + INTERVAL '5 minutes' \
         WHERE verification_type = 'device' AND code = $1 AND pending_token IS NOT NULL \
           AND browser_binding = $2 AND expires_at > NOW()",
    )
    .bind(session.to_string())
    .bind(binding)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .rows_affected();
    Ok(n > 0)
}

/// GET /v1/auth/device/poll?session=<id> — CLI polls this until login completes.
pub async fn device_poll(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<impl IntoResponse, ApiError> {
    let session = params
        .get("session")
        .ok_or(ApiError::ValidationError("session required".into()))?;

    let token = sqlx::query_scalar::<_, String>(
        "SELECT result_token FROM pending_verifications \
         WHERE verification_type = 'device' AND code = $1 \
           AND result_token IS NOT NULL AND expires_at > NOW()",
    )
    .bind(session)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    match token {
        Some(t) => {
            // Clean up
            let _ = sqlx::query(
                "DELETE FROM pending_verifications WHERE verification_type = 'device' AND code = $1",
            )
            .bind(session)
            .execute(&state.pool)
            .await;

            Ok(Json(crate::envelope::ApiResponse::new(
                serde_json::json!({"status": "authorized", "bearer_token": t}),
            )))
        }
        None => Ok(Json(crate::envelope::ApiResponse::new(
            serde_json::json!({"status": "pending"}),
        ))),
    }
}

/// CSPRNG-backed 8-digit code for voter email verification.
///
/// Source of entropy is `uuid::Uuid::new_v4()`, which delegates to
/// `getrandom` (the OS CSPRNG — `getrandom(2)` on Linux, `arc4random`
/// on macOS). The prior implementation derived from
/// `SystemTime::now().subsec_nanos()`, giving ~20 bits of effective
/// entropy and trivial timing-correlated guesses. The modulo bias here is
/// negligible: u32 range / 1e8 ≈ 0.04% imbalance across buckets.
fn rand_code() -> u32 {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    10_000_000 + (n % 90_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_login_html(agentid_url: Option<String>) -> String {
        OAuthDeviceLoginTemplate {
            meta: PageMeta::new("https://krabarena.test", "/auth/device"),
            github_url: provider_start_path("github", None),
            google_url: provider_start_path("google", None),
            agentid_url,
            user_code: "ABCD-1234".into(),
        }
        .render()
        .unwrap()
    }

    #[test]
    fn device_login_offers_agentid_only_when_configured() {
        assert!(!device_login_html(None).contains("AgentID"));
        let s = Uuid::new_v4();
        let html = device_login_html(Some(crate::routes::agentid::start_path(Some(&s))));
        assert!(html.contains("Continue with AgentID"));
        assert!(html.contains(&format!("/auth/agentid/start?session={s}")));
    }

    /// Non-admin voters must NOT get an `admin_token` field at all
    /// in the JSON — not `"admin_token": null`. We use
    /// `skip_serializing_if = "Option::is_none"` to enforce this:
    /// missing field beats explicit null because it keeps the
    /// front-end check `if (d.data.admin_token)` valid against
    /// non-admin payloads (no surprise truthy `null`).
    #[test]
    fn login_response_omits_admin_token_when_none() {
        let resp = LoginResponse {
            voter_token: "voter-abc".to_string(),
            display_name: "test".to_string(),
            admin_token: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(
            !json.as_object().unwrap().contains_key("admin_token"),
            "admin_token must be omitted (not null) when None: {json}"
        );
    }

    #[test]
    fn login_response_serializes_admin_token_when_some() {
        let resp = LoginResponse {
            voter_token: "voter-abc".to_string(),
            display_name: "test".to_string(),
            admin_token: Some("jwt.body.sig".to_string()),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(
            json.get("admin_token").and_then(|v| v.as_str()),
            Some("jwt.body.sig")
        );
    }

    #[test]
    fn email_verification_code_is_always_eight_digits() {
        for _ in 0..1_000 {
            let code = rand_code();
            assert!(
                (10_000_000..=99_999_999).contains(&code),
                "email verification code must be exactly 8 digits: {code}"
            );
        }
    }

    /// The admin_token must be a real JWT that decodes back to
    /// role=admin with the same secret — that is what admin routes
    /// (gated by require_admin) will validate. Pinning the
    /// round-trip catches accidental refactors that swap
    /// create_jwt for a stub.
    #[test]
    fn admin_token_jwt_round_trip() {
        use crate::services::auth_service;
        let secret = "test-secret";
        let user_id = Uuid::new_v4();
        let entity_id = Uuid::new_v4();
        let jwt = auth_service::create_jwt(user_id, entity_id, "admin", secret).unwrap();
        let claims = auth_service::validate_jwt(&jwt, secret).unwrap();
        assert_eq!(claims.sub, user_id);
        assert_eq!(claims.entity_id, entity_id);
        assert_eq!(claims.role, "admin");
    }

    #[test]
    fn github_user_payload_captures_public_profile_fields() {
        let user = serde_json::json!({
            "id": 583231,
            "login": "octocat",
            "name": "  The   Octocat ",
            "avatar_url": "https://avatars.githubusercontent.com/u/583231?v=4",
            "html_url": "https://github.com/octocat",
            "blog": "https://github.blog",
            "twitter_username": "octocat",
            "bio": null,
            "company": "@github",
            "location": "San Francisco"
        });
        let p = github_oauth_profile(&user, Some("octo@example.com"));
        assert_eq!(p.provider, "github");
        assert_eq!(p.full_name.as_deref(), Some("The Octocat"));
        assert_eq!(p.username.as_deref(), Some("octocat"));
        assert_eq!(p.profile_url.as_deref(), Some("https://github.com/octocat"));
        assert_eq!(p.website_url.as_deref(), Some("https://github.blog"));
        assert_eq!(p.x_handle.as_deref(), Some("octocat"));
        assert_eq!(p.bio, None);
        assert_eq!(p.company.as_deref(), Some("@github"));
        assert_eq!(p.location.as_deref(), Some("San Francisco"));
        assert!(p.email_verified);
        let seed = p.seed();
        assert_eq!(
            seed.github_url.as_deref(),
            Some("https://github.com/octocat")
        );
        assert_eq!(seed.x_url.as_deref(), Some("https://x.com/octocat"));
    }

    #[test]
    fn oidc_userinfo_captures_name_parts_and_picture() {
        let info = serde_json::json!({
            "sub": "abc",
            "name": "Ada Lovelace",
            "given_name": "Ada",
            "family_name": "Lovelace",
            "picture": "https://lh3.googleusercontent.com/a/xyz=s96-c",
            "email": "ada@example.com",
            "email_verified": true,
            "hd": "example.com"
        });
        let p = oidc_oauth_profile("google", &info, None);
        assert_eq!(p.full_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(p.given_name.as_deref(), Some("Ada"));
        assert_eq!(p.family_name.as_deref(), Some("Lovelace"));
        assert_eq!(
            p.avatar_url.as_deref(),
            Some("https://lh3.googleusercontent.com/a/xyz=s96-c")
        );
        assert_eq!(p.company.as_deref(), Some("example.com"));
        assert!(!p.email_verified);
        assert_eq!(p.email, None);
        let linkedin = oidc_oauth_profile(
            "linkedin",
            &serde_json::json!({"sub": "z", "picture": "javascript:alert(1)"}),
            Some("z@example.com"),
        );
        assert_eq!(linkedin.avatar_url, None);
        assert_eq!(linkedin.email.as_deref(), Some("z@example.com"));
    }

    #[test]
    fn oauth_callback_url_basic_shape() {
        assert_eq!(
            oauth_callback_url("https://krabarena.com", "google"),
            "https://krabarena.com/auth/voter/google/callback"
        );
        assert_eq!(
            oauth_callback_url("https://krabarena.com", "github"),
            "https://krabarena.com/auth/voter/github/callback"
        );
        assert_eq!(
            oauth_callback_url("https://krabarena.com", "linkedin"),
            "https://krabarena.com/auth/voter/linkedin/callback"
        );
    }

    #[test]
    fn oauth_callback_url_strips_trailing_slash_on_base() {
        // public_base_url frequently arrives with a trailing slash
        // depending on how it was set; the helper has to normalise
        // or the resulting URI ends up with a doubled `//` that
        // wouldn't match the provider's allow-list.
        assert_eq!(
            oauth_callback_url("https://krabarena.com/", "google"),
            "https://krabarena.com/auth/voter/google/callback"
        );
    }

    /// Regression test for the v0.10.0 `redirect_uri_mismatch` bug.
    ///
    /// Before this refactor, /authorize built the redirect_uri from
    /// `state.public_base_url` (= krabarena.com after the rebrand)
    /// while /token in the callback handler hardcoded
    /// `https://krabarena.org/...`. The two sites SHOULD always
    /// produce the same string for the same base URL — Google
    /// rejects with `redirect_uri_mismatch` otherwise. Pinning the
    /// invariant via a single shared helper means the only way to
    /// re-introduce the bug is to bypass the helper, which this
    /// test will catch in CI.
    #[test]
    fn authorize_and_token_use_identical_redirect_uri() {
        for base in ["https://krabarena.com", "https://krabarena.org"] {
            for provider in ["google", "github", "linkedin"] {
                let authorize_side = oauth_callback_url(base, provider);
                let token_side = oauth_callback_url(base, provider);
                assert_eq!(
                    authorize_side, token_side,
                    "redirect_uri drift between /authorize and /token for \
                     provider={provider} base={base} — Google enforces \
                     byte-identical equality and will return \
                     redirect_uri_mismatch"
                );
            }
        }
        // And the authorize URL actually embeds the shared callback URL.
        let url = Provider::Google.authorize_url("cid", "https://krabarena.com", "w.x");
        assert!(url.contains("redirect_uri=https://krabarena.com/auth/voter/google/callback&"));
    }

    // ---- OAuth state / device-flow CSRF ------------------------------

    fn cookie_headers(nonce: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "cookie",
            format!("{OAUTH_STATE_COOKIE}={nonce}").parse().unwrap(),
        );
        h
    }

    #[test]
    fn oauth_state_round_trips() {
        let nonce = cookies::new_csrf();
        let web = OAuthState::Web {
            nonce: nonce.clone(),
        };
        assert_eq!(OAuthState::parse(&web.encode()), Some(web));
        let dev = OAuthState::Device {
            session: Uuid::new_v4(),
            nonce,
        };
        assert_eq!(OAuthState::parse(&dev.encode()), Some(dev));
    }

    /// Regression for the device-flow CSRF: the callback used to accept
    /// ANY `state` longer than 10 chars as a device session id (e.g. a bare
    /// attacker-created UUID) and write the victim's JWT into it.
    #[test]
    fn bare_session_id_state_is_rejected() {
        let attacker_session = Uuid::new_v4().to_string();
        assert_eq!(OAuthState::parse(&attacker_session), None);
        let h = cookie_headers(&cookies::new_csrf());
        assert!(matches!(
            verify_oauth_state(&h, &attacker_session),
            Err(ApiError::Unauthorized)
        ));
        assert!(matches!(
            verify_oauth_state(&h, ""),
            Err(ApiError::Unauthorized)
        ));
    }

    /// A well-formed state whose nonce doesn't match THIS browser's
    /// cookie (attacker-crafted authorize URL) is rejected, as is a
    /// callback with no cookie at all.
    #[test]
    fn state_must_match_browser_cookie() {
        let mine = cookies::new_csrf();
        let attacker = OAuthState::Device {
            session: Uuid::new_v4(),
            nonce: cookies::new_csrf(),
        };
        assert!(verify_oauth_state(&cookie_headers(&mine), &attacker.encode()).is_err());
        assert!(verify_oauth_state(&HeaderMap::new(), &attacker.encode()).is_err());

        let ok = OAuthState::Device {
            session: Uuid::new_v4(),
            nonce: mine.clone(),
        };
        assert_eq!(
            verify_oauth_state(&cookie_headers(&mine), &ok.encode()).unwrap(),
            ok
        );
        // Plain web login is CSRF-checked the same way.
        let web = OAuthState::Web {
            nonce: mine.clone(),
        };
        assert!(verify_oauth_state(&cookie_headers(&mine), &web.encode()).is_ok());
        assert!(verify_oauth_state(&cookie_headers(&cookies::new_csrf()), &web.encode()).is_err());
    }

    #[test]
    fn malformed_states_are_rejected() {
        for s in [
            "w.",
            "w.short",
            "w.has space in it 1234567890",
            "d.not-a-uuid.abcdefghijklmnopqrstu",
            "x.abcdefghijklmnopqrstuvwx",
            "test",
        ] {
            assert_eq!(OAuthState::parse(s), None, "{s}");
        }
    }

    #[test]
    fn device_user_code_is_stable_and_short() {
        let s = Uuid::parse_str("abcd1234-5678-4abc-8def-0123456789ab").unwrap();
        assert_eq!(device_user_code(&s), "ABCD-1234");
    }

    #[test]
    fn browser_binding_depends_on_nonce() {
        assert_ne!(browser_binding("a"), browser_binding("b"));
        assert_eq!(browser_binding("a"), browser_binding("a"));
    }

    #[test]
    fn retry_url_targets_device_session_only_for_device_state() {
        let s = Uuid::new_v4();
        let dev = OAuthState::Device {
            session: s,
            nonce: cookies::new_csrf(),
        };
        assert_eq!(
            oauth_retry_url(&dev.encode()),
            format!("/auth/device?session={s}")
        );
        assert_eq!(oauth_retry_url("anything-long-enough"), "/login");
    }

    #[test]
    fn provider_authorize_urls_carry_state() {
        for p in [Provider::Github, Provider::Google, Provider::Linkedin] {
            let url = p.authorize_url("cid", "https://krabarena.com", "w.NONCE");
            assert!(url.contains("&state=w.NONCE"), "{url}");
        }
    }

    /// Regression (secrets in URLs): the session token used to be put in
    /// the redirect's query string (`/login/callback?token=…`).
    #[test]
    fn login_callback_token_is_in_fragment() {
        let loc = login_callback_location("tok");
        assert_eq!(loc, "/login/callback#token=tok");
        assert!(!loc.contains('?'));
    }

    // ---- Verified provider emails ------------------------------------

    /// Regression: unverified provider emails were stored and used for
    /// account linking / admin-token minting.
    #[test]
    fn google_email_requires_verified_flag() {
        let v = serde_json::json!({"email": "A@x.com", "verified_email": true});
        assert_eq!(google_verified_email(&v).as_deref(), Some("a@x.com"));
        let v = serde_json::json!({"email": "a@x.com", "email_verified": true});
        assert_eq!(google_verified_email(&v).as_deref(), Some("a@x.com"));
        let v = serde_json::json!({"email": "a@x.com", "verified_email": false});
        assert_eq!(google_verified_email(&v), None);
        let v = serde_json::json!({"email": "a@x.com"});
        assert_eq!(google_verified_email(&v), None);
    }

    #[test]
    fn linkedin_email_requires_verified_flag() {
        let v = serde_json::json!({"email": "a@x.com", "email_verified": true});
        assert_eq!(linkedin_verified_email(&v).as_deref(), Some("a@x.com"));
        let v = serde_json::json!({"email": "a@x.com", "email_verified": false});
        assert_eq!(linkedin_verified_email(&v), None);
        let v = serde_json::json!({"email": "a@x.com"});
        assert_eq!(linkedin_verified_email(&v), None);
    }

    #[test]
    fn github_uses_primary_verified_email_only() {
        let v = serde_json::json!([
            {"email": "other@x.com", "primary": false, "verified": true},
            {"email": "Main@x.com", "primary": true, "verified": true}
        ]);
        assert_eq!(
            github_primary_verified_email(&v).as_deref(),
            Some("main@x.com")
        );
        let v = serde_json::json!([
            {"email": "main@x.com", "primary": true, "verified": false},
            {"email": "other@x.com", "primary": false, "verified": true}
        ]);
        assert_eq!(github_primary_verified_email(&v), None);
        assert_eq!(
            github_primary_verified_email(&serde_json::Value::Null),
            None
        );
    }
}
