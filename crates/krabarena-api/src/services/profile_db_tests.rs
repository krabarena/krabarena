use axum::http::{HeaderMap, HeaderValue};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::error::ApiError;
use crate::routes::users::authorize_agent_profile_edit;
use crate::services::profile_service::{self, OAuthProfile, ProfileUpdateRequest, PublicProfile};
use crate::services::{auth_service, voter_service};

const SECRET: &str = "profile-db-test-secret";

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for profile DB tests");
        return None;
    };
    static MIGRATED: tokio::sync::OnceCell<Result<(), String>> = tokio::sync::OnceCell::const_new();
    let res = MIGRATED
        .get_or_init(|| async {
            crate::db::run_migrations(&url)
                .await
                .map_err(|e| format!("{e:#}"))
        })
        .await;
    if let Err(e) = res {
        panic!("migrations failed against TEST_DATABASE_URL: {e}");
    }
    PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .ok()
}

fn unique_email() -> String {
    format!("profile-db-{}@example.test", Uuid::new_v4())
}

async fn seed_agent(pool: &PgPool, email: &str) -> (Uuid, Uuid) {
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method)
         VALUES ($1, $2, 'email') RETURNING id",
    )
    .bind(format!("Profile {email}"))
    .bind(format!("profile-{}.test", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert entity");
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method)
         VALUES ($1, 'agent', $2, 'email') RETURNING id",
    )
    .bind(entity_id)
    .bind(email)
    .fetch_one(pool)
    .await
    .expect("insert user");
    (user_id, entity_id)
}

fn github_profile(email: &str, name: &str, avatar: &str) -> OAuthProfile {
    OAuthProfile {
        provider: "github".into(),
        email: Some(email.into()),
        email_verified: true,
        full_name: Some(name.into()),
        username: Some("octo-jane".into()),
        profile_url: Some("https://github.com/octo-jane".into()),
        avatar_url: Some(avatar.into()),
        website_url: Some("jane.example.com".into()),
        x_handle: Some("octojane".into()),
        bio: Some("Benchmarks things".into()),
        company: Some("@acme".into()),
        location: Some("Berlin".into()),
        ..Default::default()
    }
}

async fn sign_in(pool: &PgPool, provider: &str, email: &str, profile: &OAuthProfile) -> Uuid {
    let voter = voter_service::find_or_create_voter(
        pool,
        provider,
        &format!("{provider}-{email}"),
        "jane",
        Some(email),
        profile.avatar_url.as_deref(),
    )
    .await
    .unwrap();
    profile_service::upsert_oauth_snapshot(pool, voter.id, profile)
        .await
        .unwrap();
    profile_service::sync_agent_for_email(pool, email)
        .await
        .unwrap();
    voter.id
}

async fn profile_of(pool: &PgPool, user_id: Uuid) -> PublicProfile {
    profile_service::get_public_profile(pool, user_id)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn oauth_snapshot_is_stored_and_refreshed_on_every_sign_in() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let voter = sign_in(
        &pool,
        "github",
        &email,
        &github_profile(&email, "Jane Doe", "https://avatars.example/1.png"),
    )
    .await;
    let snap = profile_service::oauth_snapshot(&pool, voter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.full_name.as_deref(), Some("Jane Doe"));
    assert_eq!(snap.username.as_deref(), Some("octo-jane"));
    assert_eq!(snap.location.as_deref(), Some("Berlin"));
    assert!(snap.email_verified);

    sign_in(
        &pool,
        "github",
        &email,
        &github_profile(&email, "Jane Q. Doe", "https://avatars.example/2.png"),
    )
    .await;
    let snap = profile_service::oauth_snapshot(&pool, voter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.full_name.as_deref(), Some("Jane Q. Doe"));
    assert_eq!(
        snap.avatar_url.as_deref(),
        Some("https://avatars.example/2.png")
    );
}

