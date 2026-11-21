//! DB-backed regression tests for the 2026-11 authentication hardening
//! (email-code attempt cap, device-flow confirmation binding, verified
//! voter-session email, live JWT role lookup). Opt-in via
//! `TEST_DATABASE_URL` like the other DB tests; without it each test
//! prints SKIP and passes. Never point it at prod.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::routes::auth::{LiveAuth, invalidate_live_auth, lookup_live_auth};
use crate::routes::oauth::{
    MAX_FAILED_CODE_ATTEMPTS, approve_device_session, check_email_code, issue_email_codes,
};
use crate::services::{auth_service, voter_service};

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for auth DB tests");
        return None;
    };
    crate::db::run_migrations(&url)
        .await
        .expect("migrations failed against TEST_DATABASE_URL");
    PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .ok()
}

fn unique_email() -> String {
    format!("auth-db-{}@example.test", Uuid::new_v4())
}

/// Regression: `POST /auth/voter/email/verify` had no attempt cap, so an
/// 8-digit code could be brute-forced (only the per-IP rate limit, which
/// was itself bypassable, stood in the way). After 5 misses the code is
/// dead even when the right value is finally submitted.
#[tokio::test]
async fn email_code_dies_after_five_failures() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    issue_email_codes(&pool, &email, "12345678", &auth_service::new_link_token())
        .await
        .unwrap();

    for _ in 0..MAX_FAILED_CODE_ATTEMPTS {
        assert!(!check_email_code(&pool, &email, "00000000").await.unwrap());
    }
    assert!(
        !check_email_code(&pool, &email, "12345678").await.unwrap(),
        "correct code must be rejected once the attempt cap is reached"
    );
}

#[tokio::test]
async fn email_code_succeeds_once_and_is_scoped_to_identifier() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let other = unique_email();
    issue_email_codes(&pool, &email, "23456789", &auth_service::new_link_token())
        .await
        .unwrap();

    // Right code, wrong identifier → no match (and it doesn't burn the
    // victim's attempts: misses only count against `other`).
    assert!(!check_email_code(&pool, &other, "23456789").await.unwrap());
    assert!(check_email_code(&pool, &email, "23456789").await.unwrap());
    // Single use.
    assert!(!check_email_code(&pool, &email, "23456789").await.unwrap());
}

/// A fresh request issues a NEW code (counter 0) and kills the old,
/// exhausted one — it never resets the counter of an existing code.
#[tokio::test]
async fn new_email_login_replaces_exhausted_code() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    issue_email_codes(&pool, &email, "34567890", &auth_service::new_link_token())
        .await
        .unwrap();
    for _ in 0..MAX_FAILED_CODE_ATTEMPTS {
        check_email_code(&pool, &email, "00000000").await.unwrap();
    }
    issue_email_codes(&pool, &email, "45678901", &auth_service::new_link_token())
        .await
        .unwrap();
    assert!(
        !check_email_code(&pool, &email, "34567890").await.unwrap(),
        "old exhausted code must stay dead"
    );
    assert!(check_email_code(&pool, &email, "45678901").await.unwrap());
}

/// The 8-digit code is not accepted on the code-only magic-link path,
/// while the separate long link token is.
#[tokio::test]
async fn magic_link_accepts_only_long_token() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let link = auth_service::new_link_token();
    issue_email_codes(&pool, &email, "56789012", &link)
        .await
        .unwrap();
    assert!(
        auth_service::verify_email(&pool, "56789012", "s")
            .await
            .is_err()
    );
    let ok = auth_service::verify_email(&pool, &link, "s").await.unwrap();
    assert_eq!(ok.user.email.as_deref(), Some(email.as_str()));
}

async fn insert_device_row(pool: &PgPool, session: &Uuid) {
    sqlx::query(
        "INSERT INTO pending_verifications (verification_type, identifier, code, expires_at) \
         VALUES ('device', $1, $1, NOW() + INTERVAL '10 minutes')",
    )
    .bind(session.to_string())
    .execute(pool)
    .await
    .unwrap();
}

async fn stage(pool: &PgPool, session: &Uuid, binding: &str) {
    sqlx::query(
        "UPDATE pending_verifications SET pending_token = 'jwt-x', browser_binding = $2 \
         WHERE verification_type = 'device' AND code = $1",
    )
    .bind(session.to_string())
    .bind(binding)
    .execute(pool)
    .await
    .unwrap();
}

async fn result_token(pool: &PgPool, session: &Uuid) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT result_token FROM pending_verifications \
         WHERE verification_type = 'device' AND code = $1",
    )
    .bind(session.to_string())
    .fetch_optional(pool)
    .await
    .unwrap()
    .flatten()
}

/// Regression for the device-flow token theft: a staged JWT is only
/// released to the poller when the approving browser's binding matches,
/// and never before an explicit approval.
#[tokio::test]
async fn device_token_released_only_on_bound_approval() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let session = Uuid::new_v4();
    insert_device_row(&pool, &session).await;
    // Nothing staged yet: approval is a no-op.
    assert!(!approve_device_session(&pool, &session, "b1").await.unwrap());
    stage(&pool, &session, "b1").await;
    assert_eq!(
        result_token(&pool, &session).await,
        None,
        "not before approval"
    );
    // Another browser (attacker) can't approve.
    assert!(!approve_device_session(&pool, &session, "b2").await.unwrap());
    assert_eq!(result_token(&pool, &session).await, None);
    // The binding browser can.
    assert!(approve_device_session(&pool, &session, "b1").await.unwrap());
    assert_eq!(
        result_token(&pool, &session).await.as_deref(),
        Some("jwt-x")
    );
}

/// Admin tokens are minted only from the session's PROVEN email.
#[tokio::test]
async fn voter_session_records_verified_email() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let voter = voter_service::find_or_create_voter(
        &pool,
        "google",
        &Uuid::new_v4().to_string(),
        "t",
        Some(&email),
        None,
    )
    .await
    .unwrap();
    let unverified = voter_service::create_session(&pool, voter.id, None)
        .await
        .unwrap();
    let verified = voter_service::create_session(&pool, voter.id, Some(&email))
        .await
        .unwrap();
    assert_eq!(
        voter_service::session_verified_email(&pool, &unverified)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        voter_service::session_verified_email(&pool, &verified)
            .await
            .unwrap()
            .as_deref(),
        Some(email.as_str())
    );
}

/// Live JWT state reflects role changes, suspension and deletion.
#[tokio::test]
async fn live_auth_tracks_role_suspension_and_deletion() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method) \
         VALUES ('live-auth', $1, 'email') RETURNING id",
    )
    .bind(format!("live-auth-{}.test", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .unwrap();
    let uid: Uuid = sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role) \
         VALUES ($1, 'live', $2, 'email', 'admin') RETURNING id",
    )
    .bind(entity_id)
    .bind(&email)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        lookup_live_auth(&pool, uid).await,
        LiveAuth::Found {
            role: "admin".into(),
            suspended: false
        }
    );

    sqlx::query(
        "UPDATE users SET role = 'user', suspended_until = NOW() + INTERVAL '1 day' WHERE id = $1",
    )
    .bind(uid)
    .execute(&pool)
    .await
    .unwrap();
    invalidate_live_auth(uid);
    assert_eq!(
        lookup_live_auth(&pool, uid).await,
        LiveAuth::Found {
            role: "user".into(),
            suspended: true
        }
    );

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
    invalidate_live_auth(uid);
    assert_eq!(lookup_live_auth(&pool, uid).await, LiveAuth::Missing);
}
