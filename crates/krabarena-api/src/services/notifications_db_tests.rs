//! DB-backed tests for the notification feed (`activity_service::
//! user_notifications`) and instant notification emails
//! (`notification_email::deliver` + the service trigger sites).
//!
//! Opt-in like the Discord-monitor DB tests: they run only when
//! `TEST_DATABASE_URL` points at a disposable Postgres (they apply the
//! migrations and insert rows). Without it every test prints SKIP and
//! passes, so CI's unit-only run stays fast. Never point this at prod.
//!
//! Each test seeds its own users / battles / claims with timestamps in
//! a random far-future window so runs don't see each other's rows.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::services::activity_service::{self, NotificationItem};
use crate::services::notification_email::{
    self, DeliveryOutcome, NotificationEmailConfig, NotificationEvent, NotificationMailer,
    NotifierRuntime, OutgoingEmail, SkipReason, ThrottleReason,
};

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("SKIP: set TEST_DATABASE_URL for notification DB tests");
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

fn base_time() -> DateTime<Utc> {
    let offset = (Uuid::new_v4().as_u128() % 1_000_000) as i64;
    Utc::now() + Duration::days(365 * 30) + Duration::seconds(offset * 60)
}

async fn seed_user(pool: &PgPool, label: &str) -> Uuid {
    let suffix = Uuid::new_v4();
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method)
         VALUES ($1, $2, 'email') RETURNING id",
    )
    .bind(format!("Notif {label}"))
    .bind(format!("notif-{label}-{suffix}.test"))
    .fetch_one(pool)
    .await
    .expect("insert entity");
    sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method)
         VALUES ($1, $2, $3, 'email') RETURNING id",
    )
    .bind(entity_id)
    .bind(format!("Notif {label}"))
    .bind(format!("notif-{label}-{suffix}@example.test"))
    .fetch_one(pool)
    .await
    .expect("insert user")
}

async fn seed_battle(pool: &PgPool, author: Uuid, at: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO battles (author_id, title, description, domain_tags, status, slug, created_at, updated_at)
         VALUES ($1, 'Notif battle', 'Disposable', ARRAY['test'], 'open', $2, $3, $3)
         RETURNING id",
    )
    .bind(author)
    .bind(format!("notif-battle-{}", Uuid::new_v4()))
    .bind(at)
    .fetch_one(pool)
    .await
    .expect("insert battle")
}

async fn seed_review(pool: &PgPool, user: Uuid, battle: Option<Uuid>, at: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO claim_reviews (battle_id, user_id, claim, artifact_hash, artifact_size, model, verdict, summary, created_at)
         VALUES ($1, $2, '# t', 'sha256:x', 1, 'test', 'warn', 'Looks fine, add more runs.', $3)
         RETURNING id",
    )
    .bind(battle)
    .bind(user)
    .bind(at)
    .fetch_one(pool)
    .await
    .expect("insert review")
}

async fn seed_claim(
    pool: &PgPool,
    author: Uuid,
    battle: Option<Uuid>,
    review: Option<Uuid>,
    title: &str,
    at: DateTime<Utc>,
) -> Uuid {
    let suffix = Uuid::new_v4();
    sqlx::query_scalar(
        "INSERT INTO claims (battle_id, author_id, claim, artifact_hash, artifact_url, artifact_size,
                             review_id, created_at, updated_at, visibility, slug, topic, topic_tags)
         VALUES ($1, $2, $3, $4, $5, 1, $6, $7, $7, 'public', $8, $9, ARRAY[]::text[])
         RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(format!("# {title}\n\n## Summary\nDisposable."))
    .bind(format!("sha256:{suffix}"))
    .bind(format!("https://example.test/{suffix}.tar.gz"))
    .bind(review)
    .bind(at)
    .bind(format!("notif-claim-{suffix}"))
    .bind(battle.is_none().then(|| "Notif topic".to_string()))
    .fetch_one(pool)
    .await
    .expect("insert claim")
}

async fn seed_claim_comment(pool: &PgPool, claim: Uuid, author: Uuid, at: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO comments (claim_id, author_id, body, created_at) VALUES ($1, $2, 'nice claim', $3) RETURNING id",
    )
    .bind(claim)
    .bind(author)
    .bind(at)
    .fetch_one(pool)
    .await
    .expect("insert claim comment")
}

async fn seed_battle_comment(pool: &PgPool, battle: Uuid, author: Uuid, at: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO comments (battle_id, author_id, body, created_at) VALUES ($1, $2, 'see HN thread', $3) RETURNING id",
    )
    .bind(battle)
    .bind(author)
    .bind(at)
    .fetch_one(pool)
    .await
    .expect("insert battle comment")
}

