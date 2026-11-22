use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::error::ApiError;
use crate::models::claim::ClaimSort;
use crate::services::{battle_service, claim_service, verification_service};

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for CLI/web parity DB tests");
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

async fn seed_user(pool: &PgPool) -> Uuid {
    let suffix = Uuid::new_v4();
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method)
         VALUES ($1, $2, 'email') RETURNING id",
    )
    .bind(format!("Parity {suffix}"))
    .bind(format!("parity-{suffix}.test"))
    .fetch_one(pool)
    .await
    .expect("insert entity");
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role)
         VALUES ($1, $2, $3, 'email', 'user') RETURNING id",
    )
    .bind(entity_id)
    .bind(format!("Parity {suffix}"))
    .bind(format!("parity-{suffix}@example.test"))
    .fetch_one(pool)
    .await
    .expect("insert user")
}

async fn seed_battle(pool: &PgPool, author: Uuid, status: &str, parent: Option<Uuid>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO battles (author_id, parent_id, title, description, domain_tags, status, slug,
                              executive_summary, short_summary, produced_by)
         VALUES ($1, $2, 'Parity battle', 'Disposable', ARRAY['parity'], $3, $4,
                 'Exec summary text', 'Short summary text', ARRAY['fermatix'])
         RETURNING id",
    )
    .bind(author)
    .bind(parent)
    .bind(status)
    .bind(format!("parity-battle-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert battle")
}

async fn seed_claim(
    pool: &PgPool,
    author: Uuid,
    battle: Uuid,
    created_hours_ago: i32,
    updated_hours_ago: i32,
) -> Uuid {
    let suffix = Uuid::new_v4();
    sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, artifact_size,
                             visibility, slug, topic_tags, created_at, updated_at)
         VALUES ($1, $2, '# Parity title\n\n## Summary\nDisposable.', $3, $4, 1, 'public', $5,
                 ARRAY[]::text[], NOW() - make_interval(hours => $6),
                 NOW() - make_interval(hours => $7))
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(format!("sha256:{suffix}"))
    .bind(format!("https://example.test/{suffix}.tar.gz"))
    .bind(format!("parity-claim-{suffix}"))
    .bind(created_hours_ago)
    .bind(updated_hours_ago)
    .fetch_one(pool)
    .await
    .expect("insert claim")
}

#[test]
fn claim_sort_accepts_aliases_and_rejects_unknown() {
    assert_eq!(ClaimSort::parse(None).unwrap(), ClaimSort::Engaged);
    assert_eq!(
        ClaimSort::parse(Some("default")).unwrap(),
        ClaimSort::Engaged
    );
    assert_eq!(ClaimSort::parse(Some("newest")).unwrap(), ClaimSort::Newest);
    assert_eq!(
        ClaimSort::parse(Some("verified")).unwrap(),
        ClaimSort::Confirmed
    );
    assert_eq!(
        ClaimSort::parse(Some("Confirmed")).unwrap(),
        ClaimSort::Confirmed
    );
    assert_eq!(
        ClaimSort::parse(Some("disconfirmed")).unwrap(),
        ClaimSort::Refuted
    );
    assert!(matches!(
        ClaimSort::parse(Some("bogus")),
        Err(ApiError::ValidationError(_))
    ));
}

#[test]
fn verdict_filter_maps_wire_and_db_names_to_db_values() {
    let f = verification_service::normalize_verdict_filter;
    assert_eq!(f(None).unwrap(), None);
    assert_eq!(f(Some("")).unwrap(), None);
    assert_eq!(f(Some("refuted")).unwrap(), Some("disconfirmed"));
    assert_eq!(f(Some("disconfirmed")).unwrap(), Some("disconfirmed"));
    assert_eq!(f(Some("verified")).unwrap(), Some("confirmed"));
    assert_eq!(f(Some("CONFIRMED")).unwrap(), Some("confirmed"));
    assert!(matches!(
        f(Some("maybe")),
        Err(ApiError::ValidationError(_))
    ));
}

#[test]
fn battle_status_filter_maps_legacy_values() {
    let f = battle_service::normalize_status_filter;
    assert_eq!(f(None).unwrap(), None);
    assert_eq!(f(Some("open")).unwrap().as_deref(), Some("open"));
    assert_eq!(f(Some("contested")).unwrap().as_deref(), Some("active"));
    assert_eq!(f(Some("holding")).unwrap().as_deref(), Some("active"));
    assert_eq!(f(Some("Settled")).unwrap().as_deref(), Some("settled"));
    assert!(matches!(
        f(Some("resolved")),
        Err(ApiError::ValidationError(_))
    ));
}

