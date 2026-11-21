//! DB-backed regression test for the 0.21.2 temperature-consistency fix.
//!
//! The battle heat-pill (🧊/🌤️/🔥/🌋) used to be computed from two
//! different event sets — the home-feed/list card read a stored snapshot
//! while the battle detail page recomputed live — so a battle could read
//! 🧊 Cold in its preview and 🌤️ Warm on its own page. The fix routes
//! BOTH paths through one event query, `temperature::fetch_events_for_battles`,
//! which excludes soft-deleted claims. This test pins that single source:
//! a battle with a soft-deleted claim yields the same deleted-excluded
//! event set regardless of which page asks, and the chip reflects live
//! events (not a stale `battles.temperature` snapshot).
//!
//! A pure unit test can't reach the SQL `deleted_at` filter — that's what
//! this exercises against a seeded throwaway battle it cleans up itself.
//!
//! Required env: TEST_DATABASE_URL. `#[ignore]` so the unit-only CI run
//! skips it. Run:
//!
//!   TEST_DATABASE_URL=<url> cargo test -p krabarena-api \
//!       --test temperature_e2e -- --ignored --nocapture

use krabarena_api::services::temperature::{
    self, TempBucket, TempEventKind, compute, live_temperature,
};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

async fn pool() -> sqlx::PgPool {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .expect("connect")
}

async fn seed_user(pool: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role) \
         VALUES ((SELECT id FROM entities LIMIT 1), 'Temp Seeder', \
                 'temp-consistency-seed@temp.test', 'email', 'admin') \
         ON CONFLICT (email) DO UPDATE SET role = EXCLUDED.role \
         RETURNING id",
    )
    .fetch_one(pool)
    .await
    .expect("seed user")
}

async fn seed_battle(pool: &sqlx::PgPool, author: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO battles (author_id, title, description, domain_tags, slug) \
         VALUES ($1, 'Temp consistency battle', 'desc', ARRAY['test'], \
                 'temp-consistency-' || gen_random_uuid()::text) \
         RETURNING id",
    )
    .bind(author)
    .fetch_one(pool)
    .await
    .expect("seed battle")
}

async fn seed_claim(
    pool: &sqlx::PgPool,
    battle: Uuid,
    author: Uuid,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, \
                             artifact_url, artifact_size, created_at) \
         VALUES ($1, $2, '# t', 'h' || gen_random_uuid()::text, \
                 'http://x', 1, $3) \
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("seed claim")
}

async fn cleanup(pool: &sqlx::PgPool, battle: Uuid) {
    let _ = sqlx::query(
        "DELETE FROM verifications WHERE claim_id IN \
         (SELECT id FROM claims WHERE battle_id = $1)",
    )
    .bind(battle)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM claims WHERE battle_id = $1")
        .bind(battle)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM battles WHERE id = $1")
        .bind(battle)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore]
async fn temperature_event_set_excludes_soft_deleted_and_is_single_source() {
    let pool = pool().await;
    let author = seed_user(&pool).await;
    let battle = seed_battle(&pool, author).await;

    let now = chrono::Utc::now();
    let ago = |s: i64| now - chrono::Duration::seconds(s);

    // Three live claims + one soft-deleted claim + one verify on a live one.
    let live1 = seed_claim(&pool, battle, author, ago(60)).await;
    let _live2 = seed_claim(&pool, battle, author, ago(120)).await;
    let _live3 = seed_claim(&pool, battle, author, ago(180)).await;
    let deleted = seed_claim(&pool, battle, author, ago(90)).await;
    sqlx::query("UPDATE claims SET deleted_at = now() WHERE id = $1")
        .bind(deleted)
        .execute(&pool)
        .await
        .expect("soft-delete claim");
    sqlx::query(
        "INSERT INTO verifications (claim_id, author_id, verdict) \
         VALUES ($1, $2, 'confirmed')",
    )
    .bind(live1)
    .bind(author)
    .execute(&pool)
    .await
    .expect("seed verify");

    // THE single event source both the feed and the detail page now use.
    let grouped = temperature::fetch_events_for_battles(&pool, &[battle])
        .await
        .expect("fetch events");
    let events = grouped.get(&battle).cloned().unwrap_or_default();

    let claim_events = events
        .iter()
        .filter(|e| matches!(e.kind, TempEventKind::Claim))
        .count();
    let verify_events = events
        .iter()
        .filter(|e| matches!(e.kind, TempEventKind::Verify))
        .count();

    // The soft-deleted claim's event must NOT appear (3 live, not 4); the
    // verify on a live claim does. This is the deleted_at filter that the
    // pure unit tests can't reach.
    assert_eq!(
        claim_events, 3,
        "soft-deleted claim must be excluded from temperature events: {events:?}"
    );
    assert_eq!(
        verify_events, 1,
        "verify on a live claim should be present: {events:?}"
    );

    // The single-battle wrapper goes through the same query → same set.
    let via_wrapper = temperature::fetch_battle_events(&pool, battle)
        .await
        .expect("fetch_battle_events");
    assert_eq!(
        via_wrapper.len(),
        events.len(),
        "single-battle wrapper and batch must return the same events"
    );

    // The chip reflects LIVE events, not a stale snapshot: even with a
    // stored snapshot of 0 (Cold), the score is the live `compute` and
    // lands above Cold — the exact preview-vs-page divergence we fixed.
    let score = live_temperature(&events, 0.0, now);
    assert_eq!(score, compute(&events, now), "live rule must equal compute");
    assert_ne!(
        TempBucket::from_score(score),
        TempBucket::Cold,
        "live activity (4 events) should not read Cold despite stored=0"
    );

    cleanup(&pool, battle).await;
}
