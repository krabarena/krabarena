use askama::Template;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::services::auth_service;
use crate::web::meta::PageMeta;

// ---------------------------------------------------------------------
// Live (DB-backed) authorization state
// ---------------------------------------------------------------------
//
// JWTs live for 30 days and carry the `role` claim they were minted
// with. Trusting that claim meant a demoted admin, a suspended user, or a
// deleted user/bot kept their old privileges until the token expired.
// The auth middleware (`web::auth_context::auth_middleware`) now resolves
// the token's `sub` against `users` once per request (PK lookup, cached
// in-process for at most `LIVE_AUTH_TTL`) and exposes the result through
// a task-local. The sync helpers below (`require_admin`,
// `require_editor`, `extract_user_with_role`, `extract_user_id`) read it
// so their signatures — and ~150 call sites — stay unchanged.

/// Current, DB-sourced authorization facts for a JWT subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveAuth {
    /// The user row exists. `role` is the CURRENT role; `suspended` is
    /// `suspended_until > NOW()`.
    Found { role: String, suspended: bool },
    /// No `users` row for the subject (deleted user / merged bot).
    Missing,
    /// The DB lookup failed — privileged helpers fail closed.
    LookupFailed,
}

tokio::task_local! {
    /// `(jwt_sub, live_state)` for the current request, set by the auth
    /// middleware. Absent outside a request (e.g. direct unit tests).
    static LIVE_AUTH: Option<(Uuid, LiveAuth)>;
}

/// Max staleness of the in-process cache (spec: ≤ 60s).
pub const LIVE_AUTH_TTL: Duration = Duration::from_secs(30);
const LIVE_AUTH_CACHE_MAX: usize = 20_000;

fn live_cache() -> &'static Mutex<HashMap<Uuid, (Instant, LiveAuth)>> {
    static CACHE: OnceLock<Mutex<HashMap<Uuid, (Instant, LiveAuth)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop a cached entry so a role change / suspension applies to the
/// next request on this instance immediately (other instances converge
/// within `LIVE_AUTH_TTL`).
pub fn invalidate_live_auth(user_id: Uuid) {
    if let Ok(mut c) = live_cache().lock() {
        c.remove(&user_id);
    }
}

/// Resolve the current role / suspension / existence of `user_id`. One
/// PK-indexed query, cached for `LIVE_AUTH_TTL`. Lookup failures are
/// not cached.
pub async fn lookup_live_auth(pool: &PgPool, user_id: Uuid) -> LiveAuth {
    if let Ok(c) = live_cache().lock()
        && let Some((at, v)) = c.get(&user_id)
        && at.elapsed() < LIVE_AUTH_TTL
    {
        return v.clone();
    }
    let row: Result<Option<(String, bool)>, sqlx::Error> = sqlx::query_as(
        "SELECT role, COALESCE(suspended_until > NOW(), FALSE) FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await;
    let v = match row {
        Ok(Some((role, suspended))) => LiveAuth::Found { role, suspended },
        Ok(None) => LiveAuth::Missing,
        Err(e) => {
            tracing::error!(error = %e, user_id = %user_id, "auth.live_lookup_failed");
            return LiveAuth::LookupFailed;
        }
    };
    if let Ok(mut c) = live_cache().lock() {
        if c.len() >= LIVE_AUTH_CACHE_MAX {
            c.retain(|_, (at, _)| at.elapsed() < LIVE_AUTH_TTL);
            if c.len() >= LIVE_AUTH_CACHE_MAX {
                c.clear();
            }
        }
        c.insert(user_id, (Instant::now(), v.clone()));
    }
    v
}

/// Read the bearer JWT from the request headers (same precedence as
/// every auth helper: `x-krabarena-token`, then `Authorization: Bearer`).
pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-krabarena-token")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        })
}