#[tokio::test]
async fn linking_prefills_empty_agent_fields_and_refreshes_only_oauth_owned_ones() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let (agent, _) = seed_agent(&pool, &email).await;

    sign_in(
        &pool,
        "github",
        &email,
        &github_profile(&email, "Jane Doe", "https://avatars.example/1.png"),
    )
    .await;
    let p = profile_of(&pool, agent).await;
    assert_eq!(p.full_name.as_deref(), Some("Jane Doe"));
    assert_eq!(
        p.github_url.as_deref(),
        Some("https://github.com/octo-jane")
    );
    assert_eq!(p.x_url.as_deref(), Some("https://x.com/octojane"));
    assert_eq!(p.website_url.as_deref(), Some("https://jane.example.com/"));
    assert_eq!(
        p.avatar_url.as_deref(),
        Some("https://avatars.example/1.png")
    );

    let edit = ProfileUpdateRequest {
        full_name: Some("Jane by hand".into()),
        x: Some(String::new()),
        ..Default::default()
    }
    .into_edit()
    .unwrap();
    profile_service::update_profile(&pool, agent, &edit)
        .await
        .unwrap();

    sign_in(
        &pool,
        "github",
        &email,
        &github_profile(&email, "Jane Renamed", "https://avatars.example/2.png"),
    )
    .await;
    let p = profile_of(&pool, agent).await;
    assert_eq!(p.full_name.as_deref(), Some("Jane by hand"));
    assert_eq!(p.x_url, None, "a field cleared by hand stays cleared");
    assert_eq!(
        p.avatar_url.as_deref(),
        Some("https://avatars.example/2.png"),
        "an OAuth-sourced avatar follows the provider"
    );
}

#[tokio::test]
async fn linking_never_overwrites_values_set_before_the_link() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let (agent, _) = seed_agent(&pool, &email).await;
    sqlx::query("UPDATE users SET avatar_url = 'https://storage.example/mine.png' WHERE id = $1")
        .bind(agent)
        .execute(&pool)
        .await
        .unwrap();
    profile_service::mark_avatar_edited(&pool, agent)
        .await
        .unwrap();
    let edit = ProfileUpdateRequest {
        github: Some("someone-else".into()),
        ..Default::default()
    }
    .into_edit()
    .unwrap();
    profile_service::update_profile(&pool, agent, &edit)
        .await
        .unwrap();

    sign_in(
        &pool,
        "github",
        &email,
        &github_profile(&email, "Jane Doe", "https://avatars.example/1.png"),
    )
    .await;
    let p = profile_of(&pool, agent).await;
    assert_eq!(
        p.avatar_url.as_deref(),
        Some("https://storage.example/mine.png")
    );
    assert_eq!(
        p.github_url.as_deref(),
        Some("https://github.com/someone-else")
    );
    assert_eq!(p.full_name.as_deref(), Some("Jane Doe"));
}

#[tokio::test]
async fn sign_in_without_matching_agent_only_stores_the_snapshot() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let voter = sign_in(
        &pool,
        "google",
        &email,
        &OAuthProfile {
            provider: "google".into(),
            email: Some(email.clone()),
            email_verified: true,
            given_name: Some("Ada".into()),
            family_name: Some("Lovelace".into()),
            avatar_url: Some("https://lh3.example/ada.png".into()),
            ..Default::default()
        },
    )
    .await;
    assert!(
        profile_service::linked_agent_id(&pool, &email)
            .await
            .unwrap()
            .is_none()
    );
    let snap = profile_service::oauth_snapshot(&pool, voter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snap.display_full_name().as_deref(), Some("Ada Lovelace"));

    let (agent, _) = seed_agent(&pool, &email).await;
    profile_service::sync_agent_for_email(&pool, &email)
        .await
        .unwrap();
    let p = profile_of(&pool, agent).await;
    assert_eq!(p.full_name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(p.avatar_url.as_deref(), Some("https://lh3.example/ada.png"));
    assert_eq!(p.github_url, None);
}

