use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::models::user::{UserResponse, UserRow};

pub const PROVIDER: &str = "agentid";
pub const DEFAULT_ISSUER: &str = "https://auth.agentid.com";
pub const OPEN_CLIENT_SCOPES: &str = "openid email profile";
pub const REGISTERED_CLIENT_SCOPES: &str = "openid email profile owner_profile owner_email";
pub const CALLBACK_PATH: &str = "/auth/agentid/callback";
pub const START_PATH: &str = "/auth/agentid/start";
const METADATA_TTL: Duration = Duration::from_secs(3600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const CLOCK_LEEWAY_SECS: u64 = 60;

const USER_COLUMNS: &str = "u.id, u.entity_id, u.display_name, u.email, u.auth_method, u.role, \
     u.created_at, u.kind, u.org_role, u.avatar_url";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentIdConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: String,
}

impl AgentIdConfig {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let non_empty = |k: &str| {
            get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let client_id = non_empty("AGENTID_CLIENT_ID")?;
        let client_secret = non_empty("AGENTID_CLIENT_SECRET");
        let issuer = non_empty("AGENTID_ISSUER")
            .unwrap_or_else(|| DEFAULT_ISSUER.to_string())
            .trim_end_matches('/')
            .to_string();
        let scopes = non_empty("AGENTID_SCOPES").unwrap_or_else(|| {
            if client_secret.is_some() {
                REGISTERED_CLIENT_SCOPES.to_string()
            } else {
                OPEN_CLIENT_SCOPES.to_string()
            }
        });
        Some(Self {
            issuer,
            client_id,
            client_secret,
            scopes,
        })
    }

    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn redirect_uri(&self, public_base_url: &str) -> String {
        format!("{}{CALLBACK_PATH}", public_base_url.trim_end_matches('/'))
    }

    pub fn issuer_matches(&self, iss: &str) -> bool {
        iss.trim_end_matches('/') == self.issuer
    }
}

pub fn enabled() -> bool {
    AgentIdConfig::from_env().is_some()
}

#[derive(Clone, Debug, Deserialize)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
}

#[derive(Clone)]
struct ProviderMetadata {
    discovery: Discovery,
    jwks: JwkSet,
    fetched_at: Instant,
}

fn metadata_cache() -> &'static Mutex<HashMap<String, ProviderMetadata>> {
    static CACHE: OnceLock<Mutex<HashMap<String, ProviderMetadata>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent("KrabArena")
            .build()
            .unwrap_or_default()
    })
}

fn upstream(context: &str, e: impl std::fmt::Display) -> ApiError {
    tracing::warn!(error = %e, context = context, "agentid.upstream_error");
    ApiError::UpstreamError(format!("AgentID {context} failed"))
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str, context: &str) -> Result<T, ApiError> {
    let resp = http()
        .get(url)
        .send()
        .await
        .map_err(|e| upstream(context, e))?;
    if !resp.status().is_success() {
        return Err(upstream(context, resp.status()));
    }
    resp.json::<T>().await.map_err(|e| upstream(context, e))
}

async fn fetch_metadata(cfg: &AgentIdConfig) -> Result<ProviderMetadata, ApiError> {
    let discovery: Discovery = get_json(
        &format!("{}/.well-known/openid-configuration", cfg.issuer),
        "discovery",
    )
    .await?;
    if !cfg.issuer_matches(&discovery.issuer) {
        return Err(upstream(
            "discovery",
            format!("issuer mismatch: {}", discovery.issuer),
        ));
    }
    let jwks: JwkSet = get_json(&discovery.jwks_uri, "jwks").await?;
    Ok(ProviderMetadata {
        discovery,
        jwks,
        fetched_at: Instant::now(),
    })
}