/// Run `fut` with the live auth state for this request's JWT (if any)
/// in scope. Called by the auth middleware around `next.run(req)`.
pub async fn with_live_auth<F: std::future::Future>(
    pool: &PgPool,
    headers: &HeaderMap,
    jwt_secret: &str,
    fut: F,
) -> F::Output {
    let live =
        match bearer_token(headers).and_then(|t| auth_service::validate_jwt(t, jwt_secret).ok()) {
            Some(claims) => Some((claims.sub, lookup_live_auth(pool, claims.sub).await)),
            None => None,
        };
    LIVE_AUTH.scope(live, fut).await
}

/// Run `fut` with an explicit live state (tests).
#[cfg(test)]
pub async fn scope_live_auth<F: std::future::Future>(
    live: Option<(Uuid, LiveAuth)>,
    fut: F,
) -> F::Output {
    LIVE_AUTH.scope(live, fut).await
}

/// What the helpers know about the current role.
#[derive(Debug, PartialEq, Eq)]
enum EffectiveRole {
    /// Verified against the DB just now (≤ TTL).
    Live { role: String, suspended: bool },
    /// No live state available (outside a request scope).
    ClaimOnly(String),
    /// Live lookup failed.
    Unknown,
}

/// Combine the JWT claims with the task-local live state. A missing
/// user is rejected outright.
fn effective_role(
    claims: &auth_service::JwtClaims,
    live: Option<Option<(Uuid, LiveAuth)>>,
) -> Result<EffectiveRole, ApiError> {
    match live {
        Some(Some((sub, state))) if sub == claims.sub => match state {
            LiveAuth::Found { role, suspended } => Ok(EffectiveRole::Live { role, suspended }),
            LiveAuth::Missing => Err(ApiError::Unauthorized),
            LiveAuth::LookupFailed => Ok(EffectiveRole::Unknown),
        },
        _ => Ok(EffectiveRole::ClaimOnly(claims.role.clone())),
    }
}

fn current_effective_role(claims: &auth_service::JwtClaims) -> Result<EffectiveRole, ApiError> {
    effective_role(claims, LIVE_AUTH.try_with(|v| v.clone()).ok())
}

/// Role to use for a privileged decision. Suspended users are treated as
/// plain `user`; an unknown live state fails closed. A claim-only role
/// (no live state in scope) is trusted only in unit tests — in a real
/// request the auth middleware always sets the scope, so reaching this
/// branch in production means the helper ran outside the middleware and
/// we refuse rather than trust a possibly-revoked claim.
fn privileged_role(eff: EffectiveRole) -> Result<String, ApiError> {
    match eff {
        EffectiveRole::Live {
            suspended: true, ..
        } => Ok("user".to_string()),
        EffectiveRole::Live { role, .. } => Ok(role),
        EffectiveRole::ClaimOnly(role) if cfg!(test) => Ok(role),
        EffectiveRole::ClaimOnly(_) => {
            tracing::error!("auth.live_state_missing: role check outside auth middleware scope");
            Err(ApiError::Internal(anyhow::anyhow!(
                "authorization state unavailable"
            )))
        }
        EffectiveRole::Unknown => Err(ApiError::Internal(anyhow::anyhow!(
            "authorization state unavailable"
        ))),
    }
}

/// Extracts the authenticated user ID from the Authorization or
/// X-Krabbit-Token header (fallback when Cloud Run IAM occupies
/// the Authorization header). Rejects tokens whose user no longer
/// exists.
pub fn extract_user_id(headers: &HeaderMap, jwt_secret: &str) -> Result<Uuid, ApiError> {
    let claims = extract_claims(headers, jwt_secret)?;
    current_effective_role(&claims)?;
    Ok(claims.sub)
}

/// Extracts user ID and the user's CURRENT role (from the DB, not the
/// possibly-stale JWT claim). Suspended users get `"user"`.
pub fn extract_user_with_role(
    headers: &HeaderMap,
    jwt_secret: &str,
) -> Result<(Uuid, String), ApiError> {
    let claims = extract_claims(headers, jwt_secret)?;
    let role = privileged_role(current_effective_role(&claims)?)?;
    Ok((claims.sub, role))
}

