//! DB-backed regression test for the home "Proposed battles · 7-day"
//! window (`battle_candidates_service::list_candidates` / `count_open`
//! with `recent_days`). Opt-in via `TEST_DATABASE_URL` like the other DB
//! tests; without it the test prints SKIP and passes. Never point it at
//! prod.
//!
//! Bug (2026-10-26): the panel had no time window, so after a quiet week
//! it kept showing months-old proposals ranked by lifetime votes.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::services::battle_candidates_service as svc;

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for battle candidate DB tests");
        return None;
    };
    crate::db::run_migrations(&url)
        .await
        .expect("migrations failed against TEST_DATABASE_URL");
    PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .ok()
}

async fn seed_voter(pool: &PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO voters (provider, provider_id, display_name)
         VALUES ('email', $1, 'candidate-test') RETURNING id",
    )
    .bind(Uuid::new_v4().to_string())
    .fetch_one(pool)
    .await
    .expect("seed voter")
}

async fn seed_candidate(pool: &PgPool, voter: Uuid, title: &str, age_days: i32) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO battle_candidates (title, rationale, proposer_voter_id, created_at)
         VALUES ($1, 'regression test', $2, now() - make_interval(days => $3))
         RETURNING id",
    )
    .bind(title)
    .bind(voter)
    .bind(age_days)
    .fetch_one(pool)
    .await
    .expect("seed candidate")
}

#[tokio::test]
async fn home_window_hides_stale_candidates_but_keeps_recently_voted_ones() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let tag = Uuid::new_v4().simple().to_string();
    let voter = seed_voter(&pool).await;
    let fresh = seed_candidate(&pool, voter, &format!("fresh proposal {tag}"), 1).await;
    let stale = seed_candidate(&pool, voter, &format!("stale proposal {tag}"), 90).await;
    let revived = seed_candidate(&pool, voter, &format!("revived proposal {tag}"), 90).await;
    // An old candidate upvoted this week still belongs in the 7-day panel.
    sqlx::query("INSERT INTO battle_candidate_votes (candidate_id, voter_id) VALUES ($1, $2)")
        .bind(revived)
        .bind(voter)
        .execute(&pool)
        .await
        .expect("seed vote");
    // Give the stale one lifetime votes from long ago: it must still be hidden.
    let old_voter = seed_voter(&pool).await;
    sqlx::query(
        "INSERT INTO battle_candidate_votes (candidate_id, voter_id, created_at)
         VALUES ($1, $2, now() - interval '60 days')",
    )
    .bind(stale)
    .bind(old_voter)
    .execute(&pool)
    .await
    .expect("seed old vote");

    let windowed: Vec<Uuid> = svc::list_candidates(&pool, None, 1000, 0, Some(7))
        .await
        .expect("list windowed")
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert!(windowed.contains(&fresh), "fresh candidate missing");
    assert!(
        windowed.contains(&revived),
        "recently voted candidate missing"
    );
    assert!(
        !windowed.contains(&stale),
        "stale candidate leaked into 7-day window"
    );

    let all: Vec<Uuid> = svc::list_candidates(&pool, None, 1000, 0, None)
        .await
        .expect("list all")
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert!(
        all.contains(&stale),
        "full list must still include stale candidates"
    );

    let n_window = svc::count_open(&pool, Some(7))
        .await
        .expect("count windowed");
    let n_all = svc::count_open(&pool, None).await.expect("count all");
    assert!(
        n_all >= n_window + 1,
        "count window must exclude the stale candidate"
    );

    sqlx::query("DELETE FROM battle_candidates WHERE id = ANY($1)")
        .bind(vec![fresh, stale, revived])
        .execute(&pool)
        .await
        .expect("cleanup");
}