async fn metadata(cfg: &AgentIdConfig, force_refresh: bool) -> Result<ProviderMetadata, ApiError> {
    if !force_refresh
        && let Ok(cache) = metadata_cache().lock()
        && let Some(m) = cache.get(&cfg.issuer)
        && m.fetched_at.elapsed() < METADATA_TTL
    {
        return Ok(m.clone());
    }
    let fresh = fetch_metadata(cfg).await?;
    if let Ok(mut cache) = metadata_cache().lock() {
        cache.insert(cfg.issuer.clone(), fresh.clone());
    }
    Ok(fresh)
}

pub async fn discovery(cfg: &AgentIdConfig) -> Result<Discovery, ApiError> {
    Ok(metadata(cfg, false).await?.discovery)
}

fn derive_secret(label: &str, server_secret: &str, browser_nonce: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(
        format!("krab-agentid-{label}|{server_secret}|{browser_nonce}").as_bytes(),
    ))
}

pub fn pkce_verifier(server_secret: &str, browser_nonce: &str) -> String {
    derive_secret("pkce", server_secret, browser_nonce)
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn oidc_nonce(server_secret: &str, browser_nonce: &str) -> String {
    derive_secret("nonce", server_secret, browser_nonce)
}

pub struct AuthorizeRequest<'a> {
    pub state: &'a str,
    pub code_verifier: &'a str,
    pub nonce: &'a str,
    pub login_hint: Option<&'a str>,
}

pub fn authorize_url(
    cfg: &AgentIdConfig,
    discovery: &Discovery,
    public_base_url: &str,
    req: &AuthorizeRequest<'_>,
) -> Result<String, ApiError> {
    let mut url = reqwest::Url::parse(&discovery.authorization_endpoint)
        .map_err(|e| upstream("discovery", e))?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &cfg.client_id)
            .append_pair("redirect_uri", &cfg.redirect_uri(public_base_url))
            .append_pair("scope", &cfg.scopes)
            .append_pair("state", req.state)
            .append_pair("nonce", req.nonce)
            .append_pair("code_challenge", &pkce_challenge(req.code_verifier))
            .append_pair("code_challenge_method", "S256");
        if let Some(hint) = req
            .login_hint
            .map(str::trim)
            .filter(|h| valid_login_hint(h))
        {
            q.append_pair("login_hint", hint);
        }
    }
    Ok(url.to_string())
}

fn valid_login_hint(h: &str) -> bool {
    !h.is_empty() && h.len() <= 320 && h.contains('@') && !h.chars().any(char::is_control)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    access_token: Option<String>,
}

pub struct Tokens {
    pub id_token: String,
    pub access_token: Option<String>,
}

pub async fn exchange_code(
    cfg: &AgentIdConfig,
    discovery: &Discovery,
    public_base_url: &str,
    code: &str,
    code_verifier: &str,
) -> Result<Tokens, ApiError> {
    let redirect_uri = cfg.redirect_uri(public_base_url);
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri.as_str()),
        ("code_verifier", code_verifier),
    ];
    let mut req = http().post(&discovery.token_endpoint);
    match cfg.client_secret.as_deref() {
        Some(secret) => req = req.basic_auth(&cfg.client_id, Some(secret)),
        None => form.push(("client_id", cfg.client_id.as_str())),
    }
    let resp = req
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| upstream("token exchange", e))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(status = %status, body = %body.chars().take(300).collect::<String>(), "agentid.token_exchange_rejected");
        return Err(ApiError::ValidationError(
            "AgentID rejected the sign-in code. Start the sign-in again.".into(),
        ));
    }
    let tokens: TokenResponse = resp
        .json()
        .await
        .map_err(|e| upstream("token exchange", e))?;
    let id_token = tokens
        .id_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| upstream("token exchange", "no id_token"))?;
    Ok(Tokens {
        id_token,
        access_token: tokens.access_token.filter(|t| !t.is_empty()),
    })
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct AgentIdClaims {
    pub iss: String,
    pub sub: String,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub actor_type: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub owner_sub: Option<String>,
    #[serde(default)]
    pub owner_name: Option<String>,
    #[serde(default)]
    pub owner_email: Option<String>,
    #[serde(default)]
    pub owner_email_verified: Option<bool>,
}

