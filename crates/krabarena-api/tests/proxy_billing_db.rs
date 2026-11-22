//! DB-level tests for Battle Proxy actual-cost billing
//! (`proxy_charges`, migration 072): reservation under the per-battle
//! advisory lock, settlement, dedupe across a Monid run + its poll,
//! stale reservations, and the mixed flat/actual spend aggregation
//! that `proxy-info` and the budget gates read.
//!
//! Needs a THROWAWAY Postgres with pgvector — the test runs every
//! migration and inserts fixture users/battles. Never point it at a
//! shared Neon branch:
//!
//!   docker run -d --rm --name krab-pg -e POSTGRES_PASSWORD=pw \
//!     -p 127.0.0.1:55871:5432 pgvector/pgvector:pg16
//!   PROXY_BILLING_DATABASE_URL=postgres://postgres:pw@127.0.0.1:55871/postgres \
//!     cargo test -p krabarena-api --test proxy_billing_db -- --ignored --test-threads=1
//!
//! Marked `#[ignore]` so `cargo test --workspace` skips it.

use krabarena_api::models::proxy::ProxySpec;
use krabarena_api::services::proxy_billing::{ChargeDecision, ChargeSource};
use krabarena_api::services::proxy_service;
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::OnceCell;
use uuid::Uuid;

static MIGRATED: OnceCell<()> = OnceCell::const_new();

async fn pool() -> PgPool {
    let url = std::env::var("PROXY_BILLING_DATABASE_URL").expect("PROXY_BILLING_DATABASE_URL");
    MIGRATED
        .get_or_init(|| async {
            krabarena_api::db::run_migrations(&url)
                .await
                .expect("migrations");
            // Prod's `proxy_logs` got its full-capture columns outside
            // the migration files; a from-scratch DB lacks them and
            // `log_request`'s INSERT would silently fail. Throwaway DB
            // only — never run this against a shared branch.
            let pool = PgPoolOptions::new().connect(&url).await.expect("connect");
            sqlx::query(
                "ALTER TABLE proxy_logs \
                   ADD COLUMN IF NOT EXISTS request_body TEXT, \
                   ADD COLUMN IF NOT EXISTS request_headers JSONB, \
                   ADD COLUMN IF NOT EXISTS response_body TEXT, \
                   ADD COLUMN IF NOT EXISTS response_headers JSONB",
            )
            .execute(&pool)
            .await
            .expect("proxy_logs capture columns");
            pool.close().await;
        })
        .await;
    PgPoolOptions::new()
        .max_connections(12)
        .connect(&url)
        .await
        .expect("connect")
}

fn spec(budget_cents: i32, per_claim_cents: i32) -> ProxySpec {
    ProxySpec::validate(&json!({
        "services": {
            "keenable": {
                "upstream": "https://api.keenable.ai",
                "docs_url": "https://docs.keenable.ai/search",
                "cost_per_1k_cents": 100
            },
            "monid": {
                "upstream": "https://api.monid.ai",
                "docs_url": "https://monid.ai/SKILL.md",
                "cost_per_1k_cents": 0,
                "billing": {
                    "type": "response_json",
                    "cost_path": "cost.value",
                    "dedupe_key_path": "runId",
                    "max_per_request_cents": 0.4
                }
            },
            "openrouter": {
                "upstream": "https://openrouter.ai",
                "docs_url": "https://openrouter.ai/docs",
                "cost_per_1k_cents": 100,
                "billing": { "type": "response_json", "cost_path": "usage.cost" }
            }
        },
        "budget_cents": budget_cents,
        "per_claim_cents": per_claim_cents
    }))
    .unwrap()
}

/// Fresh entity + user + battle so tests don't see each other's rows.
async fn fixture(pool: &PgPool) -> (Uuid, Uuid) {
    let tag = Uuid::new_v4().simple().to_string();
    let (entity_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO entities (name, domain, verification_method) VALUES ($1, $2, 'email') RETURNING id",
    )
    .bind(format!("e-{tag}"))
    .bind(format!("{tag}.example"))
    .fetch_one(pool)
    .await
    .expect("entity");
    let (user_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (entity_id, display_name, email, auth_method) \
         VALUES ($1, $2, $3, 'email') RETURNING id",
    )
    .bind(entity_id)
    .bind(format!("u-{tag}"))
    .bind(format!("{tag}@example.com"))
    .fetch_one(pool)
    .await
    .expect("user");
    let (battle_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO battles (author_id, title, description, domain_tags, slug) \
         VALUES ($1, $2, 'proxy billing test', ARRAY['test'], $2) RETURNING id",
    )
    .bind(user_id)
    .bind(format!("b-{tag}"))
    .fetch_one(pool)
    .await
    .expect("battle");
    (battle_id, user_id)
}