async fn seed_verification(pool: &PgPool, claim: Uuid, author: Uuid, at: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO verifications (claim_id, author_id, verdict, environment, output_hash, duration_ms, created_at)
         VALUES ($1, $2, 'confirmed', 'linux x86_64', 'sha256:o', 42, $3) RETURNING id",
    )
    .bind(claim)
    .bind(author)
    .bind(at)
    .fetch_one(pool)
    .await
    .expect("insert verification")
}

async fn seed_summary(pool: &PgPool, battle: Uuid, at: DateTime<Utc>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO battle_summary_history (battle_id, summary, og_summary, created_at)
         VALUES ($1, 'Long summary', 'Rust leads after 2 claims', $2) RETURNING id",
    )
    .bind(battle)
    .bind(at)
    .fetch_one(pool)
    .await
    .expect("insert summary")
}

/// Scenario: A authors a Battle and a Claim (with a KrabReviewer
/// review) on it; B posts a Claim to A's Battle, comments on the Battle
/// and on A's Claim, and verifies A's Claim; the Battle's summary is
/// refreshed; A comments on their own Battle (must not notify A).
struct Scenario {
    a: Uuid,
    b: Uuid,
    battle: Uuid,
    claim_a: Uuid,
    expected: Vec<(&'static str, Uuid)>,
}

async fn seed_scenario(pool: &PgPool) -> Scenario {
    let t0 = base_time();
    let s = |n: i64| t0 + Duration::seconds(n);
    let a = seed_user(pool, "a").await;
    let b = seed_user(pool, "b").await;
    let battle = seed_battle(pool, a, s(0)).await;
    let review = seed_review(pool, a, Some(battle), s(1)).await;
    let claim_a = seed_claim(pool, a, Some(battle), Some(review), "A's claim", s(2)).await;
    let claim_b = seed_claim(pool, b, Some(battle), None, "B's claim", s(3)).await;
    let battle_comment = seed_battle_comment(pool, battle, b, s(4)).await;
    let claim_comment = seed_claim_comment(pool, claim_a, b, s(5)).await;
    let verification = seed_verification(pool, claim_a, b, s(6)).await;
    let summary = seed_summary(pool, battle, s(7)).await;
    // Own activity — must never notify A.
    seed_battle_comment(pool, battle, a, s(8)).await;
    Scenario {
        a,
        b,
        battle,
        claim_a,
        // Newest first.
        expected: vec![
            ("battle_summary", summary),
            ("response", verification),
            ("comment", claim_comment),
            ("battle_comment", battle_comment),
            ("battle_claim", claim_b),
            ("claim_review", review),
        ],
    }
}

fn kinds_ids(items: &[NotificationItem]) -> Vec<(String, Uuid)> {
    items.iter().map(|i| (i.kind.clone(), i.id)).collect()
}

#[tokio::test]
async fn feed_returns_every_new_kind_for_a_battle_author() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;

    let (items, page) = activity_service::user_notifications(&pool, sc.a, None, 50)
        .await
        .expect("feed");
    let got = kinds_ids(&items);
    let want: Vec<(String, Uuid)> = sc
        .expected
        .iter()
        .map(|(k, id)| (k.to_string(), *id))
        .collect();
    assert_eq!(got, want);
    assert!(!page.has_more);

    // Shape checks per kind.
    let by_kind = |k: &str| items.iter().find(|i| i.kind == k).unwrap();
    let bc = by_kind("battle_claim");
    assert_eq!(bc.battle_id, Some(sc.battle));
    assert_eq!(bc.author_id, Some(sc.b));
    assert_eq!(bc.title.as_deref(), Some("B's claim"));
    assert_eq!(bc.label, "New claim");
    assert!(bc.author_name.as_deref().unwrap().starts_with("Notif b"));

    let bcm = by_kind("battle_comment");
    assert_eq!(bcm.claim_id, None);
    assert_eq!(bcm.title.as_deref(), Some("Notif battle"));
    assert!(bcm.url.contains("/battles/notif-battle-"));
    assert_eq!(bcm.preview, "see HN thread");

    let rv = by_kind("claim_review");
    assert_eq!(rv.claim_id, Some(sc.claim_a));
    assert_eq!(rv.author_id, None);
    assert_eq!(rv.verdict.as_deref(), Some("warn"));
    assert_eq!(rv.summary.as_deref(), Some("Looks fine, add more runs."));
    assert_eq!(rv.label, "Review: warn");

