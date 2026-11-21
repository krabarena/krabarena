//! DB-backed test for the usage report (`usage_stats::build_report`).
//! Opt-in via `TEST_DATABASE_URL` like the other DB tests; without it the
//! test prints SKIP and passes. Never point it at prod.
//!
//! Seeds user_logs / cli_telemetry / content rows on a random far-past
//! day (1990s) so concurrent or repeated runs never share a window, then
//! checks every headline count plus coverage and the previous-window
//! delta.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::services::ua_classifier::UaCategory;
use crate::services::usage_stats::{UsageWindow, build_report};

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for usage stats DB tests");
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

async fn seed_user(pool: &PgPool, label: &str, created_at: DateTime<Utc>) -> Uuid {
    let suffix = Uuid::new_v4();
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method)
         VALUES ($1, $2, 'email') RETURNING id",
    )
    .bind(format!("Usage {label}"))
    .bind(format!("usage-{label}-{suffix}.test"))
    .fetch_one(pool)
    .await
    .expect("insert entity");
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method, created_at)
         VALUES ($1, $2, $3, 'email', $4) RETURNING id",
    )
    .bind(entity_id)
    .bind(format!("Usage {label}"))
    .bind(format!("usage-{label}-{suffix}@example.test"))
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("insert user")
}

async fn seed_claim(
    pool: &PgPool,
    author: Uuid,
    battle: Uuid,
    predecessor: Option<Uuid>,
    at: DateTime<Utc>,
) -> (Uuid, String) {
    let suffix = Uuid::new_v4();
    let slug = format!("usage-claim-{suffix}");
    let id = sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, artifact_size,
                             created_at, updated_at, visibility, slug, topic_tags, predecessor_id)
         VALUES ($1, $2, '# t\n\n## Summary\nDisposable.', $3, $4, 1, $5, $5, 'public', $6,
                 ARRAY[]::text[], $7)
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(format!("sha256:{suffix}"))
    .bind(format!("https://example.test/{suffix}.tar.gz"))
    .bind(at)
    .bind(&slug)
    .bind(predecessor)
    .fetch_one(pool)
    .await
    .expect("insert claim");
    (id, slug)
}

#[allow(clippy::too_many_arguments)]
async fn log(
    pool: &PgPool,
    device: Uuid,
    voter: Option<Uuid>,
    event: &str,
    path: &str,
    referer: Option<&str>,
    ua: &str,
    at: DateTime<Utc>,
) {
    sqlx::query(
        "INSERT INTO user_logs (device_id, voter_id, event, path, method, referer, user_agent, created_at)
         VALUES ($1, $2, $3, $4, 'GET', $5, $6, $7)",
    )
    .bind(device)
    .bind(voter)
    .bind(event)
    .bind(path)
    .bind(referer)
    .bind(ua)
    .bind(at)
    .execute(pool)
    .await
    .expect("insert user_log");
}

async fn cli(
    pool: &PgPool,
    user: Uuid,
    args: &[&str],
    exit_code: i32,
    version: &str,
    ts: DateTime<Utc>,
) {
    let raw = serde_json::json!({
        "command": args[0],
        "args": args,
        "exit_code": exit_code,
    });
    sqlx::query(
        "INSERT INTO cli_telemetry (user_id, kind, agent, ts, session_id, krab_version, os, raw_json, received_at)
         VALUES ($1, 'cli_command', 'unknown', $2, $3, $4, 'linux/x86_64', $5, $2 + interval '1 hour')",
    )
    .bind(user)
    .bind(ts)
    .bind(Uuid::new_v4().to_string())
    .bind(version)
    .bind(raw)
    .execute(pool)
    .await
    .expect("insert cli_telemetry");
}

