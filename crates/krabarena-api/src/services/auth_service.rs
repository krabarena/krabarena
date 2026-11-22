use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::models::entity::{EntityResponse, EntityRow};
use crate::models::user::{UserResponse, UserRow};
use crate::services::email_service::EmailService;

// Explicit column lists for `users` and `entities`. We used to
// `SELECT *` here — that's the cached-plan-trap (AGENTS.md → "ALTER
// TABLE on prod"): when migration 042 added columns, every cached
// plan against `users` / `entities` returned `0A000: cached plan
// must not change result type` until Neon's PgBouncer backends
// recycled, which Cloud Run revision swaps don't trigger. Named
// columns sidestep the trap because Postgres recompiles the plan
// when the column list changes — but doesn't have to when the
// underlying table grows extra columns.
//
// Listed columns are the subset `UserRow` / `EntityRow` declare;
// sqlx::FromRow matches by name, so extra schema columns (e.g.
// `suspended_until`, set by the admin suspend flow but not exposed
// via these auth callbacks) stay out of the SELECT and out of the
// struct.
const USER_COLUMNS: &str = "id, entity_id, display_name, email, auth_method, role, \
     created_at, kind, org_role, avatar_url";
const ENTITY_COLUMNS: &str = "id, name, domain, verification_method, created_at, admin_user_id";

#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
    pub sub: Uuid,
    pub entity_id: Uuid,
    /// User role: "admin", "moderator", or "user". Defaults to "user" for
    /// backward compatibility with tokens issued before roles were added.
    #[serde(default = "default_role")]
    pub role: String,
    pub exp: i64,
    pub iat: i64,
}

fn default_role() -> String {
    "user".to_string()
}

#[derive(Debug, Serialize)]
pub struct AuthResult {
    pub bearer_token: String,
    pub user: UserResponse,
    pub entity: EntityResponse,
}

pub fn create_jwt(
    user_id: Uuid,
    entity_id: Uuid,
    role: &str,
    secret: &str,
) -> Result<String, ApiError> {
    let now = Utc::now();
    let claims = JwtClaims {
        sub: user_id,
        entity_id,
        role: role.to_string(),
        iat: now.timestamp(),
        exp: (now + Duration::days(30)).timestamp(),
    };

    let key =
        EncodingKey::from_secret(secret.as_bytes()).map_err(|e| ApiError::Internal(e.into()))?;
    encode(&Header::default(), &claims, &key).map_err(|e| ApiError::Internal(e.into()))
}

pub fn validate_jwt(token: &str, secret: &str) -> Result<JwtClaims, ApiError> {
    let key = DecodingKey::from_secret(secret.as_bytes()).map_err(|_| ApiError::Unauthorized)?;
    decode::<JwtClaims>(token, &key, &Validation::new(Algorithm::HS256))
        .map(|data| data.claims)
        .map_err(|_| ApiError::Unauthorized)
}

pub async fn initiate_email_auth(
    pool: &PgPool,
    email: &str,
    email_service: Option<&EmailService>,
) -> Result<(), ApiError> {
    if !email.contains('@') || email.ends_with("@gmail.com") || email.ends_with("@yahoo.com") {
        return Err(ApiError::InvalidEmail);
    }

    let code = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + Duration::hours(1);

    sqlx::query(
        r#"
        INSERT INTO pending_verifications (verification_type, identifier, code, expires_at)
        VALUES ('email', $1, $2, $3)
        "#,
    )
    .bind(email)
    .bind(&code)
    .bind(expires_at)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    match email_service {
        Some(svc) => svc.send_verification(email, &code, &code).await?,
        None if dev_log_login_codes() => {
            tracing::warn!(email = %email, code = %code, "auth.email.initiated (no email service — code logged: KRAB_DEV_LOG_LOGIN_CODES=1)")
        }
        None => {
            tracing::warn!(email = %email, "auth.email.initiated (no email service — set KRAB_DEV_LOG_LOGIN_CODES=1 to log codes in dev)")
        }
    }

    Ok(())
}