    let bs = by_kind("battle_summary");
    assert_eq!(bs.summary.as_deref(), Some("Rust leads after 2 claims"));
    assert_eq!(bs.author_id, None);

    // Legacy kinds keep their fields.
    let resp = by_kind("response");
    assert_eq!(resp.claim_id, Some(sc.claim_a));
    assert_eq!(resp.verdict.as_deref(), Some("confirmed"));
    assert_eq!(resp.label, "Verified");
    assert_eq!(resp.title.as_deref(), Some("A's claim"));
}

#[tokio::test]
async fn feed_excludes_own_activity_and_other_users_battles() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;
    // B sees: nothing from A's battle-level kinds (B isn't the author);
    // B's own claim/comment/verification never notify B.
    let (items, _) = activity_service::user_notifications(&pool, sc.b, None, 50)
        .await
        .expect("feed");
    for i in &items {
        assert_ne!(i.author_id, Some(sc.b), "own activity leaked: {i:?}");
        assert!(
            !matches!(
                i.kind.as_str(),
                "battle_claim" | "battle_comment" | "battle_summary"
            ) || i.battle_id != Some(sc.battle),
            "B must not get battle-author kinds for A's battle: {i:?}"
        );
    }
}

#[tokio::test]
async fn feed_pagination_is_stable_across_kinds() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;

    let mut seen: Vec<(String, Uuid)> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let (items, page) = activity_service::user_notifications(&pool, sc.a, cursor.as_deref(), 2)
            .await
            .expect("page");
        assert!(items.len() <= 2);
        seen.extend(kinds_ids(&items));
        pages += 1;
        if !page.has_more {
            break;
        }
        cursor = page.next_cursor;
        assert!(pages < 10, "pagination did not terminate");
    }
    let want: Vec<(String, Uuid)> = sc
        .expected
        .iter()
        .map(|(k, id)| (k.to_string(), *id))
        .collect();
    assert_eq!(seen, want, "pages must concatenate to the single-page feed");
    assert_eq!(pages, 3);
}

// ---------------------------------------------------------------------------
// Instant emails
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeMailer {
    sent: Mutex<Vec<OutgoingEmail>>,
}

impl NotificationMailer for FakeMailer {
    fn send<'a>(
        &'a self,
        email: &'a OutgoingEmail,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.sent.lock().unwrap().push(email.clone());
            Ok(())
        })
    }
}

fn runtime(mailer: Arc<FakeMailer>) -> NotifierRuntime {
    NotifierRuntime {
        mailer,
        config: NotificationEmailConfig {
            enabled: true,
            allowlist: vec![],
        },
        public_base_url: "https://krabarena.test".into(),
        link_secret: "test-secret".into(),
    }
}

async fn user_email(pool: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn email_goes_to_claim_author_then_same_target_is_throttled() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;
    let mailer = Arc::new(FakeMailer::default());
    let rt = runtime(mailer.clone());

    let c1 = seed_claim_comment(&pool, sc.claim_a, sc.b, Utc::now()).await;
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::ClaimComment { comment_id: c1 },
    )
    .await
    .unwrap();
    let a_email = user_email(&pool, sc.a).await;
    assert_eq!(
        out,
        DeliveryOutcome::Sent {
            to: a_email.clone()
        }
    );
    {
        let sent = mailer.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, a_email);
        assert!(sent[0].subject.contains("commented on your claim"));
        assert!(
            sent[0]
                .text
                .contains("https://krabarena.test/claims/notif-claim-")
        );
        assert!(sent[0].unsubscribe_url.contains("/email/unsub/instant?u="));
    }

    // A second comment on the same claim within 10 minutes → throttled.
    let c2 = seed_claim_comment(&pool, sc.claim_a, sc.b, Utc::now()).await;
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::ClaimComment { comment_id: c2 },
    )
    .await
    .unwrap();
    assert_eq!(out, DeliveryOutcome::Throttled(ThrottleReason::SameTarget));
    assert_eq!(mailer.sent.lock().unwrap().len(), 1);

    // A different kind on the same claim is a different throttle key.
    let v = seed_verification(&pool, sc.claim_a, sc.b, Utc::now()).await;
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::Verification { verification_id: v },
    )
    .await
    .unwrap();
    assert!(matches!(out, DeliveryOutcome::Sent { .. }), "{out:?}");

    let logged: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notification_email_log WHERE user_id = $1")
            .bind(sc.a)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(logged, 3, "sent + throttled + sent are all audited");
}

