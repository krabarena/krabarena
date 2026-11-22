use askama::Template;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;
use crate::routes::oauth::{
    OAuthState, clear_state_cookie, oauth_error_page, set_state_cookie_max_age, stage_device_jwt,
    verify_oauth_state,
};
use crate::services::agentid::{
    self, AgentIdConfig, AgentIdentity, AuthorizeRequest, CALLBACK_PATH, START_PATH,
};
use crate::web::cookies;
use crate::web::meta::PageMeta;

pub const STATE_COOKIE_MAX_AGE: i64 = 1200;
const PROVIDER_LABEL: &str = "AgentID";
const TOKEN_TTL_DAYS: i64 = 30;

pub fn start_path(session: Option<&Uuid>) -> String {
    match session {
        Some(s) => format!("{START_PATH}?session={s}"),
        None => START_PATH.to_string(),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub login_hint: Option<String>,
    #[serde(default)]
    pub iss: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub iss: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

pub async fn start(State(state): State<AppState>, Query(q): Query<StartQuery>) -> Response {
    match AgentIdConfig::from_env() {
        Some(cfg) => start_with(&state, &cfg, q).await,
        None => crate::web::errors::render_404(&state.public_base_url, START_PATH),
    }
}

pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    match AgentIdConfig::from_env() {
        Some(cfg) => callback_with(&state, &cfg, &headers, q).await,
        None => crate::web::errors::render_404(&state.public_base_url, CALLBACK_PATH),
    }
}

pub async fn start_with(state: &AppState, cfg: &AgentIdConfig, q: StartQuery) -> Response {
    let base = state.public_base_url.clone();
    match start_impl(state, cfg, q).await {
        Ok(resp) => resp,
        Err(e) => oauth_error_page(&base, PROVIDER_LABEL, &e, START_PATH),
    }
}

async fn start_impl(
    state: &AppState,
    cfg: &AgentIdConfig,
    q: StartQuery,
) -> Result<Response, ApiError> {
    if let Some(iss) = q.iss.as_deref().filter(|s| !s.is_empty())
        && !cfg.issuer_matches(iss)
    {
        return Err(ApiError::ValidationError(
            "This sign-in was started by an unexpected identity provider.".into(),
        ));
    }
    let session = match q
        .session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(raw) => Some(Uuid::parse_str(raw).map_err(|_| {
            ApiError::ValidationError(
                "Invalid CLI session. Re-run `krab auth login --agentid`.".into(),
            )
        })?),
    };
    if let Some(session) = session {
        extend_device_session(state, &session).await?;
    }
    let discovery = agentid::discovery(cfg).await?;
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
    let encoded_state = oauth_state.encode();
    let verifier = agentid::pkce_verifier(&state.jwt_secret, &nonce);
    let oidc_nonce = agentid::oidc_nonce(&state.jwt_secret, &nonce);
    let url = agentid::authorize_url(
        cfg,
        &discovery,
        &state.public_base_url,
        &AuthorizeRequest {
            state: &encoded_state,
            code_verifier: &verifier,
            nonce: &oidc_nonce,
            login_hint: q.login_hint.as_deref(),
        },
    )?;
    let mut resp = Redirect::to(&url).into_response();
    set_state_cookie_max_age(resp.headers_mut(), &nonce, STATE_COOKIE_MAX_AGE);
    Ok(resp)
}

async fn extend_device_session(state: &AppState, session: &Uuid) -> Result<(), ApiError> {
    let n = sqlx::query(
        "UPDATE pending_verifications \
         SET expires_at = GREATEST(expires_at, NOW() + INTERVAL '20 minutes') \
         WHERE verification_type = 'device' AND code = $1 AND expires_at > NOW() \
           AND result_token IS NULL",
    )
    .bind(session.to_string())
    .execute(&state.pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::ValidationError(
            "This CLI sign-in expired. Re-run `krab auth login --agentid`.".into(),
        ));
    }
    Ok(())
}

pub fn retry_url(raw_state: &str) -> String {
    match OAuthState::parse(raw_state) {
        Some(OAuthState::Device { session, .. }) => {
            format!("/auth/device?session={session}&provider=agentid")
        }
        _ => START_PATH.to_string(),
    }
}

pub async fn callback_with(
    state: &AppState,
    cfg: &AgentIdConfig,
    headers: &HeaderMap,
    q: CallbackQuery,
) -> Response {
    let base = state.public_base_url.clone();
    let retry = retry_url(&q.state);
    let format = ResponseFormat::from_headers(headers);
    match callback_impl(state, cfg, headers, &q, format).await {
        Ok(resp) => resp,
        Err(e) => oauth_error_page(&base, PROVIDER_LABEL, &e, &retry),
    }
}

fn provider_error_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect()
}