/// Minimum length of a code accepted by the code-only magic-link path
/// (`verify_email`). Link tokens are 36-char UUIDs (CLI flow) or 64-hex
/// random tokens (voter flow); the 8-digit voter code is far shorter and
/// must ONLY be verifiable together with its email address
/// (`POST /auth/voter/email/verify`, with an attempt cap). Before this
/// guard the 8-digit code was accepted here on its own — a brute-forcer
/// could match ANY user's pending code with ~10^8 guesses total and no
/// per-identifier cap.
pub const MIN_LINK_TOKEN_LEN: usize = 32;

/// True when `code` is long enough to be a magic-link token.
pub fn is_link_token(code: &str) -> bool {
    code.len() >= MIN_LINK_TOKEN_LEN
}

/// A fresh 256-bit magic-link token, hex encoded (64 chars).
pub fn new_link_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Login codes are logged (instead of emailed) only when no email
/// service is configured AND the operator explicitly opts in with
/// `KRAB_DEV_LOG_LOGIN_CODES=1`. Logging them unconditionally put live
/// credentials into Cloud Logging whenever Mailgun was misconfigured.
pub fn dev_log_login_codes() -> bool {
    std::env::var("KRAB_DEV_LOG_LOGIN_CODES").ok().as_deref() == Some("1")
}

pub async fn verify_email(
    pool: &PgPool,
    code: &str,
    jwt_secret: &str,
) -> Result<AuthResult, ApiError> {
    if !is_link_token(code) {
        return Err(ApiError::InvalidVerificationToken);
    }
    let record = sqlx::query_as::<_, (String,)>(
        r#"
        SELECT identifier FROM pending_verifications
        WHERE code = $1 AND verification_type = 'email' AND expires_at > NOW()
        "#,
    )
    .bind(code)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?
    .ok_or(ApiError::InvalidVerificationToken)?;

    let email = &record.0;
    let domain = email
        .split('@')
        .nth(1)
        .ok_or(ApiError::InvalidEmail)?
        .to_string();

    let entity = find_or_create_entity(pool, &domain, "email").await?;
    let user = find_or_create_user(pool, entity.id, email, "email").await?;
    let token = create_jwt(user.id, entity.id, &user.role, jwt_secret)?;

    // Store the JWT so the poll endpoint can return it, then clean up after 5 min
    sqlx::query(
        "UPDATE pending_verifications SET result_token = $1, expires_at = NOW() + INTERVAL '5 minutes' WHERE code = $2",
    )
    .bind(&token)
    .bind(code)
    .execute(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(AuthResult {
        bearer_token: token,
        user,
        entity,
    })
}

/// Public wrapper for OAuth-based entity creation.
pub async fn find_or_create_entity_pub(
    pool: &PgPool,
    domain: &str,
    method: &str,
) -> Result<EntityResponse, ApiError> {
    find_or_create_entity(pool, domain, method).await
}

/// Public wrapper for OAuth-based user creation with explicit display_name and email.
///
/// Lookup order, important for cross-provider login (same person using
/// email verification once, then Google OAuth later — we must not INSERT
/// a second row because `users.email` is UNIQUE):
///   1. Email match — if a user with this email exists, return them.
///   2. (entity_id, display_name) match — legacy path when email isn't
///      available (rare for OAuth but allowed).
///   3. INSERT a fresh row.
pub async fn find_or_create_user_pub(
    pool: &PgPool,
    entity_id: Uuid,
    _identifier: &str,
    method: &str,
    display_name: &str,
    email: Option<&str>,
) -> Result<UserResponse, ApiError> {
    // OAuth carve-out for users.display_name (Google, GitHub, etc.).
    // Same policy as voter_service.find_or_create_voter — truncate
    // rather than reject so a pathological IdP value doesn't break
    // login. The email-signup path goes through a different code
    // path and validates normally. See `krabarena_shared::field_limits`
    // module docstring.
    let (display_name_owned, dn_trunc) = krabarena_shared::field_limits::truncate_to_cap(
        display_name,
        krabarena_shared::field_limits::Field::UsersDisplayName.max(),
    );
    if let Some(info) = dn_trunc {
        tracing::info!(
            method = method,
            field = "users.display_name",
            original_chars = info.original_chars,
            cap = info.cap,
            "oauth.field.truncated"
        );
    }
    let display_name = display_name_owned.as_str();

    if let Some(e) = email.filter(|s| !s.is_empty()) {
        let by_email = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE email = $1"
        ))
        .bind(e)
        .fetch_optional(pool)
        .await
        .map_err(|err| ApiError::Internal(err.into()))?;
        if let Some(row) = by_email {
            return Ok(UserResponse::from(row));
        }
    }

    let by_name = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE entity_id = $1 AND display_name = $2"
    ))
    .bind(entity_id)
    .bind(display_name)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    if let Some(row) = by_name {
        return Ok(UserResponse::from(row));
    }

    // Use ON CONFLICT on email so a racing second OAuth callback for the
    // same email returns the existing user instead of raising
    // `users_email_key`. For the NULL-email case (user didn't grant
    // email scope), ON CONFLICT (email) is a no-op because Postgres
    // treats NULLs as distinct in unique indexes, so the INSERT goes
    // through normally. Refreshing display_name on re-auth is a
    // feature, not a bug — users change their display names at the
    // provider and we want the latest.
    let row = sqlx::query_as::<_, UserRow>(&format!(
        r#"
        INSERT INTO users (entity_id, display_name, email, auth_method)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (email) DO UPDATE
            SET display_name = EXCLUDED.display_name
        RETURNING {USER_COLUMNS}
        "#,
    ))
    .bind(entity_id)
    .bind(display_name)
    .bind(email)
    .bind(method)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(UserResponse::from(row))
}