async fn log(pool: &PgPool, log_id: Uuid, battle: Uuid, user: Uuid, svc: &str) {
    proxy_service::log_request(
        pool,
        log_id,
        battle,
        user,
        svc,
        "POST",
        "v1/run",
        None,
        None,
        &json!({}),
        200,
        Some(10),
        5,
        0,
        Some("{}"),
        &json!({}),
    )
    .await;
}

fn actual(micro: i64, key: Option<&str>) -> ChargeDecision {
    ChargeDecision {
        amount_micro: micro,
        source: ChargeSource::Actual,
        dedupe_key: key.map(str::to_string),
        reported_micro: None,
    }
}

fn service<'a>(
    info: &'a krabarena_api::models::proxy::ProxyInfo,
    name: &str,
) -> &'a krabarena_api::models::proxy::ServicePublicInfo {
    info.service_details
        .iter()
        .find(|d| d.name == name)
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn monid_run_and_poll_are_billed_once() {
    let pool = pool().await;
    let s = spec(0, 0);
    let (battle, user) = fixture(&pool).await;

    // POST /v1/run → COMPLETED inline, $0.004.
    let run_log = Uuid::new_v4();
    let c1 = proxy_service::reserve_charge(&pool, &s, battle, user, "monid", run_log, 4_000, 0)
        .await
        .unwrap();
    assert_eq!(
        proxy_service::settle_charge(&pool, c1, &actual(4_000, Some("run_1"))).await,
        4_000
    );
    log(&pool, run_log, battle, user, "monid").await;

    // GET /v1/runs/run_1 repeats the same cost → duplicate, 0.
    let poll_log = Uuid::new_v4();
    let c2 = proxy_service::reserve_charge(&pool, &s, battle, user, "monid", poll_log, 4_000, 0)
        .await
        .unwrap();
    assert_eq!(
        proxy_service::settle_charge(&pool, c2, &actual(4_000, Some("run_1"))).await,
        0
    );
    log(&pool, poll_log, battle, user, "monid").await;

    let (src, dup): (String, Option<String>) =
        sqlx::query_as("SELECT source, duplicate_of_key FROM proxy_charges WHERE id = $1")
            .bind(c2)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((src.as_str(), dup.as_deref()), ("duplicate", Some("run_1")));

    // The same runId on ANOTHER battle is billed independently.
    let (other, other_user) = fixture(&pool).await;
    let c3 = proxy_service::reserve_charge(
        &pool,
        &s,
        other,
        other_user,
        "monid",
        Uuid::new_v4(),
        4_000,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        proxy_service::settle_charge(&pool, c3, &actual(4_000, Some("run_1"))).await,
        4_000
    );

    let info = proxy_service::get_proxy_info_with_spec(&pool, battle, &s)
        .await
        .unwrap();
    let m = service(&info, "monid");
    assert_eq!(m.request_count, 2);
    assert_eq!(m.actual_cost_requests, 1);
    assert!((m.spent_usd - 0.004).abs() < 1e-12, "{}", m.spent_usd);
    assert!((info.spent_usd - 0.004).abs() < 1e-12);
}

#[tokio::test]
#[ignore]
async fn pending_run_then_completed_poll_bills_the_poll() {
    let pool = pool().await;
    let s = spec(0, 0);
    let (battle, user) = fixture(&pool).await;
    let pending = ChargeDecision {
        amount_micro: 0,
        source: ChargeSource::Fallback,
        dedupe_key: None,
        reported_micro: None,
    };
    let c1 =
        proxy_service::reserve_charge(&pool, &s, battle, user, "monid", Uuid::new_v4(), 4_000, 0)
            .await
            .unwrap();
    assert_eq!(proxy_service::settle_charge(&pool, c1, &pending).await, 0);
    let c2 =
        proxy_service::reserve_charge(&pool, &s, battle, user, "monid", Uuid::new_v4(), 4_000, 0)
            .await
            .unwrap();
    assert_eq!(
        proxy_service::settle_charge(&pool, c2, &actual(4_000, Some("run_2"))).await,
        4_000
    );
    let info = proxy_service::get_proxy_info_with_spec(&pool, battle, &s)
        .await
        .unwrap();
    assert!((service(&info, "monid").spent_usd - 0.004).abs() < 1e-12);
}