async fn callback_impl(
    state: &AppState,
    cfg: &AgentIdConfig,
    headers: &HeaderMap,
    q: &CallbackQuery,
    format: ResponseFormat,
) -> Result<Response, ApiError> {
    let oauth_state = verify_oauth_state(headers, &q.state)?;
    if let Some(iss) = q.iss.as_deref()
        && !cfg.issuer_matches(iss)
    {
        return Err(ApiError::Unauthorized);
    }
    if let Some(err) = q.error.as_deref() {
        return Err(ApiError::ValidationError(format!(
            "AgentID sign-in did not complete ({}).",
            provider_error_code(err)
        )));
    }
    if q.code.is_empty() {
        return Err(ApiError::ValidationError(
            "AgentID sign-in was cancelled or failed.".into(),
        ));
    }
    let browser_nonce = oauth_state.nonce().to_string();
    let verifier = agentid::pkce_verifier(&state.jwt_secret, &browser_nonce);
    let discovery = agentid::discovery(cfg).await?;
    let tokens =
        agentid::exchange_code(cfg, &discovery, &state.public_base_url, &q.code, &verifier).await?;
    let claims = agentid::verify_id_token_with_refresh(
        cfg,
        &tokens.id_token,
        &agentid::oidc_nonce(&state.jwt_secret, &browser_nonce),
    )
    .await?;
    let userinfo = match (tokens.access_token.as_deref(), cfg.client_secret.is_some()) {
        (Some(access), true) => agentid::fetch_userinfo(&discovery, access).await,
        _ => None,
    };
    let claims = agentid::merge_userinfo(claims, userinfo.as_ref());
    let identity = AgentIdentity::from_claims(cfg, &claims);
    let user = agentid::sign_in_agent(&state.pool, &identity).await?;
    let jwt = crate::services::auth_service::create_jwt_via(
        user.id,
        user.entity_id,
        &user.role,
        &state.jwt_secret,
        Some(agentid::PROVIDER),
    )?;
    tracing::info!(
        user_id = %user.id,
        owner_attributed = identity.owner_email.is_some(),
        device = matches!(oauth_state, OAuthState::Device { .. }),
        "agentid.sign_in"
    );

    match oauth_state {
        OAuthState::Device { session, nonce } => {
            stage_device_jwt(state, &jwt, &user.display_name, &session, &nonce).await
        }
        OAuthState::Web { .. } => {
            let session = AgentSession {
                status: "authenticated",
                provider: agentid::PROVIDER,
                token_type: "Bearer",
                bearer_token: jwt,
                expires_in_days: TOKEN_TTL_DAYS,
                user_id: user.id,
                display_name: user.display_name.clone(),
                email: user.email.clone(),
                api_base_url: state.public_base_url.trim_end_matches('/').to_string(),
                next_steps: next_steps(&state.public_base_url),
            };
            let mut resp = render_session(&state.public_base_url, &session, format);
            clear_state_cookie(resp.headers_mut());
            Ok(resp)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseFormat {
    Html,
    Json,
    Markdown,
}

impl ResponseFormat {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let accept = headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if accept.contains("text/html") || accept.is_empty() {
            Self::Html
        } else if accept.contains("application/json") {
            Self::Json
        } else if accept.contains("text/markdown") || accept.contains("text/plain") {
            Self::Markdown
        } else {
            Self::Html
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AgentSession {
    pub status: &'static str,
    pub provider: &'static str,
    pub token_type: &'static str,
    pub bearer_token: String,
    pub expires_in_days: i64,
    pub user_id: Uuid,
    pub display_name: String,
    pub email: Option<String>,
    pub api_base_url: String,
    pub next_steps: Vec<String>,
}

pub fn next_steps(public_base_url: &str) -> Vec<String> {
    let base = public_base_url.trim_end_matches('/');
    vec![
        format!("curl -fsSL {base}/install.sh | sh"),
        "krab auth set-token <bearer_token>".to_string(),
        "krab auth status".to_string(),
        "krab battle list --status active".to_string(),
    ]
}

pub fn session_markdown(s: &AgentSession) -> String {
    let base = &s.api_base_url;
    let token = &s.bearer_token;
    let email = s
        .email
        .as_deref()
        .map(|e| format!(" <{e}>"))
        .unwrap_or_default();
    format!(
        "# Signed in to KrabArena with AgentID\n\
         \n\
         Agent: {name}{email}\n\
         KrabArena user id: {id}\n\
         Token: Bearer JWT, valid {days} days. It is shown once; keep it secret.\n\
         \n\
         ## Token\n\
         \n\
         {token}\n\
         \n\
         ## Next steps\n\
         \n\
         1. Install the CLI: `curl -fsSL {base}/install.sh | sh`\n\
         2. Save the token: `krab auth set-token {token}`\n\
         3. Check it: `krab auth status`\n\
         4. Read the agent guide at {base}/docs/agent and pick a Battle: `krab battle list --status active`\n\
         \n\
         Without the CLI, send `Authorization: Bearer <token>` to {base}/v1/... endpoints.\n\
         Next time, sign in from a terminal with `krab auth login --agentid`.\n",
        name = s.display_name,
        id = s.user_id,
        days = s.expires_in_days,
    )
}

#[derive(Template)]
#[template(path = "pages/agentid_signed_in.html")]
struct AgentIdSignedInTemplate {
    meta: PageMeta,
    display_name: String,
    token: String,
    base_url: String,
    markdown: String,
    json: String,
}

fn no_store(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    resp
}

pub fn render_session(public_base_url: &str, s: &AgentSession, format: ResponseFormat) -> Response {
    let resp = match format {
        ResponseFormat::Json => Json(crate::envelope::ApiResponse::new(s)).into_response(),
        ResponseFormat::Markdown => (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            session_markdown(s),
        )
            .into_response(),
        ResponseFormat::Html => crate::web::errors::render_template_or_500(
            AgentIdSignedInTemplate {
                meta: PageMeta::new(public_base_url, CALLBACK_PATH)
                    .with_title("Signed in with AgentID")
                    .with_description("Your KrabArena agent token from AgentID sign-in.")
                    .noindex_follow(),
                display_name: s.display_name.clone(),
                token: s.bearer_token.clone(),
                base_url: s.api_base_url.clone(),
                markdown: session_markdown(s),
                json: serde_json::to_string_pretty(s).unwrap_or_default(),
            },
            public_base_url,
            CALLBACK_PATH,
            "agentid.signed_in",
        ),
    };
    no_store(resp)
}

#[cfg(test)]
#[path = "agentid_tests.rs"]
mod tests;
