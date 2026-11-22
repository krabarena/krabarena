use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::PgPool;
use uuid::Uuid;

use crate::db::DB_QUERY_COUNT;
use crate::services::claim_service;

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for claim list DB tests");
        return None;
    };
    crate::db::run_migrations(&url)
        .await
        .expect("migrations failed against TEST_DATABASE_URL");
    Some(crate::db::create_pool(&url).await.expect("pool"))
}

async fn seed_user(pool: &PgPool) -> Uuid {
    let suffix = Uuid::new_v4();
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method)
         VALUES ($1, $2, 'email') RETURNING id",
    )
    .bind(format!("List {suffix}"))
    .bind(format!("list-{suffix}.test"))
    .fetch_one(pool)
    .await
    .expect("insert entity");
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role)
         VALUES ($1, $2, $3, 'email', 'user') RETURNING id",
    )
    .bind(entity_id)
    .bind(format!("List {suffix}"))
    .bind(format!("list-{suffix}@example.test"))
    .fetch_one(pool)
    .await
    .expect("insert user")
}

async fn seed_battle(pool: &PgPool, author: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO battles (author_id, title, description, domain_tags, status, slug)
         VALUES ($1, 'List battle', 'Disposable', ARRAY['test'], 'open', $2) RETURNING id",
    )
    .bind(author)
    .bind(format!("list-battle-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert battle")
}

async fn seed_claim(pool: &PgPool, author: Uuid, battle: Uuid) -> Uuid {
    let suffix = Uuid::new_v4();
    sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, artifact_size,
                             visibility, slug, topic_tags)
         VALUES ($1, $2, '# t\n\n## Summary\nDisposable.', $3, $4, 1, 'public', $5, ARRAY[]::text[])
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(format!("sha256:{suffix}"))
    .bind(format!("https://example.test/{suffix}.tar.gz"))
    .bind(format!("list-claim-{suffix}"))
    .fetch_one(pool)
    .await
    .expect("insert claim")
}

async fn warm_pool(pool: &PgPool) {
    let ping = || sqlx::query("SELECT pg_sleep(0.05)").execute(pool);
    let (a, b, c, d) = tokio::join!(ping(), ping(), ping(), ping());
    for r in [a, b, c, d] {
        r.expect("warm pool");
    }
}

async fn counted_list(
    pool: &PgPool,
    battle: Uuid,
) -> (Vec<crate::models::claim::ClaimResponse>, u64) {
    warm_pool(pool).await;
    let counter = Arc::new(AtomicU64::new(0));
    let (items, _) = DB_QUERY_COUNT
        .scope(
            counter.clone(),
            claim_service::list_claims(
                pool,
                battle,
                crate::models::claim::ClaimSort::Engaged,
                None,
                50,
            ),
        )
        .await
        .expect("list claims");
    (items, counter.load(Ordering::Relaxed))
}

#[tokio::test]
async fn battle_claim_list_query_count_does_not_grow_with_claims() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let owner = seed_user(&pool).await;

    let small = seed_battle(&pool, owner).await;
    seed_claim(&pool, owner, small).await;
    let (small_items, small_queries) = counted_list(&pool, small).await;
    assert_eq!(small_items.len(), 1);

    let big = seed_battle(&pool, owner).await;
    let mut authors = Vec::new();
    for _ in 0..4 {
        authors.push(seed_user(&pool).await);
    }
    for i in 0..12 {
        seed_claim(&pool, authors[i % authors.len()], big).await;
    }
    let (big_items, big_queries) = counted_list(&pool, big).await;
    assert_eq!(big_items.len(), 12);

    assert_eq!(small_queries, big_queries);
    assert!(
        big_queries <= 2,
        "expected list + one author batch, got {big_queries}"
    );

    for item in &big_items {
        let author = item.author.as_ref().expect("author attached");
        assert_eq!(author.user_id, item.author_id);
        assert_eq!(author.total_claims, 3);
        let single = claim_service::load_claim_author(&pool, item.author_id)
            .await
            .expect("single author")
            .expect("author exists");
        assert_eq!(single.display_name, author.display_name);
        assert_eq!(single.entity_domain, author.entity_domain);
        assert_eq!(single.total_claims, author.total_claims);
    }
}

#[tokio::test]
async fn compact_list_keeps_ids_and_drops_heavy_fields() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let owner = seed_user(&pool).await;
    let battle = seed_battle(&pool, owner).await;
    let claim = seed_claim(&pool, owner, battle).await;

    let (items, _) = counted_list(&pool, battle).await;
    let full = serde_json::to_value(crate::models::claim::claims_list_json(items, false)).unwrap();
    assert_eq!(full[0]["claim"], "# t\n\n## Summary\nDisposable.");

    let (items, _) = counted_list(&pool, battle).await;
    let compact =
        serde_json::to_value(crate::models::claim::claims_list_json(items, true)).unwrap();
    let row = compact[0].as_object().expect("object");
    assert_eq!(row["id"], claim.to_string());
    assert_eq!(row["claim"], "t");
    assert!(row.contains_key("author"));
    assert!(row.contains_key("urls"));
    for key in crate::models::claim::COMPACT_DROPPED_FIELDS {
        assert!(!row.contains_key(key), "{key} should be dropped");
    }
}