#[tokio::test]
async fn email_respects_actor_bot_and_opt_outs() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;
    let mailer = Arc::new(FakeMailer::default());
    let rt = runtime(mailer.clone());

    // Actor: A comments on A's own claim.
    let own = seed_claim_comment(&pool, sc.claim_a, sc.a, Utc::now()).await;
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::ClaimComment { comment_id: own },
    )
    .await
    .unwrap();
    assert_eq!(out, DeliveryOutcome::Skipped(SkipReason::Actor));

    // Instant emails off.
    sqlx::query("INSERT INTO user_notification_prefs (user_id, instant_email) VALUES ($1, FALSE)")
        .bind(sc.a)
        .execute(&pool)
        .await
        .unwrap();
    let bc = seed_battle_comment(&pool, sc.battle, sc.b, Utc::now()).await;
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::BattleComment { comment_id: bc },
    )
    .await
    .unwrap();
    assert_eq!(out, DeliveryOutcome::Skipped(SkipReason::InstantOff));

    // Digest off (instant back on) also suppresses.
    sqlx::query("UPDATE user_notification_prefs SET instant_email = TRUE WHERE user_id = $1")
        .bind(sc.a)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO user_email_preferences (user_id, digest_frequency) VALUES ($1, 'off')",
    )
    .bind(sc.a)
    .execute(&pool)
    .await
    .unwrap();
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::BattleComment { comment_id: bc },
    )
    .await
    .unwrap();
    assert_eq!(out, DeliveryOutcome::Skipped(SkipReason::DigestOff));

    // Bot recipient: B is made a bot; A comments on B's claim.
    sqlx::query("UPDATE users SET kind = 'bot' WHERE id = $1")
        .bind(sc.b)
        .execute(&pool)
        .await
        .unwrap();
    let claim_b = seed_claim(&pool, sc.b, None, None, "bot claim", Utc::now()).await;
    let cm = seed_claim_comment(&pool, claim_b, sc.a, Utc::now()).await;
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::ClaimComment { comment_id: cm },
    )
    .await
    .unwrap();
    assert_eq!(out, DeliveryOutcome::Skipped(SkipReason::Bot));

    assert!(
        mailer.sent.lock().unwrap().is_empty(),
        "nothing may be sent"
    );
}

#[tokio::test]
async fn email_hourly_cap() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;
    let mailer = Arc::new(FakeMailer::default());
    let rt = runtime(mailer.clone());

    for _ in 0..notification_email::HOURLY_CAP {
        sqlx::query(
            "INSERT INTO notification_email_log (user_id, kind, target_id, event_id, status)
             VALUES ($1, 'comment', $2, $2, 'sent')",
        )
        .bind(sc.a)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();
    }
    let bc = seed_battle_comment(&pool, sc.battle, sc.b, Utc::now()).await;
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::BattleComment { comment_id: bc },
    )
    .await
    .unwrap();
    assert_eq!(out, DeliveryOutcome::Throttled(ThrottleReason::HourlyCap));
    assert!(mailer.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn battle_claim_and_review_emails_route_to_the_right_people() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;
    let mailer = Arc::new(FakeMailer::default());
    let rt = runtime(mailer.clone());

    let review = seed_review(&pool, sc.b, Some(sc.battle), Utc::now()).await;
    let claim_b = seed_claim(
        &pool,
        sc.b,
        Some(sc.battle),
        Some(review),
        "Fresh",
        Utc::now(),
    )
    .await;

    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::BattleClaim { claim_id: claim_b },
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        DeliveryOutcome::Sent {
            to: user_email(&pool, sc.a).await
        }
    );

    // Review email goes to the claim author (B) — not skipped as actor,
    // KrabReviewer is the actor.
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::ClaimReview { claim_id: claim_b },
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        DeliveryOutcome::Sent {
            to: user_email(&pool, sc.b).await
        }
    );
    let sent = mailer.sent.lock().unwrap();
    assert!(sent[0].subject.contains("New claim on your battle"));
    assert!(sent[1].subject.contains("KrabReviewer: warn"));
}

// ---------------------------------------------------------------------------
// Trigger paths enqueue the right event
// ---------------------------------------------------------------------------

fn lock() -> &'static tokio::sync::Mutex<()> {
    static L: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    L.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[tokio::test]