#[tokio::test]
#[ignore]
async fn concurrent_reservations_respect_the_battle_budget() {
    let pool = pool().await;
    // 1¢ budget, each Monid call reserves its 0.4¢ cap. Reservations
    // at 0, 0.4, 0.8¢ spent pass; at 1.2¢ the cap is reached. Without
    // the advisory lock all 10 would pass the check concurrently.
    let s = spec(1, 0);
    let (battle, user) = fixture(&pool).await;
    let mut handles = Vec::new();
    for _ in 0..10 {
        let pool = pool.clone();
        let s = s.clone();
        handles.push(tokio::spawn(async move {
            proxy_service::reserve_charge(
                &pool,
                &s,
                battle,
                user,
                "monid",
                Uuid::new_v4(),
                4_000,
                0,
            )
            .await
            .is_ok()
        }));
    }
    let mut ok = 0;
    for h in handles {
        ok += h.await.unwrap() as i32;
    }
    assert_eq!(
        ok, 3,
        "exactly three 0.4¢ reservations fit under a 1¢ budget"
    );

    // A flat-billed call is now blocked too: in-flight reservations count.
    let err = proxy_service::check_budgets(&pool, &s, battle, user)
        .await
        .unwrap_err();
    assert!(
        format!("{err:?}").contains("battle budget exhausted"),
        "{err:?}"
    );
}

#[tokio::test]
#[ignore]
async fn per_claimer_cap_uses_actual_costs() {
    let pool = pool().await;
    let s = spec(0, 1); // 1¢ per claimer
    let (battle, user) = fixture(&pool).await;
    let c =
        proxy_service::reserve_charge(&pool, &s, battle, user, "monid", Uuid::new_v4(), 4_000, 0)
            .await
            .unwrap();
    proxy_service::settle_charge(&pool, c, &actual(10_000, Some("big"))).await;
    assert!(
        proxy_service::reserve_charge(&pool, &s, battle, user, "monid", Uuid::new_v4(), 4_000, 0)
            .await
            .is_err()
    );
    // Another claimer on the same battle is unaffected.
    let (_, other_user) = fixture(&pool).await;
    assert!(
        proxy_service::check_budgets(&pool, &s, battle, other_user)
            .await
            .is_ok()
    );
}

#[tokio::test]
#[ignore]
async fn legacy_flat_rows_and_stale_reservations() {
    let pool = pool().await;
    let s = spec(0, 0);
    let (battle, user) = fixture(&pool).await;

    // Two OpenRouter calls from before billing was enabled: flat 0.1¢ each.
    log(&pool, Uuid::new_v4(), battle, user, "openrouter").await;
    log(&pool, Uuid::new_v4(), battle, user, "openrouter").await;
    // One actual-cost call at 28µ$.
    let l = Uuid::new_v4();
    let c = proxy_service::reserve_charge(&pool, &s, battle, user, "openrouter", l, 1_000, 1_000)
        .await
        .unwrap();
    proxy_service::settle_charge(&pool, c, &actual(28, None)).await;
    log(&pool, l, battle, user, "openrouter").await;
    // Flat keenable call.
    log(&pool, Uuid::new_v4(), battle, user, "keenable").await;
    // A reservation that never settled (process died mid-call), 10 min
    // old: counts at its 40_000µ$ reserve (the per-request cap) — the
    // sponsor was probably billed upstream, so never at less.
    let stale = proxy_service::reserve_charge(
        &pool,
        &s,
        battle,
        user,
        "openrouter",
        Uuid::new_v4(),
        40_000,
        1_000,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE proxy_charges SET created_at = now() - interval '10 minutes' WHERE id = $1",
    )
    .bind(stale)
    .execute(&pool)
    .await
    .unwrap();

    let info = proxy_service::get_proxy_info_with_spec(&pool, battle, &s)
        .await
        .unwrap();
    let or = service(&info, "openrouter");
    assert_eq!(or.request_count, 3);
    assert_eq!(or.actual_cost_requests, 1);
    // 2 × 1000 (flat history) + 28 (actual) + 40_000 (stale at cap)
    assert!((or.spent_usd - 0.042028).abs() < 1e-12, "{}", or.spent_usd);
    assert!((service(&info, "keenable").spent_usd - 0.001).abs() < 1e-12);
    assert!((info.spent_usd - 0.043028).abs() < 1e-12);

    // The next capped reservation sweeps it to `abandoned` at the same
    // amount.
    let capped = spec(1_000, 0);
    proxy_service::reserve_charge(
        &pool,
        &capped,
        battle,
        user,
        "keenable",
        Uuid::new_v4(),
        1_000,
        1_000,
    )
    .await
    .unwrap();
    let (src, amt): (String, i64) =
        sqlx::query_as("SELECT source, amount_micro_usd FROM proxy_charges WHERE id = $1")
            .bind(stale)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((src.as_str(), amt), ("abandoned", 40_000));

    // A released reservation (upstream connect error) leaves no trace.
    let r =
        proxy_service::reserve_charge(&pool, &s, battle, user, "monid", Uuid::new_v4(), 4_000, 0)
            .await
            .unwrap();
    proxy_service::release_charge(&pool, r).await;
    let n: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM proxy_charges WHERE id = $1")
        .bind(r)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n.0, 0);
}

