//! DB-backed regression tests for UUID -> slug canonical redirects.
//!
//! These hit the actual axum routes and handlers with seeded Battle/Claim rows,
//! so they cover the extractor + SQL lookup + Redirect response path that pure
//! URL helper tests cannot reach.
//!
//! Required env: TEST_DATABASE_URL (a Neon branch with the current schema).
//! Marked `#[ignore]` so unit-only CI stays fast. Run:
//!
//!   TEST_DATABASE_URL=<url> cargo test -p krabarena-api \
//!       --test canonical_redirect_e2e -- --ignored --test-threads=1

use std::sync::Arc;

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use krabarena_api::{AppState, LeaderboardCaches, RecaptchaConfig, services, web};
use sqlx::postgres::PgPoolOptions;
use tower::util::ServiceExt;
use uuid::Uuid;

async fn pool() -> sqlx::PgPool {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .expect("connect")
}

fn app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState {
        proxy_pool: pool.clone(),
        pool,
        jwt_secret: "test-secret".into(),
        blob_store_url: "http://127.0.0.1:3000/static/artifacts".into(),
        private_blob_store_url: None,
        public_base_url: "http://127.0.0.1:3000".into(),
        discord_monitor: services::discord_monitor::DiscordMonitorConfig::from_lookup(|_| None),
        email_service: None,
        gemini: services::gemini_service::GeminiClient::new(None),
        proxy_http: reqwest::Client::new(),
        rate_limiter: None,
        leaderboards: Arc::new(LeaderboardCaches::new()),
        recaptcha: RecaptchaConfig::default(),
    };

    axum::Router::new()
        .route("/battles/{battle_id}", get(web::pages::battle_detail))
        .route("/claims/{claim_id}", get(web::pages::claim_detail))
        .layer(DefaultBodyLimit::disable())
        .with_state(state)
}

async fn seed_user(pool: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role) \
         VALUES ((SELECT id FROM entities LIMIT 1), 'Canonical Redirect Seeder', \
                 'canonical-redirect-seed@integrity.test', 'email', 'admin') \
         ON CONFLICT (email) DO UPDATE SET role = EXCLUDED.role \
         RETURNING id",
    )
    .fetch_one(pool)
    .await
    .expect("seed user")
}

async fn seed_battle(pool: &sqlx::PgPool, author: Uuid, slug: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO battles (author_id, title, description, domain_tags, slug) \
         VALUES ($1, 'Canonical redirect test battle', 'desc', ARRAY['test'], $2) \
         RETURNING id",
    )
    .bind(author)
    .bind(slug)
    .fetch_one(pool)
    .await
    .expect("seed battle")
}

async fn seed_claim(pool: &sqlx::PgPool, battle: Uuid, author: Uuid, slug: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, \
                             artifact_size, slug) \
         VALUES ($1, $2, '# Canonical redirect claim\n## Summary\nseed', $3, \
                 'http://test/canonical-redirect', 1, $4) \
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(format!("canonical-redirect-{}", Uuid::new_v4()))
    .bind(slug)
    .fetch_one(pool)
    .await
    .expect("seed claim")
}

async fn cleanup(pool: &sqlx::PgPool, battle: Uuid) {
    let _ = sqlx::query("DELETE FROM claims WHERE battle_id = $1")
        .bind(battle)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM battles WHERE id = $1")
        .bind(battle)
        .execute(pool)
        .await;
}

async fn get_location(pool: sqlx::PgPool, uri: String) -> (StatusCode, String, Option<String>) {
    let response = app(pool)
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
    let status = response.status();
    let headers = response.headers();
    let location = headers
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("missing Location header")
        .to_string();
    let cache_control = headers
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (status, location, cache_control)
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL + single-thread; run with --ignored --test-threads=1"]
async fn battle_uuid_redirect_preserves_query_in_location() {
    let pool = pool().await;
    let author = seed_user(&pool).await;
    let slug = format!("canonical-redirect-battle-{}", Uuid::new_v4());
    let battle = seed_battle(&pool, author, &slug).await;
    let query = "utm_source=github&utm_medium=issue&utm_campaign=github_issues_outreach&utm_content=7e7ab1e4-91a2-47e2-b046-8bfbfe1e6824&utm_term=mcp%20server";

    let (status, location, cache_control) =
        get_location(pool.clone(), format!("/battles/{battle}?{query}")).await;

    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(location, format!("/battles/{slug}?{query}"));
    assert_eq!(cache_control.as_deref(), Some("no-store"));

    cleanup(&pool, battle).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL + single-thread; run with --ignored --test-threads=1"]
async fn claim_uuid_redirect_preserves_query_in_location() {
    let pool = pool().await;
    let author = seed_user(&pool).await;
    let battle_slug = format!("canonical-redirect-battle-{}", Uuid::new_v4());
    let claim_slug = format!("canonical-redirect-claim-{}", Uuid::new_v4());
    let battle = seed_battle(&pool, author, &battle_slug).await;
    let claim = seed_claim(&pool, battle, author, &claim_slug).await;
    let query = "utm_source=github&utm_medium=discussion&utm_campaign=github_claim_invitation&utm_content=2cde0a8a-28f4-47b9-aa46-7af18bc532c1&utm_term=Claude%20Skill";

    let (status, location, cache_control) =
        get_location(pool.clone(), format!("/claims/{claim}?{query}")).await;

    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(location, format!("/claims/{claim_slug}?{query}"));
    assert_eq!(cache_control.as_deref(), Some("no-store"));

    cleanup(&pool, battle).await;
}
