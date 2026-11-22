//! DB-backed regression tests for private-claim visibility.
//! Opt-in via `TEST_DATABASE_URL` like the other DB tests; without it
//! each test prints SKIP and passes. Never point it at prod.
//!
//! Bug: private standalone claims were only hidden by
//! `GET /v1/claims/{id}`. The HTML page, artifact, lineage, OG, search,
//! battle listings and the `claim.posted` webhook (delivered to EVERY
//! subscriber) all served / announced them. These tests pin the shared
//! gate (`claim_visibility::ensure_visible`), the battle listing
//! filter, and the webhook audience restriction.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::error::ApiError;
use crate::services::claim_visibility::ensure_visible;
use crate::services::webhook_dispatch::{Audience, lookup_subscriptions};

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for claim visibility DB tests");
        return None;
    };
    crate::db::run_migrations(&url)
        .await
        .expect("migrations failed against TEST_DATABASE_URL");
    PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .ok()
}

async fn seed_user(pool: &PgPool, role: &str) -> Uuid {
    let suffix = Uuid::new_v4();
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method)
         VALUES ($1, $2, 'email') RETURNING id",
    )
    .bind(format!("Vis {suffix}"))
    .bind(format!("vis-{suffix}.test"))
    .fetch_one(pool)
    .await
    .expect("insert entity");
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role)
         VALUES ($1, $2, $3, 'email', $4) RETURNING id",
    )
    .bind(entity_id)
    .bind(format!("Vis {suffix}"))
    .bind(format!("vis-{suffix}@example.test"))
    .bind(role)
    .fetch_one(pool)
    .await
    .expect("insert user")
}

async fn seed_claim(pool: &PgPool, author: Uuid, battle: Option<Uuid>, visibility: &str) -> Uuid {
    let suffix = Uuid::new_v4();
    sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, artifact_size,
                             visibility, slug, topic_tags)
         VALUES ($1, $2, '# t\n\n## Summary\nDisposable.', $3, $4, 1, $5, $6, ARRAY[]::text[])
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(format!("sha256:{suffix}"))
    .bind(format!("https://example.test/{suffix}.tar.gz"))
    .bind(visibility)
    .bind(format!("vis-claim-{suffix}"))
    .fetch_one(pool)
    .await
    .expect("insert claim")
}

#[tokio::test]
async fn private_claim_gate_hides_from_anonymous_and_strangers() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool, "user").await;
    let stranger = seed_user(&pool, "user").await;
    let admin = seed_user(&pool, "admin").await;
    let private = seed_claim(&pool, author, None, "private").await;
    let public = seed_claim(&pool, author, None, "public").await;

    assert!(matches!(
        ensure_visible(&pool, private, &None).await,
        Err(ApiError::ClaimNotFound)
    ));
    assert!(matches!(
        ensure_visible(&pool, private, &Some((stranger, "user".into()))).await,
        Err(ApiError::ClaimNotFound)
    ));
    ensure_visible(&pool, private, &Some((author, "user".into())))
        .await
        .expect("author sees own private claim");
    ensure_visible(&pool, private, &Some((admin, "admin".into())))
        .await
        .expect("admin sees private claim");
    ensure_visible(&pool, public, &None)
        .await
        .expect("public claim visible to anonymous");
}

#[tokio::test]
async fn battle_listing_excludes_private_claims() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool, "user").await;
    let battle: Uuid = sqlx::query_scalar(
        "INSERT INTO battles (author_id, title, description, domain_tags, status, slug)
         VALUES ($1, 'Vis battle', 'Disposable', ARRAY['test'], 'open', $2) RETURNING id",
    )
    .bind(author)
    .bind(format!("vis-battle-{}", Uuid::new_v4()))
    .fetch_one(&pool)
    .await
    .expect("insert battle");
    let private = seed_claim(&pool, author, Some(battle), "private").await;
    let public = seed_claim(&pool, author, Some(battle), "public").await;

    let (items, _) = crate::services::claim_service::list_claims(&pool, battle, None, None, 50)
        .await
        .expect("list claims");
    let ids: Vec<Uuid> = items.iter().map(|c| c.id).collect();
    assert!(ids.contains(&public));
    assert!(
        !ids.contains(&private),
        "private claim leaked into battle list"
    );
}

#[tokio::test]
async fn private_claim_webhooks_only_reach_author_and_admin_subscriptions() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool, "user").await;
    let stranger = seed_user(&pool, "user").await;
    let admin = seed_user(&pool, "admin").await;
    let mut subs = Vec::new();
    for owner in [author, stranger, admin] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO webhook_subscriptions
                 (owner_user_id, url, events, secret_hash, secret_plain, secret_preview)
             VALUES ($1, $2, ARRAY['claim.posted'], 'x', 'x', 'x') RETURNING id",
        )
        .bind(owner)
        .bind(format!("https://hooks.example.test/{}", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .expect("insert sub");
        subs.push(id);
    }
    let (author_sub, stranger_sub, admin_sub) = (subs[0], subs[1], subs[2]);

    let private_ids: Vec<Uuid> =
        lookup_subscriptions(&pool, "claim.posted", Audience::PrivateTo(author))
            .await
            .expect("lookup")
            .into_iter()
            .map(|s| s.id)
            .collect();
    assert!(private_ids.contains(&author_sub));
    assert!(private_ids.contains(&admin_sub));
    assert!(
        !private_ids.contains(&stranger_sub),
        "private claim announced to an unrelated subscriber"
    );

    let public_ids: Vec<Uuid> = lookup_subscriptions(&pool, "claim.posted", Audience::Everyone)
        .await
        .expect("lookup")
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert!(public_ids.contains(&stranger_sub));

    sqlx::query("DELETE FROM webhook_subscriptions WHERE id = ANY($1)")
        .bind(&subs)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
async fn voting_on_a_private_claim_is_rejected_as_not_found() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool, "user").await;
    let private = seed_claim(&pool, author, None, "private").await;
    let err = crate::services::vote_service::cast_vote(&pool, Uuid::new_v4(), private, "up")
        .await
        .expect_err("vote on a private claim must fail");
    assert!(matches!(err, ApiError::ClaimNotFound), "{err:?}");
}

#[tokio::test]
async fn voter_session_with_the_agents_verified_email_can_see_its_private_claim() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool, "user").await;
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(author)
        .fetch_one(&pool)
        .await
        .unwrap();
    let stranger_email = format!("stranger-{}@example.test", Uuid::new_v4());

    let session = |verified: Option<String>| {
        let pool = pool.clone();
        async move {
            let voter = crate::services::voter_service::find_or_create_voter(
                &pool,
                "email",
                &Uuid::new_v4().to_string(),
                "Voter",
                verified.as_deref(),
                None,
            )
            .await
            .unwrap();
            let token = crate::services::voter_service::create_session(
                &pool,
                voter.id,
                verified.as_deref(),
            )
            .await
            .unwrap();
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::COOKIE,
                format!("{}={token}", crate::web::cookies::SESSION)
                    .parse()
                    .unwrap(),
            );
            headers
        }
    };

    let owner = session(Some(email.to_uppercase())).await;
    let stranger = session(Some(stranger_email)).await;
    let unverified = session(None).await;

    use crate::services::claim_visibility::voter_session_owns;
    assert!(voter_session_owns(&pool, &owner, author).await);
    assert!(!voter_session_owns(&pool, &stranger, author).await);
    assert!(!voter_session_owns(&pool, &unverified, author).await);
    assert!(!voter_session_owns(&pool, &axum::http::HeaderMap::new(), author).await);
}