// Regression (proxy audit 2026-11-19): the dedupe key was claimed by the
// first positive cost, so a later, larger final cost for the same run
// billed 0. Now each later charge bills the increase over the max seen.
#[tokio::test]
#[ignore]
async fn dedupe_bills_the_max_seen_cost() {
    let pool = pool().await;
    let s = spec(0, 0);
    let (battle, user) = fixture(&pool).await;
    let mut charged = Vec::new();
    for cost in [1_000, 4_000, 4_000, 2_000, 6_000] {
        let c = proxy_service::reserve_charge(
            &pool,
            &s,
            battle,
            user,
            "monid",
            Uuid::new_v4(),
            4_000,
            0,
        )
        .await
        .unwrap();
        charged.push(proxy_service::settle_charge(&pool, c, &actual(cost, Some("run_grow"))).await);
    }
    assert_eq!(charged, vec![1_000, 3_000, 0, 0, 2_000]);
    let info = proxy_service::get_proxy_info_with_spec(&pool, battle, &s)
        .await
        .unwrap();
    assert!((service(&info, "monid").spent_usd - 0.006).abs() < 1e-12);
}

// Regression (proxy audit 2026-11-19): flat-rate calls got no
// reservation, so a call abandoned after it was sent upstream was never
// charged, and concurrent flat calls all passed an unlocked budget check.
#[tokio::test]
#[ignore]
async fn flat_calls_reserve_under_the_lock_and_abandoned_calls_are_charged() {
    let pool = pool().await;
    // keenable at 3_333¢/1K = 33_330µ$ (≈3.3¢) per call against a 1¢
    // budget: only the first reservation (spent 0) fits. Without the
    // lock all 8 concurrent flat calls passed the check.
    let mut s = spec(0, 0);
    s.budget_cents = 1;
    s.services.get_mut("keenable").unwrap().cost_per_1k_cents = 3_333;
    let (battle, user) = fixture(&pool).await;
    let mut handles = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        let s = s.clone();
        handles.push(tokio::spawn(async move {
            proxy_service::reserve_charge(
                &pool,
                &s,
                battle,
                user,
                "keenable",
                Uuid::new_v4(),
                33_330,
                33_330,
            )
            .await
            .ok()
        }));
    }
    let mut ids = Vec::new();
    for h in handles {
        if let Some(id) = h.await.unwrap() {
            ids.push(id);
        }
    }
    assert_eq!(ids.len(), 1, "only one flat reservation fits under 1¢");

    // The client hangs up after the request went upstream: the guard
    // settles it as `abandoned` at the reserved amount — never 0.
    let micro = proxy_service::settle_charge(
        &pool,
        ids[0],
        &krabarena_api::services::proxy_billing::abandoned_decision(33_330),
    )
    .await;
    assert_eq!(micro, 33_330);
    let (src,): (String,) = sqlx::query_as("SELECT source FROM proxy_charges WHERE id = $1")
        .bind(ids[0])
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(src, "abandoned");
    let info = proxy_service::get_proxy_info_with_spec(&pool, battle, &s)
        .await
        .unwrap();
    assert!((service(&info, "keenable").spent_usd - 0.03333).abs() < 1e-12);

    // A completed flat call settles as `flat`, and its log row is not
    // double-counted at the flat rate.
    let (b2, u2) = fixture(&pool).await;
    let s2 = spec(0, 0);
    let l = Uuid::new_v4();
    let c = proxy_service::reserve_charge(&pool, &s2, b2, u2, "keenable", l, 1_000, 1_000)
        .await
        .unwrap();
    proxy_service::settle_charge(
        &pool,
        c,
        &krabarena_api::services::proxy_billing::flat_decision(1_000),
    )
    .await;
    log(&pool, l, b2, u2, "keenable").await;
    let info = proxy_service::get_proxy_info_with_spec(&pool, b2, &s2)
        .await
        .unwrap();
    let k = service(&info, "keenable");
    assert_eq!(k.request_count, 1);
    assert!((k.spent_usd - 0.001).abs() < 1e-12, "{}", k.spent_usd);
}

