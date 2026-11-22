//! DB-backed integration tests for the 0.21.0 admin soft-delete +
//! summary-integrity work. These exercise the SQL filters directly
//! (the part a pure unit test can't reach) against a throwaway battle
//! the test seeds and tears down itself.
//!
//! Covered:
//!   1. Soft-deleted battle / claim 404 publicly (get_battle /
//!      get_claim) + absent from list_battles / list_claims; restore
//!      brings them back.
//!   2. A soft-deleted claim stops counting toward its battle's
//!      verify/refute/claim totals (get_battle stats).
//!   3. A net-REFUTED claim (`refute_count > verify_count`) is excluded
//!      from the cumulative-wins aggregation that feeds the summary —
//!      the main spam vector. Restoring the verify balance re-includes
//!      it.
//!   4. A soft-deleted claim is also excluded from the cumulative-wins
//!      aggregation.
//!
//! Required env: TEST_DATABASE_URL (a Neon branch that already has
//! migration 053 applied). Marked `#[ignore]` so `cargo test
//! --workspace` skips them. Run:
//!
//!   TEST_DATABASE_URL=<url> cargo test -p krabarena-api \
//!       --test summary_integrity_e2e -- --ignored --nocapture

use krabarena_api::services::{battle_service, battle_summarizer, claim_service};
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

/// Seed a throwaway editor user (idempotent on email).
async fn seed_user(pool: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role) \
         VALUES ((SELECT id FROM entities LIMIT 1), 'Soft-Delete Seeder', \
                 'soft-delete-seed@integrity.test', 'email', 'admin') \
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
         VALUES ($1, 'Integrity test battle', 'desc', ARRAY['test'], \
                 'integrity-test-' || gen_random_uuid()::text) \
         RETURNING id",
    )
    .bind(author)
    .fetch_one(pool)
    .await
    .expect("seed battle")
}

/// Seed a claim with a leaderboard/winners structure + a verify/refute
/// balance. `winner` is the solution name written into both
/// `winners` and the leaderboard.
async fn seed_claim(
    pool: &sqlx::PgPool,
    battle: Uuid,
    author: Uuid,
    title: &str,
    winner: &str,
    verify: i32,
    refute: i32,
) -> Uuid {
    let body = format!("# {title}\n## Summary\nseed");
    let structure = serde_json::json!({
        "leaderboard": [{"solution": winner}],
        "winners": [winner],
    });
    sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, \
                             artifact_size, slug, claim_structure, winners, \
                             verify_count, refute_count) \
         VALUES ($1, $2, $3, $4, 'http://test/integrity', 1, \
                 'integrity-claim-' || gen_random_uuid()::text, $5, $6, $7, $8) \
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(&body)
    .bind(format!("integrity-{}", Uuid::new_v4()))
    .bind(&structure)
    .bind(vec![winner.to_string()])
    .bind(verify)
    .bind(refute)
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