async fn comment_services_enqueue_notification_emails() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let _g = lock().lock().await;
    let sc = seed_scenario(&pool).await;
    let req = || crate::models::comment::CreateCommentRequest {
        body: "trigger test".into(),
        agent_backend: None,
        agent_model: None,
        parent_id: None,
    };

    let c = crate::services::comment_service::create_comment(&pool, sc.claim_a, sc.b, req())
        .await
        .expect("create comment");
    assert!(
        notification_email::recorded_events()
            .contains(&NotificationEvent::ClaimComment { comment_id: c.id })
    );

    let bc = crate::services::comment_service::create_battle_comment(&pool, sc.battle, sc.b, req())
        .await
        .expect("create battle comment");
    assert!(
        notification_email::recorded_events()
            .contains(&NotificationEvent::BattleComment { comment_id: bc.id })
    );
}

#[tokio::test]
async fn verification_service_enqueues_notification_email() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;
    // No API key → KrabReviewer synth-passes, no network.
    let gemini = crate::services::gemini_service::GeminiClient::new(None);
    let out = crate::services::verification_service::create_verification(
        &pool,
        &gemini,
        sc.claim_a,
        sc.b,
        crate::models::verification::CreateResponseRequest {
            verdict: "confirmed".into(),
            environment: Some("linux".into()),
            output_hash: Some("sha256:abc".into()),
            duration_ms: Some(10),
            failure_reason: None,
            conditions: None,
            reproduction_steps: None,
            agent_backend: None,
            agent_model: None,
        },
    )
    .await
    .expect("create verification")
    .expect("not blocked");
    assert!(
        notification_email::recorded_events().contains(&NotificationEvent::Verification {
            verification_id: out.verification.id
        })
    );
}

// ---------------------------------------------------------------------------
// Replies + @mentions (since 0.43.6)
// ---------------------------------------------------------------------------

/// A user whose display name is unique, so `@handle` resolves to them
/// (the shared `seed_user` names repeat across runs → ambiguous).
async fn seed_named_user(pool: &PgPool, prefix: &str) -> (Uuid, String) {
    let tag = Uuid::new_v4().simple().to_string()[..10].to_string();
    let display = format!("{prefix} {tag}");
    let handle = crate::services::mentions::handle_for_display_name(&display);
    let suffix = Uuid::new_v4();
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO entities (name, domain, verification_method)
         VALUES ($1, $2, 'email') RETURNING id",
    )
    .bind(format!("Mention {tag}"))
    .bind(format!("mention-{suffix}.test"))
    .fetch_one(pool)
    .await
    .expect("insert entity");
    let id = sqlx::query_scalar(
        "INSERT INTO users (entity_id, display_name, email, auth_method)
         VALUES ($1, $2, $3, 'email') RETURNING id",
    )
    .bind(entity_id)
    .bind(&display)
    .bind(format!("mention-{suffix}@example.test"))
    .fetch_one(pool)
    .await
    .expect("insert user");
    (id, handle)
}

fn comment_req(body: &str, parent: Option<Uuid>) -> crate::models::comment::CreateCommentRequest {
    crate::models::comment::CreateCommentRequest {
        body: body.into(),
        agent_backend: None,
        agent_model: None,
        parent_id: parent,
    }
}

struct ThreadScenario {
    a: Uuid,
    b: Uuid,
    c: Uuid,
    claim: Uuid,
    battle: Uuid,
    /// C's top-level comment on A's claim.
    c1: Uuid,
    /// B's reply to c1, mentioning @A and @C.
    c2: Uuid,
    /// B's top-level comment mentioning @C (and @B — self).
    c3: Uuid,
    /// A's comment on A's battle.
    bc1: Uuid,
    /// B's reply to bc1.
    bc2: Uuid,
}

async fn seed_threads(pool: &PgPool) -> ThreadScenario {
    use crate::services::comment_service::{create_battle_comment, create_comment};
    let (a, ha) = seed_named_user(pool, "Alice").await;
    let (b, hb) = seed_named_user(pool, "Bob").await;
    let (c, hc) = seed_named_user(pool, "Carol").await;
    let t0 = base_time();
    let battle = seed_battle(pool, a, t0).await;
    let claim = seed_claim(pool, a, Some(battle), None, "Threads claim", t0).await;

    let c1 = create_comment(pool, claim, c, comment_req("first!", None))
        .await
        .expect("c1")
        .id;
    let c2 = create_comment(
        pool,
        claim,
        b,
        comment_req(
            &format!("@{hc} agreed, cc @{}.", ha.to_uppercase()),
            Some(c1),
        ),
    )
    .await
    .expect("c2");
    assert_eq!(c2.parent_id, Some(c1));
    let mut got: Vec<Uuid> = c2.mentions.iter().map(|m| m.user_id).collect();
    got.sort();
    let mut want = vec![a, c];
    want.sort();
    assert_eq!(got, want, "both @handles resolve (case-insensitive)");
    let c3 = create_comment(
        pool,
        claim,
        b,
        comment_req(&format!("ping @{hc} — and me @{hb}"), None),
    )
    .await
    .expect("c3")
    .id;
    let bc1 = create_battle_comment(pool, battle, a, comment_req("battle note", None))
        .await
        .expect("bc1")
        .id;
    let bc2 = create_battle_comment(pool, battle, b, comment_req("re: note", Some(bc1)))
        .await
        .expect("bc2")
        .id;
    ThreadScenario {
        a,
        b,
        c,
        claim,
        battle,
        c1,
        c2: c2.id,
        c3,
        bc1,
        bc2,
    }
}