fn jwt_headers(user_id: Uuid, entity_id: Uuid) -> HeaderMap {
    let jwt = auth_service::create_jwt(user_id, entity_id, "user", SECRET).unwrap();
    let mut h = HeaderMap::new();
    h.insert("x-krabarena-token", HeaderValue::from_str(&jwt).unwrap());
    h
}

fn voter_headers(token: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("x-voter-token", HeaderValue::from_str(token).unwrap());
    h
}

async fn authorize(
    pool: &PgPool,
    headers: HeaderMap,
    target: Uuid,
) -> Result<&'static str, ApiError> {
    let scoped = headers.clone();
    crate::routes::auth::with_live_auth(pool, &scoped, SECRET, async {
        authorize_agent_profile_edit(pool, &headers, SECRET, target).await
    })
    .await
}

#[tokio::test]
async fn profile_edit_auth_rules() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let email = unique_email();
    let (agent, entity) = seed_agent(&pool, &email).await;
    let (other, other_entity) = seed_agent(&pool, &unique_email()).await;

    assert!(matches!(
        authorize(&pool, HeaderMap::new(), agent).await,
        Err(ApiError::Unauthorized)
    ));
    assert!(matches!(
        authorize(&pool, voter_headers("not-a-session"), agent).await,
        Err(ApiError::Unauthorized)
    ));

    assert_eq!(
        authorize(&pool, jwt_headers(agent, entity), agent)
            .await
            .unwrap(),
        "jwt"
    );
    assert!(matches!(
        authorize(&pool, jwt_headers(other, other_entity), agent).await,
        Err(ApiError::Forbidden)
    ));

    let linked =
        voter_service::find_or_create_voter(&pool, "email", &email, "owner", Some(&email), None)
            .await
            .unwrap();
    let linked_token = voter_service::create_session(&pool, linked.id, Some(&email))
        .await
        .unwrap();
    assert_eq!(
        authorize(&pool, voter_headers(&linked_token), agent)
            .await
            .unwrap(),
        "voter"
    );
    assert!(matches!(
        authorize(&pool, voter_headers(&linked_token), other).await,
        Err(ApiError::Forbidden)
    ));

    let stranger_email = unique_email();
    let stranger = voter_service::find_or_create_voter(
        &pool,
        "email",
        &stranger_email,
        "stranger",
        Some(&stranger_email),
        None,
    )
    .await
    .unwrap();
    let stranger_token = voter_service::create_session(&pool, stranger.id, Some(&stranger_email))
        .await
        .unwrap();
    assert!(matches!(
        authorize(&pool, voter_headers(&stranger_token), agent).await,
        Err(ApiError::Forbidden)
    ));
}

#[tokio::test]
async fn update_profile_round_trips_canonical_values() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (agent, _) = seed_agent(&pool, &unique_email()).await;
    let edit = ProfileUpdateRequest {
        full_name: Some("  Grace   Hopper ".into()),
        bio: Some("Compilers.\nNanoseconds.".into()),
        website: Some("hopper.example".into()),
        x: Some("@grace".into()),
        linkedin: Some("linkedin.com/in/grace-hopper".into()),
        github: Some("https://github.com/grace/".into()),
    }
    .into_edit()
    .unwrap();
    let saved = profile_service::update_profile(&pool, agent, &edit)
        .await
        .unwrap();
    assert_eq!(saved, profile_of(&pool, agent).await);
    assert_eq!(saved.full_name.as_deref(), Some("Grace Hopper"));
    assert_eq!(saved.bio.as_deref(), Some("Compilers.\nNanoseconds."));
    assert_eq!(
        saved.website_url.as_deref(),
        Some("https://hopper.example/")
    );
    assert_eq!(saved.x_url.as_deref(), Some("https://x.com/grace"));
    assert_eq!(
        saved.linkedin_url.as_deref(),
        Some("https://www.linkedin.com/in/grace-hopper")
    );
    assert_eq!(
        saved.github_url.as_deref(),
        Some("https://github.com/grace")
    );

    let missing = profile_service::update_profile(&pool, Uuid::new_v4(), &edit).await;
    assert!(matches!(missing, Err(ApiError::NotFound(_))));
}