/// Extracts user ID and requires admin role.
pub fn require_admin(headers: &HeaderMap, jwt_secret: &str) -> Result<Uuid, ApiError> {
    let (user_id, role) = extract_user_with_role(headers, jwt_secret)?;
    if role != "admin" {
        return Err(ApiError::Forbidden);
    }
    Ok(user_id)
}

/// Extracts user ID and requires editor or admin role — the gate for
/// opening Battles. Sites with no promoted editors rely on their bootstrap
/// admin promoting users via `PATCH /v1/admin/users/{id}`.
pub fn require_editor(headers: &HeaderMap, jwt_secret: &str) -> Result<Uuid, ApiError> {
    let (user_id, role) = extract_user_with_role(headers, jwt_secret)?;
    if role != "admin" && role != "editor" {
        return Err(ApiError::Forbidden);
    }
    Ok(user_id)
}

fn extract_claims(
    headers: &HeaderMap,
    jwt_secret: &str,
) -> Result<auth_service::JwtClaims, ApiError> {
    let token = bearer_token(headers).ok_or(ApiError::Unauthorized)?;
    auth_service::validate_jwt(token, jwt_secret)
}

#[derive(Debug, Deserialize)]
pub struct EmailRequest {
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyEmailRequest {
    pub token: String,
}

pub async fn initiate_email(
    State(state): State<AppState>,
    Json(req): Json<EmailRequest>,
) -> Result<impl IntoResponse, ApiError> {
    auth_service::initiate_email_auth(&state.pool, &req.email, state.email_service.as_ref())
        .await?;
    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "message": "Verification email sent"
        }))),
    ))
}

pub async fn verify_email(
    State(state): State<AppState>,
    Json(req): Json<VerifyEmailRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let result = auth_service::verify_email(&state.pool, &req.token, &state.jwt_secret).await?;
    Ok((StatusCode::OK, Json(ApiResponse::new(result))))
}

/// POST /auth/migrate — one-time upgrade of a legacy `localStorage` voter
/// token to the HttpOnly `ka_session` cookie. The migrating frontend
/// sends the old token in `x-voter-token`; we validate it against
/// `voter_sessions` and, on success, set the session + CSRF cookies.
pub async fn migrate(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let token = headers
        .get("x-voter-token")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    crate::services::voter_service::get_voter_by_session(&state.pool, token)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let mut resp = StatusCode::OK.into_response();
    crate::web::cookies::set_login_cookies(resp.headers_mut(), token);
    Ok(resp)
}

/// GET /auth/verify?token=<code> — magic link from email, verifies and shows the JWT.
#[derive(Debug, Deserialize)]
pub struct VerifyQuery {
    pub token: String,
}

#[derive(Template)]
#[template(path = "pages/auth_email_verified.html")]
struct AuthEmailVerifiedTemplate {
    meta: PageMeta,
    name: String,
    voter_token: Option<String>,
    admin_token: Option<String>,
    cli_token: String,
}

#[derive(Template)]
#[template(path = "pages/auth_email_failed.html")]
struct AuthEmailFailedTemplate {
    meta: PageMeta,
}