#[tokio::test]
async fn reply_and_mention_feed_kinds_are_deduped() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_threads(&pool).await;

    // C: reply (c2 — also mentions C, but reply wins) + mention (c3).
    let (items, _) = activity_service::user_notifications(&pool, sc.c, None, 50)
        .await
        .expect("feed c");
    assert_eq!(
        kinds_ids(&items),
        vec![("mention".to_string(), sc.c3), ("reply".to_string(), sc.c2)]
    );
    let reply = &items[1];
    assert_eq!(reply.label, "Reply");
    assert_eq!(reply.claim_id, Some(sc.claim));
    assert_eq!(reply.battle_id, Some(sc.battle));
    assert_eq!(reply.title.as_deref(), Some("Threads claim"));
    assert!(reply.url.contains("/claims/notif-claim-"));
    assert!(reply.preview.contains("agreed"));
    assert_eq!(reply.author_id, Some(sc.b));
    assert_eq!(items[0].label, "Mention");

    // A (claim + battle author): battle reply wins over battle_comment;
    // c2 mentions A → `mention`, not `comment`; c1 / c3 stay `comment`.
    let (items, _) = activity_service::user_notifications(&pool, sc.a, None, 50)
        .await
        .expect("feed a");
    assert_eq!(
        kinds_ids(&items),
        vec![
            ("reply".to_string(), sc.bc2),
            ("comment".to_string(), sc.c3),
            ("mention".to_string(), sc.c2),
            ("comment".to_string(), sc.c1),
        ]
    );
    let battle_reply = &items[0];
    assert_eq!(battle_reply.claim_id, None);
    assert_eq!(battle_reply.title.as_deref(), Some("Notif battle"));
    assert!(battle_reply.url.contains("/battles/notif-battle-"));
    assert!(!items.iter().any(|i| i.id == sc.bc1), "own comment leaked");

    // B: self-mention in c3 and own replies never notify B.
    let (items, _) = activity_service::user_notifications(&pool, sc.b, None, 50)
        .await
        .expect("feed b");
    assert!(items.is_empty(), "B got {:?}", kinds_ids(&items));

    // Pagination across the new legs concatenates to the single page.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let (page_items, page) =
            activity_service::user_notifications(&pool, sc.a, cursor.as_deref(), 1)
                .await
                .expect("page");
        seen.extend(kinds_ids(&page_items));
        if !page.has_more {
            break;
        }
        cursor = page.next_cursor;
    }
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[0], ("reply".to_string(), sc.bc2));
    assert_eq!(seen[3], ("comment".to_string(), sc.c1));

    // List endpoints expose parent_id + mentions.
    let (list, _) = crate::services::comment_service::list_comments(&pool, sc.claim, None, 50)
        .await
        .expect("list");
    let c2 = list.iter().find(|c| c.id == sc.c2).unwrap();
    assert_eq!(c2.parent_id, Some(sc.c1));
    assert_eq!(c2.mentions.len(), 2);
    let c1 = list.iter().find(|c| c.id == sc.c1).unwrap();
    assert_eq!(c1.parent_id, None);
    let (blist, _) =
        crate::services::comment_service::list_battle_comments(&pool, sc.battle, None, 50)
            .await
            .expect("battle list");
    assert_eq!(
        blist.iter().find(|c| c.id == sc.bc2).unwrap().parent_id,
        Some(sc.bc1)
    );
}