#[tokio::test]
async fn usage_report_counts_seeded_rows() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let offset = (Uuid::new_v4().as_u128() % 3000) as i64;
    let day = NaiveDate::from_ymd_opt(1991, 1, 1).unwrap() + Duration::days(offset);
    let window = UsageWindow {
        first_day: day,
        last_day: day,
        days: 1,
    };
    let t = |h: i64| window.start() + Duration::hours(h);
    let prev_t = window.start() - Duration::hours(12);

    // --- content ---
    let author = seed_user(&pool, "author", t(1)).await;
    let old_cli_user = seed_user(&pool, "cli-old", Utc::now()).await;
    let battle_slug = format!("usage-battle-{}", Uuid::new_v4());
    let battle: Uuid = sqlx::query_scalar(
        "INSERT INTO battles (author_id, title, description, domain_tags, status, slug, created_at, updated_at)
         VALUES ($1, 'Usage battle', 'Disposable', ARRAY['test'], 'open', $2, $3, $3) RETURNING id",
    )
    .bind(author)
    .bind(&battle_slug)
    .bind(t(1))
    .fetch_one(&pool)
    .await
    .expect("insert battle");
    let (live_claim, live_slug) = seed_claim(&pool, author, battle, None, t(2)).await;
    let (_old_claim, old_slug) = seed_claim(&pool, author, battle, None, t(2)).await;
    let (_new_claim, new_slug) = seed_claim(&pool, author, battle, Some(_old_claim), t(3)).await;
    sqlx::query(
        "INSERT INTO comments (claim_id, author_id, body, created_at) VALUES ($1, $2, 'hi', $3)",
    )
    .bind(live_claim)
    .bind(author)
    .bind(t(4))
    .execute(&pool)
    .await
    .expect("insert comment");
    let voter: Uuid = sqlx::query_scalar(
        "INSERT INTO voters (provider, provider_id, display_name, created_at)
         VALUES ('email', $1, 'usage-test', $2) RETURNING id",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(t(5))
    .fetch_one(&pool)
    .await
    .expect("insert voter");

    // --- crawlers ---
    let tag = Uuid::new_v4().simple().to_string();
    let keenable =
        format!("Mozilla/5.0 (compatible; KeenableBot/1.0; +https://keenable.ai/bot) t{tag}");
    let sleepbot = format!("Mozilla/5.0 (compatible; SleepBot/1.0; +http://sleepbot.com/) t{tag}");
    let gbot = format!(
        "Mozilla/5.0 (Linux; Android 6.0.1; Nexus 5X Build/MMB29P) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0 Mobile Safari/537.36 (compatible; Googlebot/2.1; +http://www.google.com/bot.html) t{tag}"
    );
    for path in [
        format!("/claims/{live_slug}"),
        format!("/claims/{live_claim}"),
        format!("/claims/{old_slug}"), // superseded: page hit, not coverage
        format!("/battles/{battle_slug}?tab=claims"),
        "/health".to_string(), // excluded everywhere
    ] {
        log(
            &pool,
            Uuid::new_v4(),
            None,
            "visit",
            &path,
            None,
            &keenable,
            t(6),
        )
        .await;
    }
    log(
        &pool,
        Uuid::new_v4(),
        None,
        "visit",
        &format!("/claims/{new_slug}"),
        None,
        &sleepbot,
        t(6),
    )
    .await;
    log(&pool, Uuid::new_v4(), None, "visit", "/", None, &gbot, t(7)).await;
    // Previous window: one KeenableBot hit.
    log(
        &pool,
        Uuid::new_v4(),
        None,
        "visit",
        "/",
        None,
        &keenable,
        prev_t,
    )
    .await;

    // --- humans ---
    let browser = format!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36 t{tag}"
    );
    let (d1, d2) = (Uuid::new_v4(), Uuid::new_v4());
    log(
        &pool,
        d1,
        None,
        "visit",
        "/",
        Some("https://news.ycombinator.com/item?id=1"),
        &browser,
        t(8),
    )
    .await;
    log(
        &pool,
        d1,
        None,
        "visit",
        "/battles/x?y=1",
        Some("https://krabarena.com/"),
        &browser,
        t(9),
    )
    .await;
    log(&pool, d1, None, "visit", "/.env", None, &browser, t(9)).await; // probe: not a page
    log(
        &pool,
        d2,
        Some(voter),
        "visit",
        "/battles/x",
        None,
        &browser,
        t(10),
    )
    .await;
    log(
        &pool,
        d2,
        Some(voter),
        "login",
        "/auth/voter/github/callback",
        None,
        &browser,
        t(10),
    )
    .await;
    log(
        &pool,
        d2,
        Some(voter),
        "vote",
        "/v1/votes",
        None,
        &browser,
        t(11),
    )
    .await;

    // --- CLI ---
    cli(
        &pool,
        author,
        &["claim", "get", "slug-a", "--format", "json"],
        0,
        "0.43.7",
        t(12),
    )
    .await;
    cli(
        &pool,
        author,
        &["claim", "get", "slug-b"],
        1,
        "0.43.7",
        t(13),
    )
    .await;
    cli(&pool, old_cli_user, &["battle", "list"], 0, "0.43.6", t(14)).await;
    cli(
        &pool,
        old_cli_user,
        &["battle", "list"],
        0,
        "0.43.6",
        window.start() - Duration::days(30),
    )
    .await;

    let now = window.end() + Duration::days(1);
    let r = build_report(&pool, window, "0.43.7", now)
        .await
        .expect("report");

    // Crawlers: 5 KeenableBot (incl. SleepBot) + 1 Googlebot; /health excluded.
    assert_eq!(r.crawlers.total, 6, "{:#?}", r.crawlers);
    let kb = r
        .crawlers
        .by_family
        .iter()
        .find(|f| f.family == "KeenableBot")
        .expect("KeenableBot");
    assert_eq!(kb.category, UaCategory::AiSearchApi);
    assert_eq!(kb.hits, 5);
    assert_eq!(kb.claim_pages, 4);
    assert_eq!(kb.battle_pages, 1);
    // live_claim (by slug AND uuid = 1) + new version via SleepBot; the
    // superseded old version doesn't count.
    assert_eq!(kb.coverage.claims, 2);
    assert_eq!(kb.coverage.battles, 1);
    let gb = r
        .crawlers
        .by_family
        .iter()
        .find(|f| f.family == "Googlebot Smartphone")
        .expect("gbot");
    assert_eq!((gb.hits, gb.category), (1, UaCategory::SearchEngine));
    let d = &r.previous.deltas["crawlers.total"];
    assert_eq!((d.current, d.previous), (6, 1));
    assert_eq!(r.previous.deltas["crawlers.ai_search_api"].previous, 1);

    // Humans.
    assert_eq!(r.humans.visits, 3, "{:#?}", r.humans);
    assert_eq!(r.humans.devices, 2);
    assert_eq!(r.humans.engaged_devices, 2);
    assert_eq!(r.humans.new_devices, 2);
    assert_eq!(r.humans.new_engaged_devices, 2);
    assert_eq!(r.humans.voters_active, 1);
    assert_eq!(r.humans.logins, 1);
    assert_eq!(r.humans.votes, 1);
    assert_eq!(r.humans.top_pages[0].path, "/battles/x");
    assert_eq!(r.humans.top_pages[0].visits, 2);
    assert_eq!(r.humans.top_referrers.len(), 1, "own host excluded");
    assert_eq!(r.humans.top_referrers[0].host, "news.ycombinator.com");

    // CLI.
    assert_eq!(r.cli.users, 2, "{:#?}", r.cli);
    assert_eq!(r.cli.new_users, 1);
    assert_eq!(r.cli.invocations, 3);
    assert_eq!(r.cli.failed, 1);
    assert_eq!(r.cli.top_commands[0].command, "claim get");
    assert_eq!(r.cli.top_commands[0].invocations, 2);
    assert_eq!(r.cli.users_on_latest, 1);
    assert_eq!(r.cli.versions[0].version, "0.43.7");

    // Content.
    assert_eq!(r.content.battles, 1);
    assert_eq!(r.content.claims, 2);
    assert_eq!(r.content.claim_edits, 1);
    assert_eq!(r.content.comments, 1);
    assert_eq!(r.content.signups.voters, 1);
    assert_eq!(r.content.signups.users, 1);
    assert!(
        r.summary.contains("CRAWLERS  6 hits (+500%)"),
        "{}",
        r.summary
    );
}