pub fn verify_id_token(
    token: &str,
    jwks: &JwkSet,
    cfg: &AgentIdConfig,
    expected_nonce: &str,
) -> Result<AgentIdClaims, ApiError> {
    let invalid = |why: &str| {
        tracing::warn!(reason = why, "agentid.id_token_rejected");
        ApiError::ValidationError("AgentID returned an invalid identity token.".into())
    };
    let header = decode_header(token).map_err(|_| invalid("malformed header"))?;
    if header.alg != Algorithm::ES256 {
        return Err(invalid("unexpected alg"));
    }
    let jwk = match header.kid.as_deref() {
        Some(kid) => jwks.find(kid),
        None if jwks.keys.len() == 1 => jwks.keys.first(),
        None => None,
    }
    .ok_or_else(|| invalid("unknown kid"))?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| invalid("unusable jwk"))?;
    let mut validation = Validation::new(Algorithm::ES256);
    validation.leeway = CLOCK_LEEWAY_SECS;
    validation.set_issuer(&[cfg.issuer.as_str(), &format!("{}/", cfg.issuer)]);
    validation.set_audience(&[cfg.client_id.as_str()]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<AgentIdClaims>(token, &key, &validation)
        .map_err(|e| invalid(&format!("{:?}", e.kind())))?
        .claims;
    if claims.sub.trim().is_empty() {
        return Err(invalid("empty sub"));
    }
    if let Some(n) = claims.nonce.as_deref()
        && !crate::web::cookies::ct_eq(n, expected_nonce)
    {
        return Err(invalid("nonce mismatch"));
    }
    if let Some(actor) = claims.actor_type.as_deref()
        && actor != "agent"
    {
        return Err(invalid("actor_type is not agent"));
    }
    Ok(claims)
}

pub async fn verify_id_token_with_refresh(
    cfg: &AgentIdConfig,
    token: &str,
    expected_nonce: &str,
) -> Result<AgentIdClaims, ApiError> {
    let meta = metadata(cfg, false).await?;
    let kid_known = decode_header(token)
        .ok()
        .and_then(|h| h.kid)
        .is_none_or(|kid| meta.jwks.find(&kid).is_some());
    let jwks = if kid_known {
        meta.jwks
    } else {
        metadata(cfg, true).await?.jwks
    };
    verify_id_token(token, &jwks, cfg, expected_nonce)
}

pub async fn fetch_userinfo(
    discovery: &Discovery,
    access_token: &str,
) -> Option<serde_json::Value> {
    let endpoint = discovery.userinfo_endpoint.as_deref()?;
    let resp = http()
        .get(endpoint)
        .bearer_auth(access_token)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| tracing::warn!(error = %e, "agentid.userinfo_failed"))
        .ok()?;
    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), "agentid.userinfo_rejected");
        return None;
    }
    resp.json().await.ok()
}

pub fn merge_userinfo(
    mut claims: AgentIdClaims,
    userinfo: Option<&serde_json::Value>,
) -> AgentIdClaims {
    let Some(info) = userinfo else {
        return claims;
    };
    if info["sub"].as_str() != Some(claims.sub.as_str()) {
        tracing::warn!("agentid.userinfo_sub_mismatch");
        return claims;
    }
    let text = |k: &str| info[k].as_str().map(str::to_string);
    claims.owner_sub = claims.owner_sub.or_else(|| text("owner_sub"));
    claims.owner_name = claims.owner_name.or_else(|| text("owner_name"));
    if claims.owner_email.is_none() {
        claims.owner_email = text("owner_email");
        claims.owner_email_verified = info["owner_email_verified"].as_bool();
    }
    claims
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentIdentity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub display_name: String,
    pub owner_sub: Option<String>,
    pub owner_email: Option<String>,
    pub owner_name: Option<String>,
}