pub async fn verify_email_link(
    State(state): State<AppState>,
    Query(query): Query<VerifyQuery>,
) -> impl IntoResponse {
    match auth_service::verify_email(&state.pool, &query.token, &state.jwt_secret).await {
        Ok(result) => {
            // The magic link is shared by the agent/CLI flow (which wants the
            // JWT for `krab auth set-token`) and the web voter login. KrabArena
            // has TWO identity systems: agents live in `users`/`entities` and
            // authenticate with a JWT; human voters live in `voters` and
            // authenticate with an opaque `voter_sessions` token. The website's
            // `voter_token` MUST be a voter-session token — storing a JWT there
            // logged the user in "weirdly" (the UI showed logged-in, but every
            // voter action failed to resolve the session because the JWT isn't
            // a row in `voter_sessions`). So mint a real voter session here, the
            // same way the code-entry login (`oauth::email_verify`) does.
            let email = result.user.email.clone().unwrap_or_default();
            let (voter_name, voter_token) = if email.is_empty() {
                (result.user.display_name.clone(), None)
            } else {
                let display_name = email.split('@').next().unwrap_or("voter").to_string();
                match crate::services::voter_service::find_or_create_voter(
                    &state.pool,
                    "email",
                    &email,
                    &display_name,
                    Some(&email),
                    None,
                )
                .await
                {
                    Ok(voter) => {
                        // The magic link proves control of `email`.
                        match crate::services::voter_service::create_session(
                            &state.pool,
                            voter.id,
                            Some(&email),
                        )
                        .await
                        {
                            Ok(token) => (voter.display_name, Some(token)),
                            Err(_) => (result.user.display_name.clone(), None),
                        }
                    }
                    Err(_) => (result.user.display_name.clone(), None),
                }
            };
            // Admin surfaces are gated by the *agent* JWT with role=admin —
            // `verify_email` already minted exactly that as `bearer_token`.
            let admin_token = if result.user.role == "admin" {
                Some(result.bearer_token.clone())
            } else {
                None
            };
            let cookie_token = voter_token.clone();
            let mut resp = crate::web::errors::render_template_or_500(
                verified_page_template(
                    &state.public_base_url,
                    &voter_name,
                    voter_token,
                    admin_token,
                    result.bearer_token,
                ),
                &state.public_base_url,
                "/auth/verify",
                "auth.email.verified",
            );
            if let Some(t) = cookie_token {
                crate::web::cookies::set_login_cookies(resp.headers_mut(), &t);
            }
            resp
        }
        Err(_) => crate::web::errors::render_template_or_500(
            AuthEmailFailedTemplate {
                meta: PageMeta::new(&state.public_base_url, "/auth/verify")
                    .with_title("Verification failed")
                    .with_description(
                        "The KrabArena email verification link is invalid or expired.",
                    )
                    .noindex_follow(),
            },
            &state.public_base_url,
            "/auth/verify",
            "auth.email.failed",
        ),
    }
}

/// Renders the post-email-verification page.
///
/// Web login (the bug fixed 2026-06-09): the email magic link is shared
/// by the web voter login (`oauth::email_login`, 8-digit code) and the
/// CLI flow (`auth_service::initiate_email_auth`, UUID code). It used to
/// render a CLI-token-only page, so a voter who clicked the link in their
/// email landed on a dead-end "here's your CLI token" screen instead of
/// being logged into the website. We now store the same `localStorage`
/// keys the site's code-entry flow sets (`voter_token`, `voter_name`,
/// plus `krabarena_token` for admins — see about.html / login.html) and
/// lead with a "Continue to KrabArena" button, so the link logs the user
/// straight into the site. The CLI token stays available below for `krab`
/// users.
///
/// Bug guard (mobile overflow): the markup had no `width=device-width`
/// viewport and the token / command lived in plain `<pre>` blocks
/// (default `white-space: pre`), so on a phone the long JWT ran off the
/// right edge instead of wrapping. We ship a viewport meta and
/// `white-space: pre-wrap; overflow-wrap: anywhere` so both blocks wrap,
/// plus a Copy button on each.
/// `voter_token` — the opaque `voter_sessions` token for the website
/// session (NOT a JWT). `admin_token` — the agent JWT with role=admin,
/// stored as `krabarena_token` to unlock admin surfaces (admins only).
/// `cli_token` — the agent JWT shown in the CLI section for `krab`.
fn verified_page_template(
    public_base_url: &str,
    name: &str,
    voter_token: Option<String>,
    admin_token: Option<String>,
    cli_token: String,
) -> AuthEmailVerifiedTemplate {
    AuthEmailVerifiedTemplate {
        meta: PageMeta::new(public_base_url, "/auth/verify")
            .with_title("Signed in")
            .with_description("Your KrabArena email sign-in is complete.")
            .noindex_follow(),
        name: name.to_string(),
        voter_token,
        admin_token,
        cli_token,
    }
}