#[tokio::test]
#[ignore]
async fn contended_battle_spend_row_returns_proxy_busy_instead_of_queueing() {
    let pool = pool().await;
    let (battle, user) = fixture(&pool).await;
    let s = spec(10_000, 0);

    proxy_service::reserve_charge(
        &pool,
        &s,
        battle,
        user,
        "keenable",
        Uuid::new_v4(),
        1_000,
        1_000,
    )
    .await
    .unwrap();
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM proxy_spend WHERE battle_id = $1 FOR UPDATE")
        .bind(battle)
        .execute(&mut *holder)
        .await
        .unwrap();

    let started = std::time::Instant::now();
    let result = proxy_service::reserve_charge(
        &pool,
        &s,
        battle,
        user,
        "keenable",
        Uuid::new_v4(),
        1_000,
        1_000,
    )
    .await;
    let waited = started.elapsed();
    holder.rollback().await.unwrap();

    assert!(
        matches!(result, Err(krabarena_api::error::ApiError::ProxyBusy)),
        "expected PROXY_BUSY, got {result:?}"
    );
    assert!(
        waited < std::time::Duration::from_secs(10),
        "a contended reservation must give up quickly, waited {waited:?}"
    );

    let ok = proxy_service::reserve_charge(
        &pool,
        &s,
        battle,
        user,
        "keenable",
        Uuid::new_v4(),
        1_000,
        1_000,
    )
    .await;
    assert!(
        ok.is_ok(),
        "reservation must succeed once the lock is free: {ok:?}"
    );
}