async fn find_or_create_entity(
    pool: &PgPool,
    domain: &str,
    method: &str,
) -> Result<EntityResponse, ApiError> {
    let existing = sqlx::query_as::<_, EntityRow>(&format!(
        "SELECT {ENTITY_COLUMNS} FROM entities WHERE domain = $1"
    ))
    .bind(domain)
    .fetch_optional(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    if let Some(row) = existing {
        return Ok(EntityResponse::from(row));
    }

    let row = sqlx::query_as::<_, EntityRow>(&format!(
        r#"
        INSERT INTO entities (name, domain, verification_method)
        VALUES ($1, $2, $3)
        RETURNING {ENTITY_COLUMNS}
        "#,
    ))
    .bind(domain)
    .bind(domain)
    .bind(method)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(EntityResponse::from(row))
}

/// Repository seam over the Postgres-bound auth operations. **Not yet
/// adopted by call-sites** — this is the skeleton for the
/// repository-trait refactor outlined in the architectural audit so
/// the DB-bound functions can be exercised under a mock without
/// spinning up Postgres. Each method mirrors a free function in this
/// module; the next step is to land a `PgAuthRepository(PgPool)`
/// implementation and migrate route handlers off the free functions.
///
/// JWT issuance/validation (`create_jwt`, `validate_jwt`) is
/// deliberately NOT on this trait — those functions are already
/// pool-free and are unit-tested directly below.
///
/// Uses native async-in-trait (edition 2024). If we ever need
/// `dyn AuthRepository` we'll have to introduce `async-trait` or
/// `trait_variant::make` — but mock-via-struct doesn't need dyn.
#[allow(dead_code, async_fn_in_trait)]
pub trait AuthRepository: Send + Sync {
    async fn insert_email_verification(
        &self,
        email: &str,
        code: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), ApiError>;

    async fn consume_email_verification(&self, code: &str) -> Result<Option<String>, ApiError>;

    async fn find_or_create_entity(
        &self,
        domain: &str,
        method: &str,
    ) -> Result<EntityResponse, ApiError>;

    async fn find_or_create_user(
        &self,
        entity_id: Uuid,
        identifier: &str,
        method: &str,
    ) -> Result<UserResponse, ApiError>;
}

async fn find_or_create_user(
    pool: &PgPool,
    entity_id: Uuid,
    identifier: &str,
    method: &str,
) -> Result<UserResponse, ApiError> {
    let existing = if method == "email" {
        sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE email = $1"
        ))
        .bind(identifier)
        .fetch_optional(pool)
        .await
    } else {
        sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE entity_id = $1 AND auth_method = 'dns'"
        ))
        .bind(entity_id)
        .fetch_optional(pool)
        .await
    }
    .map_err(|e| ApiError::Internal(e.into()))?;

    if let Some(row) = existing {
        return Ok(UserResponse::from(row));
    }

    let display_name = if method == "email" {
        identifier
            .split('@')
            .next()
            .unwrap_or(identifier)
            .to_string()
    } else {
        identifier.to_string()
    };

    let email = if method == "email" {
        Some(identifier.to_string())
    } else {
        None
    };

    let row = sqlx::query_as::<_, UserRow>(&format!(
        r#"
        INSERT INTO users (entity_id, display_name, email, auth_method)
        VALUES ($1, $2, $3, $4)
        RETURNING {USER_COLUMNS}
        "#,
    ))
    .bind(entity_id)
    .bind(&display_name)
    .bind(email.as_deref())
    .bind(method)
    .fetch_one(pool)
    .await
    .map_err(|e| ApiError::Internal(e.into()))?;

    Ok(UserResponse::from(row))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret-do-not-use-in-prod";

    #[test]
    fn create_jwt_round_trips() {
        let user_id = Uuid::new_v4();
        let entity_id = Uuid::new_v4();
        let token = create_jwt(user_id, entity_id, "user", SECRET).expect("issuance");
        let claims = validate_jwt(&token, SECRET).expect("validation");

        assert_eq!(claims.sub, user_id);
        assert_eq!(claims.entity_id, entity_id);
        assert_eq!(claims.role, "user");
        // 30-day expiry, give or take a few seconds of scheduling jitter.
        let ttl = claims.exp - claims.iat;
        let thirty_days = chrono::Duration::days(30).num_seconds();
        assert!(
            (ttl - thirty_days).abs() < 5,
            "ttl {} differed from 30d {} by more than 5s",
            ttl,
            thirty_days
        );
    }

    #[test]
    fn validate_jwt_rejects_wrong_secret() {
        let token = create_jwt(Uuid::new_v4(), Uuid::new_v4(), "user", SECRET).unwrap();
        let err = validate_jwt(&token, "different-secret").unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }

    #[test]
    fn validate_jwt_rejects_malformed_token() {
        let err = validate_jwt("not.a.jwt", SECRET).unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }

    #[test]
    fn validate_jwt_rejects_tampered_payload() {
        let token = create_jwt(Uuid::new_v4(), Uuid::new_v4(), "user", SECRET).unwrap();
        // Flip a character mid-payload. JWT is `header.payload.sig`;
        // mutating the payload breaks the signature.
        let mut parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let mut payload = parts[1].to_string();
        let first = payload.chars().next().unwrap();
        let replacement = if first == 'A' { 'B' } else { 'A' };
        payload.replace_range(0..1, &replacement.to_string());
        parts[1] = &payload;
        let tampered = parts.join(".");

        let err = validate_jwt(&tampered, SECRET).unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized));
    }

    #[test]
    fn jwt_claims_role_defaults_to_user_on_legacy_tokens() {
        // Old tokens were issued without a `role` claim. Encode a
        // hand-rolled token with no `role` field and confirm serde's
        // default kicks in on decode.
        use jsonwebtoken::{EncodingKey, Header, encode};

        #[derive(serde::Serialize)]
        struct LegacyClaims {
            sub: Uuid,
            entity_id: Uuid,
            exp: i64,
            iat: i64,
        }

        let now = Utc::now();
        let legacy = LegacyClaims {
            sub: Uuid::new_v4(),
            entity_id: Uuid::new_v4(),
            iat: now.timestamp(),
            exp: (now + Duration::days(1)).timestamp(),
        };
        let token = encode(
            &Header::default(),
            &legacy,
            &EncodingKey::from_secret(SECRET.as_bytes()).unwrap(),
        )
        .unwrap();

        let claims = validate_jwt(&token, SECRET).expect("legacy token should decode");
        assert_eq!(claims.role, "user");
    }

    /// Regression: the code-only magic-link path accepted the 8-digit
    /// voter code with no identifier, so codes were brute-forceable
    /// across every pending login at once.
    #[test]
    fn short_codes_are_not_link_tokens() {
        assert!(!is_link_token("12345678"));
        assert!(!is_link_token(""));
        assert!(is_link_token(&Uuid::new_v4().to_string()));
        let t = new_link_token();
        assert_eq!(t.len(), 64);
        assert!(is_link_token(&t));
        assert_ne!(t, new_link_token());
    }

    #[tokio::test]
    async fn verify_email_rejects_short_code_without_db() {
        // Lazy pool to a dead address: the guard must reject before any
        // query is attempted.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://127.0.0.1:1/none")
            .unwrap();
        assert!(matches!(
            verify_email(&pool, "12345678", SECRET).await,
            Err(ApiError::InvalidVerificationToken)
        ));
    }

    fn sign_hs256<T: serde::Serialize>(claims: &T, alg: Algorithm, secret: &str) -> String {
        encode(
            &Header::new(alg),
            claims,
            &EncodingKey::from_secret(secret.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    fn claims_expiring_at(exp: chrono::DateTime<Utc>) -> JwtClaims {
        JwtClaims {
            sub: Uuid::new_v4(),
            entity_id: Uuid::new_v4(),
            role: "user".to_string(),
            iat: (exp - Duration::days(30)).timestamp(),
            exp: exp.timestamp(),
        }
    }

    #[test]
    fn validate_jwt_rejects_expired_token() {
        let token = sign_hs256(
            &claims_expiring_at(Utc::now() - Duration::hours(1)),
            Algorithm::HS256,
            SECRET,
        );
        assert!(matches!(
            validate_jwt(&token, SECRET),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn validate_jwt_rejects_other_hmac_algorithm() {
        let token = sign_hs256(
            &claims_expiring_at(Utc::now() + Duration::hours(1)),
            Algorithm::HS512,
            SECRET,
        );
        assert!(matches!(
            validate_jwt(&token, SECRET),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn validate_jwt_rejects_unsigned_alg_none_token() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = b64.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = b64.encode(
            serde_json::to_vec(&claims_expiring_at(Utc::now() + Duration::hours(1))).unwrap(),
        );
        let token = format!("{header}.{payload}.");
        assert!(matches!(
            validate_jwt(&token, SECRET),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn empty_secret_neither_signs_nor_verifies() {
        assert!(matches!(
            create_jwt(Uuid::new_v4(), Uuid::new_v4(), "user", ""),
            Err(ApiError::Internal(_))
        ));
        let token = create_jwt(Uuid::new_v4(), Uuid::new_v4(), "user", SECRET).unwrap();
        assert!(matches!(
            validate_jwt(&token, ""),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn es256_round_trips_with_the_configured_crypto_backend() {
        use p256::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};

        let signing_key = p256::SecretKey::from_slice(&[7u8; 32]).unwrap();
        let private_pem = signing_key.to_pkcs8_pem(LineEnding::LF).unwrap();
        let public_pem = signing_key
            .public_key()
            .to_public_key_pem(LineEnding::LF)
            .unwrap();
        let claims = claims_expiring_at(Utc::now() + Duration::hours(1));

        let token = encode(
            &Header::new(Algorithm::ES256),
            &claims,
            &EncodingKey::from_ec_pem(private_pem.as_bytes()).unwrap(),
        )
        .unwrap();
        let decoded = decode::<JwtClaims>(
            &token,
            &DecodingKey::from_ec_pem(public_pem.as_bytes()).unwrap(),
            &Validation::new(Algorithm::ES256),
        )
        .unwrap();
        assert_eq!(decoded.claims.sub, claims.sub);

        assert!(matches!(
            validate_jwt(&token, SECRET),
            Err(ApiError::Unauthorized)
        ));
    }

    #[test]
    fn create_jwt_preserves_admin_role() {
        let token = create_jwt(Uuid::new_v4(), Uuid::new_v4(), "admin", SECRET).expect("issuance");
        let claims = validate_jwt(&token, SECRET).expect("validation");
        assert_eq!(claims.role, "admin");
    }
}