#[cfg(test)]
fn render_verified_page(
    name: &str,
    voter_token: Option<&str>,
    admin_token: Option<&str>,
    cli_token: &str,
) -> String {
    verified_page_template(
        "https://krabarena.test",
        name,
        voter_token.map(str::to_string),
        admin_token.map(str::to_string),
        cli_token.to_string(),
    )
    .render()
    .expect("verified email template renders")
}

// `GET /v1/auth/email/poll?email=X` was removed (0.45.6): it handed the
// freshly minted JWT to anyone who knew the email address (no nonce, no
// session binding), so an attacker polling an admin's address could
// steal the token the moment the admin clicked any sign-in link. The CLI
// logs in via the device flow and never called it.

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(sub: Uuid, role: &str) -> auth_service::JwtClaims {
        auth_service::JwtClaims {
            sub,
            entity_id: Uuid::new_v4(),
            role: role.to_string(),
            exp: 0,
            iat: 0,
            via: None,
        }
    }

    fn found(role: &str, suspended: bool) -> LiveAuth {
        LiveAuth::Found {
            role: role.to_string(),
            suspended,
        }
    }

    /// Regression (token revocation): a 30-day JWT minted with
    /// role=admin used to keep admin rights after demotion. The live DB
    /// role must win over the claim.
    #[test]
    fn live_role_overrides_stale_admin_claim() {
        let sub = Uuid::new_v4();
        let c = claims(sub, "admin");
        let eff = effective_role(&c, Some(Some((sub, found("user", false))))).unwrap();
        assert_eq!(privileged_role(eff).unwrap(), "user");
    }

    #[test]
    fn suspended_user_loses_privileged_role() {
        let sub = Uuid::new_v4();
        let c = claims(sub, "admin");
        let eff = effective_role(&c, Some(Some((sub, found("admin", true))))).unwrap();
        assert_eq!(privileged_role(eff).unwrap(), "user");
    }

    #[test]
    fn deleted_user_token_is_rejected() {
        let sub = Uuid::new_v4();
        let c = claims(sub, "admin");
        assert!(matches!(
            effective_role(&c, Some(Some((sub, LiveAuth::Missing)))),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn lookup_failure_fails_closed_for_privileged_checks() {
        let sub = Uuid::new_v4();
        let c = claims(sub, "admin");
        let eff = effective_role(&c, Some(Some((sub, LiveAuth::LookupFailed)))).unwrap();
        assert!(privileged_role(eff).is_err());
    }

    #[test]
    fn live_state_for_other_subject_is_ignored() {
        let c = claims(Uuid::new_v4(), "editor");
        let eff = effective_role(&c, Some(Some((Uuid::new_v4(), LiveAuth::Missing)))).unwrap();
        assert_eq!(eff, EffectiveRole::ClaimOnly("editor".into()));
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Bearer {token}").parse().unwrap());
        h
    }

    /// End-to-end through the task-local: `require_admin` /
    /// `require_editor` consult the live state set by the middleware.
    #[tokio::test]
    async fn require_admin_uses_live_role_in_request_scope() {
        let secret = "s";
        let sub = Uuid::new_v4();
        let jwt = auth_service::create_jwt(sub, Uuid::new_v4(), "admin", secret).unwrap();
        let h = bearer(&jwt);

        let demoted = scope_live_auth(Some((sub, found("user", false))), async {
            require_admin(&h, secret)
        })
        .await;
        assert!(matches!(demoted, Err(ApiError::Forbidden)));

        let editor_ok = scope_live_auth(Some((sub, found("editor", false))), async {
            require_editor(&h, secret)
        })
        .await;
        assert_eq!(editor_ok.unwrap(), sub);

        let gone = scope_live_auth(Some((sub, LiveAuth::Missing)), async {
            extract_user_id(&h, secret)
        })
        .await;
        assert!(matches!(gone, Err(ApiError::Unauthorized)));

        let still_admin = scope_live_auth(Some((sub, found("admin", false))), async {
            require_admin(&h, secret)
        })
        .await;
        assert_eq!(still_admin.unwrap(), sub);
    }

    // Regression guard for the mobile-overflow bug: the verified page
    // must declare a device-width viewport and render the token /
    // command in wrapping blocks (pre-wrap), and must expose a copy
    // affordance for both. Without these, the long JWT ran off the
    // right edge of a phone screen and there was no easy way to copy it.
    #[test]
    fn verified_page_wraps_and_offers_copy() {
        let token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.payload.sig";
        let html = render_verified_page("andrey", Some("voter-session-abc"), None, token);

        // Fits to screen on mobile.
        assert!(
            html.contains(r#"name="viewport""#) && html.contains("width=device-width"),
            "missing mobile viewport meta"
        );
        // Multiline wrapping instead of a single overflowing line.
        assert!(
            html.matches("copy-code").count() >= 2,
            "token + command blocks must both use the shared copy-code component"
        );

        // Copy affordance for both the raw token and the full command.
        assert_eq!(
            html.matches("data-copy-target").count(),
            2,
            "expected a Copy button wired for both the token and the command"
        );

        // The command actually contains the token, and the token is shown.
        assert!(html.contains(&format!("krab auth set-token {token}")));
        assert!(html.contains(token));
        assert!(html.contains("andrey"));
    }

    // Regression guard for the web-voter dead-end found via the prod
    // auth E2E (2026-06-09): a voter who clicks the email magic link must
    // end up *logged into the website*, not stranded on a CLI-only token
    // page. The verified page therefore stores the same localStorage keys
    // the site's code-entry flow sets and offers a Continue button.
    #[test]
    fn verified_page_logs_into_website() {
        // The CLI/agent JWT and the website voter-session token are DISTINCT:
        // `voter_token` must be the opaque voter session (a row in
        // `voter_sessions`), never the agent JWT — storing the JWT there was
        // the "weird login" bug (UI logged-in, voter actions broken).
        let agent_jwt = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.payload.sig";
        let voter_session = "11111111-2222-3333-4444-555555555555";

        let html = render_verified_page("andrey", Some(voter_session), None, agent_jwt);
        // voter_token is the VOTER SESSION, not the agent JWT.
        assert!(
            html.contains(&format!("data-voter-token=\"{voter_session}\"")),
            "voter_token must be the opaque voter-session token"
        );
        assert!(
            !html.contains(&format!("data-voter-token=\"{agent_jwt}\"")),
            "voter_token must NOT be the agent JWT (the bug we are guarding)"
        );
        assert!(html.contains("data-voter-name=\"andrey\""));
        // Prominent path back into the app, logged in.
        assert!(
            html.contains("Continue to KrabArena"),
            "must offer a Continue button, not just a CLI token"
        );
        // A non-admin must NOT receive the admin-surface token.
        assert!(
            !html.contains("data-admin-token"),
            "non-admins must not be granted the admin token"
        );
        // The agent JWT is still available for `krab` users in the CLI section.
        assert!(html.contains(&format!("krab auth set-token {agent_jwt}")));

        // Admins unlock admin surfaces via krabarena_token = the agent JWT.
        let admin_html =
            render_verified_page("admin", Some(voter_session), Some(agent_jwt), agent_jwt);
        assert!(
            admin_html.contains(&format!("data-admin-token=\"{agent_jwt}\"")),
            "admins must get krabarena_token (agent JWT) to unlock admin UI"
        );
    }
}
