use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::error::ApiError;
use crate::services::agentid::{
    AgentIdentity, DEFAULT_ISSUER, owned_agent_for_verified_email, sign_in_agent,
};

pub(crate) async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for AgentID DB tests");
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

fn identity(owner_email: Option<&str>) -> AgentIdentity {
    let sub = Uuid::new_v4().simple().to_string();
    AgentIdentity {
        issuer: DEFAULT_ISSUER.into(),
        subject: sub.clone(),
        email: Some(format!("{sub}@agentmail.to")),
        display_name: "Research Krab".into(),
        owner_sub: owner_email.map(|_| "owner-org".to_string()),
        owner_email: owner_email.map(str::to_string),
        owner_name: owner_email.map(|_| "Maya Chen".to_string()),
    }
}

async fn identity_row(
    pool: &PgPool,
    id: &AgentIdentity,
) -> (Uuid, Option<String>, Option<String>, bool, Option<String>) {
    sqlx::query_as(
        "SELECT user_id, email, owner_email, owner_email_verified, owner_name \
         FROM user_identities WHERE provider = 'agentid' AND issuer = $1 AND subject = $2",
    )
    .bind(&id.issuer)
    .bind(&id.subject)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn first_sign_in_creates_an_agent_and_repeat_reuses_it() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let id = identity(Some("maya@acme.com"));
    let user = sign_in_agent(&pool, &id).await.unwrap();
    assert_eq!(user.auth_method, "agentid");
    assert_eq!(user.role, "user");
    assert_eq!(user.display_name, "Research Krab");
    assert_eq!(user.email, id.email);

    let mut again = id.clone();
    again.display_name = "Renamed".into();
    again.owner_name = Some("Maya C.".into());
    let second = sign_in_agent(&pool, &again).await.unwrap();
    assert_eq!(second.id, user.id);
    assert_eq!(second.display_name, "Research Krab");

    let (uid, email, owner_email, verified, owner_name) = identity_row(&pool, &id).await;
    assert_eq!(uid, user.id);
    assert_eq!(email, id.email);
    assert_eq!(owner_email.as_deref(), Some("maya@acme.com"));
    assert!(verified);
    assert_eq!(owner_name.as_deref(), Some("Maya C."));
}

#[tokio::test]
async fn open_client_sign_in_without_owner_claims_works() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let id = identity(None);
    let user = sign_in_agent(&pool, &id).await.unwrap();
    let (uid, _, owner_email, verified, owner_name) = identity_row(&pool, &id).await;
    assert_eq!(uid, user.id);
    assert_eq!(owner_email, None);
    assert!(!verified);
    assert_eq!(owner_name, None);
}

#[tokio::test]
async fn concurrent_first_sign_ins_create_one_agent() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let id = identity(None);
    let (a, b) = tokio::join!(sign_in_agent(&pool, &id), sign_in_agent(&pool, &id));
    assert_eq!(a.unwrap().id, b.unwrap().id);
}

async fn seed_user(pool: &PgPool, email: &str, auth_method: &str, role: &str) -> Uuid {
    let entity: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method) VALUES ($1, $1, 'email') RETURNING id",
    )
    .bind(format!("seed-{}.test", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role) \
         VALUES ($1, 'seed', $2, $3, $4) RETURNING id",
    )
    .bind(entity)
    .bind(email)
    .bind(auth_method)
    .bind(role)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn inbox_already_registered_by_email_link_is_adopted() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let id = identity(None);
    let existing = seed_user(&pool, id.email.as_deref().unwrap(), "email", "user").await;
    let user = sign_in_agent(&pool, &id).await.unwrap();
    assert_eq!(user.id, existing);
    assert_eq!(identity_row(&pool, &id).await.0, existing);
}

#[tokio::test]
async fn human_owned_account_is_never_taken_over_by_email_collision() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let id = identity(Some("someone-else@acme.com"));
    let existing = seed_user(&pool, id.email.as_deref().unwrap(), "github", "user").await;
    let err = sign_in_agent(&pool, &id).await.unwrap_err();
    assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    let bound: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM user_identities WHERE user_id = $1)")
            .bind(existing)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!bound);
}

#[tokio::test]
async fn admin_account_is_never_linked() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let id = identity(None);
    seed_user(&pool, id.email.as_deref().unwrap(), "email", "admin").await;
    assert!(sign_in_agent(&pool, &id).await.is_err());
}

#[tokio::test]
async fn second_agentid_subject_cannot_claim_a_bound_inbox() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let first = identity(None);
    sign_in_agent(&pool, &first).await.unwrap();
    let mut recreated = identity(None);
    recreated.email = first.email.clone();
    assert!(sign_in_agent(&pool, &recreated).await.is_err());
}

#[tokio::test]
async fn owner_resolves_to_their_agent_and_its_private_claims() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let owner = format!("owner-{}@acme.test", Uuid::new_v4());
    let id = identity(Some(&owner));
    let agent = sign_in_agent(&pool, &id).await.unwrap();
    assert_eq!(
        owned_agent_for_verified_email(&pool, &owner.to_uppercase())
            .await
            .unwrap(),
        Some(agent.id)
    );
    assert_eq!(
        owned_agent_for_verified_email(&pool, "nobody@acme.test")
            .await
            .unwrap(),
        None
    );

    let session_headers = |verified: Option<String>| {
        let pool = pool.clone();
        async move {
            let voter = crate::services::voter_service::find_or_create_voter(
                &pool,
                "email",
                &Uuid::new_v4().to_string(),
                "Owner",
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
    use crate::services::claim_visibility::voter_session_owns;
    assert!(voter_session_owns(&pool, &session_headers(Some(owner.clone())).await, agent.id).await);
    assert!(!voter_session_owns(&pool, &session_headers(None).await, agent.id).await);
    assert!(
        !voter_session_owns(
            &pool,
            &session_headers(Some("other@acme.test".into())).await,
            agent.id
        )
        .await
    );

    let open = identity(None);
    let unowned = sign_in_agent(&pool, &open).await.unwrap();
    assert!(!voter_session_owns(&pool, &session_headers(Some(owner)).await, unowned.id).await);
}
