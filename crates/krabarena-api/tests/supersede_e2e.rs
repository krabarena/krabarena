//! DB-backed integration tests for the "hide superseded claims from
//! lists" feature (0.22.0). When a claim is edited (`claim edit` /
//! `claim post --predecessor`), a NEW version is created whose
//! `predecessor_id` points at the old one. The OLD version is then
//! "superseded" and must drop out of every public LIST / FEED /
//! AGGREGATE — while staying reachable by direct fetch + lineage.
//!
//! Covered (the part a pure unit test can't reach — the SQL filter):
//!   1. After an edit, the predecessor is ABSENT from `list_claims`
//!      (per-battle) and `list_latest_claims` (global feed); the HEAD
//!      (new version) is PRESENT.
//!   2. The predecessor is STILL returned by `get_claim` (direct URL)
//!      and STILL appears in `get_lineage` of the head.
//!   3. The battle's claim COUNT (get_battle stats) counts the head
//!      only — NOT both versions.
//!   4. cumulative_wins counts the head's winner only — an edited
//!      claim does NOT double-count its win (predecessor + successor
//!      sharing the same winner would otherwise inflate the tally).
//!
//! Required env: TEST_DATABASE_URL (a Neon branch with the current
//! schema). Marked `#[ignore]` so `cargo test --workspace` skips them.
//! Run:
//!
//!   TEST_DATABASE_URL=<url> cargo test -p krabarena-api \
//!       --test supersede_e2e -- --ignored --nocapture

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

async fn seed_user(pool: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, role) \
         VALUES ((SELECT id FROM entities LIMIT 1), 'Supersede Seeder', \
                 'supersede-seed@integrity.test', 'email', 'admin') \
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
         VALUES ($1, 'Supersede test battle', 'desc', ARRAY['test'], \
                 'supersede-test-' || gen_random_uuid()::text) \
         RETURNING id",
    )
    .bind(author)
    .fetch_one(pool)
    .await
    .expect("seed battle")
}

/// Seed a claim. `predecessor` links it as a successor (the edit case).
/// `winner` is written to both `winners` + the leaderboard so the
/// cumulative-wins aggregation has something to count.
#[allow(clippy::too_many_arguments)]
async fn seed_claim(
    pool: &sqlx::PgPool,
    battle: Uuid,
    author: Uuid,
    title: &str,
    winner: &str,
    verify: i32,
    refute: i32,
    predecessor: Option<Uuid>,
) -> Uuid {
    let body = format!("# {title}\n## Summary\nseed");
    let structure = serde_json::json!({
        "leaderboard": [{"solution": winner}],
        "winners": [winner],
    });
    sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, \
                             artifact_size, slug, claim_structure, winners, \
                             verify_count, refute_count, predecessor_id) \
         VALUES ($1, $2, $3, $4, 'http://test/supersede', 1, \
                 'supersede-claim-' || gen_random_uuid()::text, $5, $6, $7, $8, $9) \
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(&body)
    .bind(format!("supersede-{}", Uuid::new_v4()))
    .bind(&structure)
    .bind(vec![winner.to_string()])
    .bind(verify)
    .bind(refute)
    .bind(predecessor)
    .fetch_one(pool)
    .await
    .expect("seed claim")
}

async fn cleanup(pool: &sqlx::PgPool, battle: Uuid) {
    // Children first (the successor FK-references the predecessor).
    let _ = sqlx::query("UPDATE claims SET predecessor_id = NULL WHERE battle_id = $1")
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
async fn superseded_claim_hidden_from_lists_but_reachable_directly() {
    let pool = pool().await;
    let author = seed_user(&pool).await;
    let battle = seed_battle(&pool, author).await;

    // v1 (the predecessor) then v2 (the edit) superseding it. Both win
    // with the same solution so we can prove wins aren't double-counted.
    let v1 = seed_claim(&pool, battle, author, "Answer v1", "SolutionA", 1, 0, None).await;
    let v2 = seed_claim(
        &pool,
        battle,
        author,
        "Answer v2",
        "SolutionA",
        1,
        0,
        Some(v1),
    )
    .await;

    // 1. Per-battle list: v1 absent, v2 present.
    let (battle_claims, _) = claim_service::list_claims(
        &pool,
        battle,
        krabarena_api::models::claim::ClaimSort::Engaged,
        None,
        100,
    )
    .await
    .expect("list_claims");
    let battle_ids: Vec<Uuid> = battle_claims.iter().map(|c| c.id).collect();
    assert!(
        !battle_ids.contains(&v1),
        "superseded v1 must NOT appear in list_claims"
    );
    assert!(
        battle_ids.contains(&v2),
        "head v2 must appear in list_claims"
    );

    // 1b. Global latest feed: same invariant.
    let (latest, _) = claim_service::list_latest_claims(&pool, None, None, 100)
        .await
        .expect("list_latest_claims");
    let latest_ids: Vec<Uuid> = latest.iter().map(|c| c.id).collect();
    assert!(
        !latest_ids.contains(&v1),
        "superseded v1 must NOT appear in list_latest_claims"
    );
    assert!(
        latest_ids.contains(&v2),
        "head v2 must appear in list_latest_claims"
    );

    // 2. Direct fetch still resolves v1, and lineage of v2 includes it.
    let fetched = claim_service::get_claim(&pool, v1)
        .await
        .expect("get_claim must still return the superseded version");
    assert_eq!(fetched.id, v1);
    let lineage = claim_service::get_lineage(&pool, v2)
        .await
        .expect("get_lineage");
    let lineage_ids: Vec<Uuid> = lineage.iter().map(|c| c.id).collect();
    assert!(
        lineage_ids.contains(&v1) && lineage_ids.contains(&v2),
        "lineage of the head must include BOTH versions, got {lineage_ids:?}"
    );

    // 3. Battle claim COUNT = 1 (head only), not 2.
    let battle_resp = battle_service::get_battle(&pool, battle)
        .await
        .expect("get_battle");
    assert_eq!(
        battle_resp.claim_count, 1,
        "battle claim_count must count the head only (got {})",
        battle_resp.claim_count
    );

    // 4. Cumulative wins: SolutionA has exactly 1 win (head), not 2.
    let wins = battle_summarizer::cumulative_wins_for_battle(&pool, battle)
        .await
        .expect("cumulative_wins");
    let solution_a = wins.iter().find(|(name, _, _)| name == "SolutionA");
    let (_, win_count, _) = solution_a.expect("SolutionA should appear in cumulative wins");
    assert_eq!(
        *win_count, 1,
        "an edited claim must not double-count its win (got {win_count})"
    );

    cleanup(&pool, battle).await;
}