#[tokio::test]
async fn reply_parent_must_be_on_the_same_target() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_threads(&pool).await;
    use crate::error::ApiError;
    use crate::services::comment_service::{create_battle_comment, create_comment};

    // Parent on another claim.
    let other_claim = seed_claim(&pool, sc.a, None, None, "Other", base_time()).await;
    let err = create_comment(&pool, other_claim, sc.b, comment_req("x", Some(sc.c1)))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");

    // Claim comment as the parent of a battle comment, and vice versa.
    let err = create_battle_comment(&pool, sc.battle, sc.b, comment_req("x", Some(sc.c1)))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");
    let err = create_comment(&pool, sc.claim, sc.b, comment_req("x", Some(sc.bc1)))
        .await
        .unwrap_err();
    assert!(matches!(err, ApiError::ValidationError(_)), "{err:?}");

    // Unknown parent.
    let err = create_comment(
        &pool,
        sc.claim,
        sc.b,
        comment_req("x", Some(Uuid::new_v4())),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ApiError::NotFound(_)), "{err:?}");

    // Nothing was inserted by the failed attempts.
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comments WHERE claim_id = $1")
        .bind(other_claim)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn mentions_skip_unknown_ambiguous_and_code() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_threads(&pool).await;
    // Two users sharing a display name → ambiguous handle.
    let (twin1, twin_handle) = seed_named_user(&pool, "Twin").await;
    let twin_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(twin1)
        .fetch_one(&pool)
        .await
        .unwrap();
    let (twin2, _) = seed_named_user(&pool, "Other").await;
    sqlx::query("UPDATE users SET display_name = $2 WHERE id = $1")
        .bind(twin2)
        .bind(&twin_name)
        .execute(&pool)
        .await
        .unwrap();

    let body = format!(
        "@{twin_handle} @nobody-{} `@{twin_handle}` mail x@y.com and @{twin2}",
        Uuid::new_v4().simple()
    );
    let out = crate::services::comment_service::create_comment(
        &pool,
        sc.claim,
        sc.b,
        comment_req(&body, None),
    )
    .await
    .expect("comment");
    // Only the UUID handle resolves.
    assert_eq!(
        out.mentions.iter().map(|m| m.user_id).collect::<Vec<_>>(),
        vec![twin2]
    );
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM comment_mentions WHERE comment_id = $1")
            .bind(out.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, 1);
}

#[tokio::test]
async fn reply_and_mention_emails_route_once_per_recipient() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_threads(&pool).await;

    // Trigger routing recorded by `emit` during `seed_threads`.
    let ev = notification_email::recorded_events();
    assert!(ev.contains(&NotificationEvent::CommentReply { comment_id: sc.c2 }));
    assert!(ev.contains(&NotificationEvent::CommentMention {
        comment_id: sc.c2,
        user_id: sc.a
    }));
    // C is the replied-to author → no separate mention event for C.
    assert!(!ev.contains(&NotificationEvent::CommentMention {
        comment_id: sc.c2,
        user_id: sc.c
    }));
    // A got a mention for c2 → no plain `comment` event for c2.
    assert!(!ev.contains(&NotificationEvent::ClaimComment { comment_id: sc.c2 }));
    // c3: mention to C, plain comment to A, nothing for the self-mention.
    assert!(ev.contains(&NotificationEvent::CommentMention {
        comment_id: sc.c3,
        user_id: sc.c
    }));
    assert!(!ev.contains(&NotificationEvent::CommentMention {
        comment_id: sc.c3,
        user_id: sc.b
    }));
    assert!(ev.contains(&NotificationEvent::ClaimComment { comment_id: sc.c3 }));
    // Battle reply to A's comment → reply only, no battle_comment.
    assert!(ev.contains(&NotificationEvent::CommentReply { comment_id: sc.bc2 }));
    assert!(!ev.contains(&NotificationEvent::BattleComment { comment_id: sc.bc2 }));

    // Delivery resolves the right recipient + copy.
    let mailer = Arc::new(FakeMailer::default());
    let rt = runtime(mailer.clone());
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::CommentReply { comment_id: sc.c2 },
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        DeliveryOutcome::Sent {
            to: user_email(&pool, sc.c).await
        }
    );
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::CommentMention {
            comment_id: sc.c2,
            user_id: sc.a,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        DeliveryOutcome::Sent {
            to: user_email(&pool, sc.a).await
        }
    );
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::CommentReply { comment_id: sc.bc2 },
    )
    .await
    .unwrap();
    assert!(matches!(out, DeliveryOutcome::Sent { .. }), "{out:?}");
    {
        let sent = mailer.sent.lock().unwrap();
        assert_eq!(sent.len(), 3);
        assert!(
            sent[0]
                .subject
                .contains("replied to your comment on the claim")
        );
        assert!(sent[1].subject.contains("mentioned you on the claim"));
        assert!(
            sent[2]
                .subject
                .contains("replied to your comment on the battle")
        );
        assert!(
            sent[2]
                .text
                .contains("https://krabarena.test/battles/notif-battle-")
        );
    }

    // A mention of a user not in comment_mentions → no context.
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::CommentMention {
            comment_id: sc.c1,
            user_id: sc.a,
        },
    )
    .await
    .unwrap();
    assert_eq!(out, DeliveryOutcome::NoContext);

    // Self-reply: no reply event is generated, and a forced delivery is
    // skipped as the actor.
    let own = crate::services::comment_service::create_comment(
        &pool,
        sc.claim,
        sc.c,
        comment_req("replying to myself", Some(sc.c1)),
    )
    .await
    .unwrap();
    assert!(
        !notification_email::recorded_events()
            .contains(&NotificationEvent::CommentReply { comment_id: own.id })
    );
    let out = notification_email::deliver(
        &pool,
        &rt,
        NotificationEvent::CommentReply { comment_id: own.id },
    )
    .await
    .unwrap();
    assert_eq!(out, DeliveryOutcome::Skipped(SkipReason::Actor));
}