#[tokio::test]
#[ignore]
async fn soft_deleted_battle_and_claim_hidden_then_restored() {
    let pool = pool().await;
    let author = seed_user(&pool).await;
    let battle = seed_battle(&pool, author).await;
    let claim = seed_claim(&pool, battle, author, "Live claim", "Alpha", 3, 0).await;

    // Baseline: battle + claim both readable; battle stats count the claim.
    assert!(battle_service::get_battle(&pool, battle).await.is_ok());
    assert!(claim_service::get_claim(&pool, claim).await.is_ok());
    let base = battle_service::get_battle(&pool, battle).await.unwrap();
    assert_eq!(base.claim_count, 1, "live claim should count");

    // --- Soft-delete the CLAIM ---
    sqlx::query("UPDATE claims SET deleted_at = NOW() WHERE id = $1")
        .bind(claim)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        claim_service::get_claim(&pool, claim).await.is_err(),
        "soft-deleted claim must 404 publicly"
    );
    let (list, _) = claim_service::list_claims(
        &pool,
        battle,
        krabarena_api::models::claim::ClaimSort::Engaged,
        None,
        50,
    )
    .await
    .unwrap();
    assert!(
        !list.iter().any(|c| c.id == claim),
        "soft-deleted claim must be absent from list_claims"
    );
    let after_del = battle_service::get_battle(&pool, battle).await.unwrap();
    assert_eq!(
        after_del.claim_count, 0,
        "soft-deleted claim must not count toward battle totals"
    );

    // Restore the claim.
    sqlx::query("UPDATE claims SET deleted_at = NULL WHERE id = $1")
        .bind(claim)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        claim_service::get_claim(&pool, claim).await.is_ok(),
        "restore must bring the claim back"
    );

    // --- Soft-delete the BATTLE ---
    sqlx::query("UPDATE battles SET deleted_at = NOW() WHERE id = $1")
        .bind(battle)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        battle_service::get_battle(&pool, battle).await.is_err(),
        "soft-deleted battle must 404 publicly"
    );
    let (battles, _) = battle_service::list_battles(&pool, None, None, None, 100)
        .await
        .unwrap();
    assert!(
        !battles.iter().any(|b| b.id == battle),
        "soft-deleted battle must be absent from list_battles"
    );

    // Restore the battle.
    sqlx::query("UPDATE battles SET deleted_at = NULL WHERE id = $1")
        .bind(battle)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        battle_service::get_battle(&pool, battle).await.is_ok(),
        "restore must bring the battle back"
    );

    cleanup(&pool, battle).await;
}

#[tokio::test]
#[ignore]
async fn net_refuted_and_soft_deleted_claims_excluded_from_cumulative() {
    let pool = pool().await;
    let author = seed_user(&pool).await;
    let battle = seed_battle(&pool, author).await;

    // A confirmed winner, a NET-REFUTED claim (the spam vector), and a
    // soft-deleted claim. Only "Alpha" (confirmed) should survive into
    // the cumulative aggregation.
    let _good = seed_claim(&pool, battle, author, "Confirmed", "Alpha", 4, 0).await;
    let _refuted = seed_claim(&pool, battle, author, "Refuted spam", "Spammer", 0, 5).await;
    let deleted = seed_claim(&pool, battle, author, "Deleted", "Ghost", 9, 0).await;
    sqlx::query("UPDATE claims SET deleted_at = NOW() WHERE id = $1")
        .bind(deleted)
        .execute(&pool)
        .await
        .unwrap();

    let cumulative = battle_summarizer::cumulative_wins_for_battle(&pool, battle)
        .await
        .expect("cumulative");
    let names: Vec<&str> = cumulative.iter().map(|(s, _, _)| s.as_str()).collect();

    assert!(
        names.contains(&"Alpha"),
        "confirmed winner must be aggregated. got: {names:?}"
    );
    assert!(
        !names.contains(&"Spammer"),
        "net-refuted claim must be excluded (anti-spam). got: {names:?}"
    );
    assert!(
        !names.contains(&"Ghost"),
        "soft-deleted claim must be excluded. got: {names:?}"
    );

    // Tie counts as NOT refuted: bump the refuted claim's verify_count
    // up to its refute_count and it should re-enter.
    sqlx::query("UPDATE claims SET verify_count = 5 WHERE battle_id = $1 AND refute_count = 5")
        .bind(battle)
        .execute(&pool)
        .await
        .unwrap();
    let cumulative2 = battle_summarizer::cumulative_wins_for_battle(&pool, battle)
        .await
        .expect("cumulative2");
    let names2: Vec<&str> = cumulative2.iter().map(|(s, _, _)| s.as_str()).collect();
    assert!(
        names2.contains(&"Spammer"),
        "a tie (verify == refute) is NOT net-refuted → re-included. got: {names2:?}"
    );

    cleanup(&pool, battle).await;
}