fn norm_email(e: Option<&str>) -> Option<String> {
    e.map(|s| s.trim().to_lowercase())
        .filter(|s| s.contains('@') && s.len() <= 320)
}

fn clean_text(s: Option<&str>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl AgentIdentity {
    pub fn from_claims(cfg: &AgentIdConfig, c: &AgentIdClaims) -> Self {
        let email = c
            .email_verified
            .unwrap_or(false)
            .then(|| norm_email(c.email.as_deref()))
            .flatten();
        let owner_email = c
            .owner_email_verified
            .unwrap_or(false)
            .then(|| norm_email(c.owner_email.as_deref()))
            .flatten();
        let display_name = clean_text(c.name.as_deref())
            .or_else(|| clean_text(c.preferred_username.as_deref()))
            .or_else(|| {
                email
                    .as_deref()
                    .and_then(|e| e.split('@').next())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "agentid-agent".to_string());
        Self {
            issuer: cfg.issuer.clone(),
            subject: c.sub.clone(),
            email,
            display_name,
            owner_sub: clean_text(c.owner_sub.as_deref()),
            owner_email,
            owner_name: clean_text(c.owner_name.as_deref()),
        }
    }

    pub fn entity_domain(&self) -> String {
        self.email
            .as_deref()
            .and_then(|e| e.split('@').nth(1))
            .filter(|d| !d.is_empty())
            .unwrap_or("agentid.com")
            .to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistingAccount {
    pub auth_method: String,
    pub role: String,
    pub kind: String,
    pub email: Option<String>,
    pub bound_to_other_agentid: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkDecision {
    Link,
    Refuse(&'static str),
}

pub fn decide_email_collision(
    existing: &ExistingAccount,
    identity: &AgentIdentity,
) -> LinkDecision {
    if existing.bound_to_other_agentid {
        return LinkDecision::Refuse(
            "This inbox's KrabArena account is already linked to a different AgentID identity.",
        );
    }
    if existing.role != "user" || existing.kind == "bot" {
        return LinkDecision::Refuse(
            "This inbox belongs to a privileged KrabArena account, which cannot be linked to AgentID.",
        );
    }
    if existing.auth_method == "email" {
        return LinkDecision::Link;
    }
    let owner_matches = matches!(
        (identity.owner_email.as_deref(), existing.email.as_deref()),
        (Some(owner), Some(account)) if owner.eq_ignore_ascii_case(account)
    );
    if owner_matches {
        LinkDecision::Link
    } else {
        LinkDecision::Refuse(
            "This inbox already belongs to a KrabArena account created with another sign-in method. Sign in with that method instead.",
        )
    }
}

fn db(e: sqlx::Error) -> ApiError {
    ApiError::Internal(e.into())
}

pub async fn sign_in_agent(
    pool: &PgPool,
    identity: &AgentIdentity,
) -> Result<UserResponse, ApiError> {
    let mut tx = pool.begin().await.map_err(db)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!(
            "{PROVIDER}|{}|{}",
            identity.issuer, identity.subject
        ))
        .execute(&mut *tx)
        .await
        .map_err(db)?;

    let bound = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM user_identities i JOIN users u ON u.id = i.user_id \
         WHERE i.provider = $1 AND i.issuer = $2 AND i.subject = $3"
    ))
    .bind(PROVIDER)
    .bind(&identity.issuer)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?;

    let user = match bound {
        Some(user) => user,
        None => {
            let user = match find_by_email(&mut tx, identity).await? {
                Some(existing) => existing,
                None => {
                    let entity_id = ensure_entity(&mut tx, &identity.entity_domain()).await?;
                    insert_agent_user(&mut tx, entity_id, identity).await?
                }
            };
            sqlx::query(
                "INSERT INTO user_identities (provider, issuer, subject, user_id) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            )
            .bind(PROVIDER)
            .bind(&identity.issuer)
            .bind(&identity.subject)
            .bind(user.id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            user
        }
    };

    sqlx::query(
        "UPDATE user_identities SET email = $4, owner_sub = $5, owner_email = $6, \
             owner_email_verified = $7, owner_name = $8, last_login_at = now() \
         WHERE provider = $1 AND issuer = $2 AND subject = $3",
    )
    .bind(PROVIDER)
    .bind(&identity.issuer)
    .bind(&identity.subject)
    .bind(&identity.email)
    .bind(&identity.owner_sub)
    .bind(&identity.owner_email)
    .bind(identity.owner_email.is_some())
    .bind(&identity.owner_name)
    .execute(&mut *tx)
    .await
    .map_err(db)?;

    tx.commit().await.map_err(db)?;
    Ok(UserResponse::from(user))
}

async fn find_by_email(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    identity: &AgentIdentity,
) -> Result<Option<UserRow>, ApiError> {
    let Some(email) = identity.email.as_deref() else {
        return Ok(None);
    };
    let Some(row) = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users u WHERE LOWER(u.email) = LOWER($1) LIMIT 1"
    ))
    .bind(email)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    else {
        return Ok(None);
    };
    let bound_to_other_agentid = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM user_identities WHERE provider = $1 AND user_id = $2)",
    )
    .bind(PROVIDER)
    .bind(row.id)
    .fetch_one(&mut **tx)
    .await
    .map_err(db)?;
    let existing = ExistingAccount {
        auth_method: row.auth_method.clone(),
        role: row.role.clone(),
        kind: row.kind.clone(),
        email: row.email.clone(),
        bound_to_other_agentid,
    };
    match decide_email_collision(&existing, identity) {
        LinkDecision::Link => {
            tracing::info!(user_id = %row.id, "agentid.linked_existing_account");
            Ok(Some(row))
        }
        LinkDecision::Refuse(why) => {
            tracing::warn!(user_id = %row.id, reason = why, "agentid.link_refused");
            Err(ApiError::ValidationError(why.to_string()))
        }
    }
}

async fn ensure_entity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    domain: &str,
) -> Result<Uuid, ApiError> {
    sqlx::query(
        "INSERT INTO entities (name, domain, verification_method) VALUES ($1, $1, $2) \
         ON CONFLICT (domain) DO NOTHING",
    )
    .bind(domain)
    .bind(PROVIDER)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM entities WHERE domain = $1")
        .bind(domain)
        .fetch_one(&mut **tx)
        .await
        .map_err(db)
}