async fn spend_rows(pool: &PgPool, battle: Uuid) -> Vec<(Uuid, i64)> {
    sqlx::query_as(
        "SELECT user_id, charged_micro_usd FROM proxy_spend WHERE battle_id = $1 ORDER BY user_id",
    )
    .bind(battle)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn aggregate_rows(pool: &PgPool, battle: Uuid) -> Vec<(Uuid, i64)> {
    sqlx::query_as(
        "SELECT '00000000-0000-0000-0000-000000000000'::uuid, COALESCE(SUM(amount_micro_usd), 0)::BIGINT \
           FROM proxy_charges WHERE battle_id = $1 \
         UNION ALL \
         SELECT user_id, SUM(amount_micro_usd)::BIGINT FROM proxy_charges WHERE battle_id = $1 GROUP BY user_id \
         ORDER BY 1",
    )
    .bind(battle)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn running_totals_match_the_full_aggregate_after_every_kind_of_write() {
    let pool = pool().await;
    let (battle, user) = fixture(&pool).await;
    let other: (Uuid,) = sqlx::query_as(
        "INSERT INTO users (entity_id, display_name, email, auth_method) \
         SELECT entity_id, display_name || '-2', 'second-' || email, 'email' FROM users WHERE id = $1 \
         RETURNING id",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .unwrap();
    let other = other.0;
    let s = spec(100_000, 50_000);

    let a =
        proxy_service::reserve_charge(&pool, &s, battle, user, "monid", Uuid::new_v4(), 4_000, 0)
            .await
            .unwrap();
    proxy_service::settle_charge(&pool, a, &actual(2_500, Some("run_x"))).await;
    let b =
        proxy_service::reserve_charge(&pool, &s, battle, other, "monid", Uuid::new_v4(), 4_000, 0)
            .await
            .unwrap();
    proxy_service::settle_charge(&pool, b, &actual(3_000, Some("run_x"))).await;
    let c = proxy_service::reserve_charge(
        &pool,
        &s,
        battle,
        other,
        "keenable",
        Uuid::new_v4(),
        1_000,
        1_000,
    )
    .await
    .unwrap();
    proxy_service::release_charge(&pool, c).await;
    proxy_service::reserve_charge(
        &pool,
        &s,
        battle,
        user,
        "openrouter",
        Uuid::new_v4(),
        7_000,
        1_000,
    )
    .await
    .unwrap();

    let counters = spend_rows(&pool, battle).await;
    assert_eq!(counters, aggregate_rows(&pool, battle).await);
    assert_eq!(
        counters.iter().find(|(u, _)| u.is_nil()).unwrap().1,
        2_500 + 500 + 7_000
    );
}

#[tokio::test]
#[ignore]
async fn reserving_stays_fast_on_a_battle_with_a_large_history() {
    let pool = pool().await;
    let (battle, user) = fixture(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SET session_replication_role = replica")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO proxy_charges (battle_id, user_id, service_name, amount_micro_usd, source, settled_at) \
         SELECT $1, $2, 'keenable', 10, 'flat', now() FROM generate_series(1, 200000)",
    )
    .bind(battle)
    .bind(user)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("SET session_replication_role = DEFAULT")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    sqlx::query(
        "INSERT INTO proxy_spend (battle_id, user_id, charged_micro_usd) \
         VALUES ($1, '00000000-0000-0000-0000-000000000000', 2000000), ($1, $2, 2000000)",
    )
    .bind(battle)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let s = spec(1_000_000, 0);

    let started = std::time::Instant::now();
    for _ in 0..50 {
        proxy_service::reserve_charge(
            &pool,
            &s,
            battle,
            user,
            "keenable",
            Uuid::new_v4(),
            1_000,
            1_000,
        )
        .await
        .unwrap();
    }
    let per_call = started.elapsed() / 50;
    assert!(
        per_call < std::time::Duration::from_millis(50),
        "reserve_charge took {per_call:?} per call on a 200k-charge Battle"
    );
    assert_eq!(
        spend_rows(&pool, battle).await,
        aggregate_rows(&pool, battle).await
    );
}

#[tokio::test]
#[ignore]
async fn proxy_info_on_a_large_log_history_costs_about_one_scan() {
    let pool = pool().await;
    let (battle, user) = fixture(&pool).await;
    sqlx::query(
        "INSERT INTO proxy_logs (battle_id, user_id, service_name, method, path, response_status, response_body) \
         SELECT $1, $2, CASE WHEN g % 2 = 0 THEN 'keenable' ELSE 'openrouter' END, \
                'POST', '/search', 200, repeat('x', 400) \
         FROM generate_series(1, 120000) g",
    )
    .bind(battle)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO proxy_charges (battle_id, user_id, service_name, proxy_log_id, amount_micro_usd, source) \
         SELECT battle_id, user_id, service_name, id, 10, 'actual' FROM proxy_logs \
         WHERE battle_id = $1 AND service_name = 'openrouter'",
    )
    .bind(battle)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE proxy_logs")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE proxy_charges")
        .execute(&pool)
        .await
        .unwrap();
    let s = spec(0, 0);

    let mut info_best = std::time::Duration::MAX;
    let mut scan_best = std::time::Duration::MAX;
    let mut info = None;
    for _ in 0..3 {
        let started = std::time::Instant::now();
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM proxy_logs WHERE battle_id = $1 GROUP BY service_name LIMIT 1",
        )
        .bind(battle)
        .fetch_one(&pool)
        .await
        .unwrap();
        scan_best = scan_best.min(started.elapsed());
        let started = std::time::Instant::now();
        info = Some(
            proxy_service::get_proxy_info_with_spec(&pool, battle, &s)
                .await
                .unwrap(),
        );
        info_best = info_best.min(started.elapsed());
    }
    let info = info.unwrap();
    assert_eq!(service(&info, "keenable").request_count, 60_000);
    assert_eq!(service(&info, "openrouter").request_count, 60_000);
    assert_eq!(info.spent_cents, 6_000 + 60);
    assert!(
        info_best < scan_best * 3,
        "proxy-info took {info_best:?}; a plain scan of the same logs took {scan_best:?}"
    );
}