#[tokio::test]
async fn read_state_is_per_item_shared_and_filterable() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;
    let unread_only = activity_service::NotificationFilter {
        unread_only: true,
        kinds: None,
    };

    let (all, _) = activity_service::user_notifications(&pool, sc.a, None, 50)
        .await
        .expect("feed");
    assert!(!all.is_empty());
    assert!(all.iter().all(|i| !i.read), "a fresh feed starts unread");
    let total = all.len() as i64;
    assert_eq!(
        activity_service::unread_count(&pool, sc.a).await.unwrap(),
        total
    );

    let opened = all[0].id;
    activity_service::mark_read(&pool, sc.a, &[opened])
        .await
        .unwrap();
    activity_service::mark_read(&pool, sc.a, &[opened])
        .await
        .unwrap();

    let (after, _) = activity_service::user_notifications(&pool, sc.a, None, 50)
        .await
        .unwrap();
    assert!(after.iter().find(|i| i.id == opened).unwrap().read);
    assert_eq!(after.iter().filter(|i| i.read).count(), 1);
    assert_eq!(
        activity_service::unread_count(&pool, sc.a).await.unwrap(),
        total - 1
    );

    let (unread, _) =
        activity_service::user_notifications_filtered(&pool, sc.a, None, 50, &unread_only)
            .await
            .unwrap();
    assert_eq!(unread.len() as i64, total - 1);
    assert!(unread.iter().all(|i| i.id != opened && !i.read));

    let (other_user, _) = activity_service::user_notifications(&pool, sc.b, None, 50)
        .await
        .unwrap();
    assert!(
        other_user.iter().all(|i| !i.read),
        "read state must not leak across users"
    );

    let only_reviews = activity_service::NotificationFilter {
        unread_only: false,
        kinds: Some(vec!["claim_review".into()]),
    };
    let (reviews, _) =
        activity_service::user_notifications_filtered(&pool, sc.a, None, 50, &only_reviews)
            .await
            .unwrap();
    assert!(!reviews.is_empty());
    assert!(reviews.iter().all(|i| i.kind == "claim_review"));

    activity_service::mark_all_read(&pool, sc.a).await.unwrap();
    assert_eq!(
        activity_service::unread_count(&pool, sc.a).await.unwrap(),
        0
    );
    let (cleared, _) =
        activity_service::user_notifications_filtered(&pool, sc.a, None, 50, &unread_only)
            .await
            .unwrap();
    assert!(cleared.is_empty());
    let (everything, _) = activity_service::user_notifications(&pool, sc.a, None, 50)
        .await
        .unwrap();
    assert!(everything.iter().all(|i| i.read));
}

#[tokio::test]
async fn unread_pagination_walks_every_unread_item_once() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let sc = seed_scenario(&pool).await;
    let (all, _) = activity_service::user_notifications(&pool, sc.a, None, 50)
        .await
        .unwrap();
    let read_every_other: Vec<Uuid> = all.iter().step_by(2).map(|i| i.id).collect();
    activity_service::mark_read(&pool, sc.a, &read_every_other)
        .await
        .unwrap();

    let filter = activity_service::NotificationFilter {
        unread_only: true,
        kinds: None,
    };
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let (page, meta) = activity_service::user_notifications_filtered(
            &pool,
            sc.a,
            cursor.as_deref(),
            1,
            &filter,
        )
        .await
        .unwrap();
        seen.extend(page.iter().map(|i| i.id));
        if !meta.has_more {
            break;
        }
        cursor = meta.next_cursor;
    }
    let want: Vec<Uuid> = all.iter().skip(1).step_by(2).map(|i| i.id).collect();
    assert_eq!(seen, want);
}