async fn insert_agent_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    entity_id: Uuid,
    identity: &AgentIdentity,
) -> Result<UserRow, ApiError> {
    let (display_name, _) = krabarena_shared::field_limits::truncate_to_cap(
        &identity.display_name,
        krabarena_shared::field_limits::Field::UsersDisplayName.max(),
    );
    let columns = USER_COLUMNS.replace("u.", "");
    sqlx::query_as::<_, UserRow>(&format!(
        "INSERT INTO users (entity_id, display_name, email, auth_method) \
         VALUES ($1, $2, $3, $4) RETURNING {columns}"
    ))
    .bind(entity_id)
    .bind(&display_name)
    .bind(&identity.email)
    .bind(PROVIDER)
    .fetch_one(&mut **tx)
    .await
    .map_err(db)
}

pub async fn owned_agent_for_verified_email(
    pool: &PgPool,
    verified_email: &str,
) -> Result<Option<Uuid>, ApiError> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM user_identities \
         WHERE owner_email_verified AND LOWER(owner_email) = LOWER($1) \
         ORDER BY last_login_at DESC LIMIT 1",
    )
    .bind(verified_email)
    .fetch_optional(pool)
    .await
    .map_err(db)
}

#[cfg(test)]
#[path = "agentid_tests.rs"]
pub(crate) mod tests;