#[tokio::test]
async fn battle_claims_default_order_matches_web_updated_at_desc() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool).await;
    let battle = seed_battle(&pool, author, "active", None).await;
    let old_but_engaged = seed_claim(&pool, author, battle, 48, 1).await;
    let new_untouched = seed_claim(&pool, author, battle, 5, 5).await;

    let (engaged, _) = claim_service::list_claims(&pool, battle, ClaimSort::Engaged, None, 10)
        .await
        .expect("list engaged");
    let ids: Vec<Uuid> = engaged.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec![old_but_engaged, new_untouched]);
    assert_eq!(engaged[0].title, "Parity title");

    let (newest, _) = claim_service::list_claims(&pool, battle, ClaimSort::Newest, None, 10)
        .await
        .expect("list newest");
    let ids: Vec<Uuid> = newest.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec![new_untouched, old_but_engaged]);

    let (page1, meta) = claim_service::list_claims(&pool, battle, ClaimSort::Engaged, None, 1)
        .await
        .expect("page 1");
    assert!(meta.has_more);
    let (page2, meta2) = claim_service::list_claims(
        &pool,
        battle,
        ClaimSort::Engaged,
        meta.next_cursor.as_deref(),
        1,
    )
    .await
    .expect("page 2");
    assert!(!meta2.has_more);
    assert_eq!(page1[0].id, old_but_engaged);
    assert_eq!(page2[0].id, new_untouched);
}

#[tokio::test]
async fn refuted_verdict_filter_returns_disconfirmed_rows() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool).await;
    let battle = seed_battle(&pool, author, "active", None).await;
    let claim = seed_claim(&pool, author, battle, 1, 1).await;
    for verdict in ["confirmed", "disconfirmed"] {
        sqlx::query(
            "INSERT INTO verifications (claim_id, author_id, verdict, environment, output_hash)
             VALUES ($1, $2, $3, 'parity', 'sha256:parity')",
        )
        .bind(claim)
        .bind(author)
        .bind(verdict)
        .execute(&pool)
        .await
        .expect("insert verification");
    }

    let refuted = verification_service::normalize_verdict_filter(Some("refuted")).unwrap();
    let (items, _) = verification_service::list_verifications(&pool, claim, refuted, None, 10)
        .await
        .expect("list refuted");
    assert_eq!(items.len(), 1, "`refuted` must match stored `disconfirmed`");

    let verified = verification_service::normalize_verdict_filter(Some("verified")).unwrap();
    let (items, _) = verification_service::list_verifications(&pool, claim, verified, None, 10)
        .await
        .expect("list verified");
    assert_eq!(items.len(), 1, "`verified` must match stored `confirmed`");
}

#[tokio::test]
async fn legacy_status_filter_lists_active_battles() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool).await;
    let battle = seed_battle(&pool, author, "active", None).await;
    let status = battle_service::normalize_status_filter(Some("contested")).unwrap();
    let (items, _) =
        battle_service::list_battles(&pool, status.as_deref(), Some("parity"), None, 100)
            .await
            .expect("list battles");
    assert!(items.iter().any(|b| b.id == battle));
    assert!(items.iter().all(|b| b.status == "active"));
}

#[tokio::test]
async fn battle_response_exposes_web_page_fields() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let author = seed_user(&pool).await;
    let parent = seed_battle(&pool, author, "open", None).await;
    let child = seed_battle(&pool, author, "open", Some(parent)).await;

    let resp = battle_service::get_battle(&pool, parent)
        .await
        .expect("get parent");
    assert_eq!(resp.sub_battle_ids, vec![child]);
    assert_eq!(resp.executive_summary.as_deref(), Some("Exec summary text"));
    assert_eq!(resp.short_summary.as_deref(), Some("Short summary text"));
    assert_eq!(resp.produced_by, vec!["fermatix".to_string()]);
    let slug = resp.slug.clone().expect("slug");
    assert_eq!(
        resp.web_url,
        format!("https://krabarena.com/battles/{slug}")
    );

    let child_resp = battle_service::get_battle(&pool, child)
        .await
        .expect("get child");
    assert_eq!(child_resp.parent_id, Some(parent));
    assert!(child_resp.sub_battle_ids.is_empty());
}
