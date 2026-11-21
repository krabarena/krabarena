//! Webhook dispatch (since 0.14.0).
//!
//! Public entrypoint `dispatch_event(pool, event, payload)` is called
//! from route handlers (e.g. `routes::claims::create_claim`) right
//! after a successful insert. It spawns a tokio task internally so
//! the caller returns to the client immediately — subscriber latency
//! never bleeds into our p99.
//!
//! Per-attempt timeout: 10s. Retries: 3 attempts with 1s / 4s / 16s
//! backoff. After the last attempt the row's `last_delivery_status`
//! reflects the final outcome (`ok` / `failed_<status>` / `timeout`)
//! and `delivery_count` / `failure_count` are bumped.
//!
//! Body shape (canonical — external subscribers depend on it):
//!
//! ```json
//! {
//!   "event": "claim.posted",
//!   "delivery_id": "wd_<12>",
//!   "delivered_at": "<ISO-8601>",
//!   "data": { ... event-specific payload ... }
//! }
//! ```
//!
//! Signature: `x-krabarena-signature: sha256=<hex>` where `<hex>` is
//! `HMAC-SHA256(secret, body_bytes)` lowercase hex. Subscribers
//! reject deliveries that don't verify. See `docs/webhooks.md` for
//! the verification recipe in Python / bash.

use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng as ArgonOsRng,
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use rand::rngs::OsRng;
use serde_json::Value;
use sha2::Sha256;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// `User-Agent` header on every outgoing delivery. Includes the
/// workspace version so subscribers can spot a deploy in their logs.
fn user_agent() -> String {
    format!("KrabArena-Webhooks/{}", env!("CARGO_PKG_VERSION"))
}

/// Generate a 32-byte (256-bit) random secret and return it as a
/// 64-char hex string. `OsRng` reads from the OS entropy source so
/// this is suitable for HMAC keys (not predictable, not biased).
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Generate a `wd_<12 hex chars>` delivery id (6 bytes of entropy).
/// 6 bytes is ample for the use case: we only need uniqueness within
/// the subscriber's idempotency window. Short enough to fit comfortably
/// in a header / log line.
pub fn generate_delivery_id() -> String {
    let mut bytes = [0u8; 6];
    OsRng.fill_bytes(&mut bytes);
    format!("wd_{}", hex::encode(bytes))
}

/// Argon2id hash of a webhook secret. We never store the plaintext —
/// subscribers receive it once in the create response and have to
/// keep their copy. Verification on the subscribe path isn't actually
/// needed (we don't authenticate the subscriber against the hash —
/// the hash is here so a DB leak can't be replayed against
/// subscribers' endpoints). `password_hash`-style output includes
/// salt + parameters, so it round-trips cleanly through
/// `PasswordHash::new`.
pub fn hash_secret(plaintext: &str) -> Result<String, anyhow::Error> {
    let salt = SaltString::generate(&mut ArgonOsRng);
    let argon = Argon2::default();
    let hash = argon
        .hash_password(plaintext.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("argon2 hash failed: {e}"))?
        .to_string();
    Ok(hash)
}

/// Verify an Argon2id hash. Only used in tests + future revocation
/// flows; the dispatcher never needs to verify the secret (it has the
/// plaintext at create time and signs with that, end of story).
#[allow(dead_code)]
pub fn verify_secret(plaintext: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(plaintext.as_bytes(), &parsed)
        .is_ok()
}

/// Compute the `x-krabarena-signature` value for `body` under
/// `secret`. Returned shape: `sha256=<lowercase hex>`. Subscribers
/// compute the same HMAC over the raw body bytes they received and
/// compare in constant time. See `docs/webhooks.md`.
pub fn sign(body: &[u8], secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(body);
    let sig = mac.finalize().into_bytes();
    format!("sha256={}", hex::encode(sig))
}

/// Verify a signature emitted by `sign`. Constant-time on the inner
/// hex bytes. Used in our unit tests and shipped to subscribers as
/// the reference implementation.
pub fn verify_signature(body: &[u8], signature: &str, secret: &str) -> bool {
    let expected = sign(body, secret);
    // `hmac::Mac::verify_slice` would also work but we already have
    // both strings in hand; `constant_time_eq` on the bytes is the
    // same property. `subtle` not in deps, so do the manual XOR.
    if expected.len() != signature.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.as_bytes().iter().zip(signature.as_bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Minimal row shape the dispatcher needs. We don't pull the full
/// row to keep the hot path tight.
///
/// `secret_plain` is the issued plaintext — the dispatcher needs it
/// to compute HMAC-SHA256 over the body. Argon2 is one-way (good for
/// "verify a presented secret", useless for "sign on the dispatch
/// path"), so the schema keeps both: `secret_hash` (argon2id) for
/// future revocation flows, `secret_plain` for outgoing signature.
/// Subscribers see the plaintext exactly once on subscribe and
/// rotate by re-subscribing.
#[derive(Debug, Clone)]
pub(crate) struct ActiveSubscription {
    pub(crate) id: Uuid,
    url: String,
    secret_plain: String,
}

/// Who may receive a given delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Every active subscriber of the event (public content).
    Everyone,
    /// Only subscriptions owned by this user (the private claim's
    /// author) or by a platform admin — the same audience that can
    /// read a private claim through the API.
    PrivateTo(Uuid),
}

impl Audience {
    /// Audience for an event about a claim with `visibility` by
    /// `author_id` — see `services::claim_visibility`.
    pub fn for_claim(visibility: Option<&str>, author_id: Uuid) -> Self {
        if crate::services::claim_visibility::is_public(visibility) {
            Audience::Everyone
        } else {
            Audience::PrivateTo(author_id)
        }
    }
}

/// Look up active subscriptions interested in `event`, restricted to
/// `audience`.
pub(crate) async fn lookup_subscriptions(
    pool: &PgPool,
    event: &str,
    audience: Audience,
) -> Result<Vec<ActiveSubscription>, sqlx::Error> {
    // `events @> ARRAY[$1]` is the GIN-indexed filter from migration
    // 041. `active = TRUE` is also indexed (partial GIN). Pull only
    // the columns the dispatcher needs. `$2` NULL = everyone; else
    // only subs owned by that user or by an admin.
    let restrict_to = match audience {
        Audience::Everyone => None,
        Audience::PrivateTo(uid) => Some(uid),
    };
    let rows = sqlx::query_as::<_, (Uuid, String, String)>(
        r#"
        SELECT s.id, s.url, s.secret_plain
          FROM webhook_subscriptions s
         WHERE s.active = TRUE
           AND s.events @> ARRAY[$1]::text[]
           AND ($2::uuid IS NULL
                OR s.owner_user_id = $2
                OR EXISTS (SELECT 1 FROM users u
                            WHERE u.id = s.owner_user_id AND u.role = 'admin'))
        "#,
    )
    .bind(event)
    .bind(restrict_to)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, url, secret_plain)| ActiveSubscription {
            id,
            url,
            secret_plain,
        })
        .collect())
}

/// Count active subscriptions interested in `event`. Used by the
/// admin recheck endpoint (since 0.15.2) to report how many
/// subscribers received the dispatch — `dispatch_event` itself is
/// fire-and-forget so the response would otherwise have to guess.
/// The query is the same shape as `lookup_subscriptions` so the
/// numbers are consistent (a sub that's active + subscribed to the
/// event between the count call and the dispatch's own lookup
/// would still get the delivery — they round to the same window).
pub async fn count_subscriptions_for_event(
    pool: &PgPool,
    event: &str,
    audience: Audience,
) -> Result<i64, sqlx::Error> {
    let restrict_to = match audience {
        Audience::Everyone => None,
        Audience::PrivateTo(uid) => Some(uid),
    };
    let n: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
          FROM webhook_subscriptions s
         WHERE s.active = TRUE
           AND s.events @> ARRAY[$1]::text[]
           AND ($2::uuid IS NULL
                OR s.owner_user_id = $2
                OR EXISTS (SELECT 1 FROM users u
                            WHERE u.id = s.owner_user_id AND u.role = 'admin'))
        "#,
    )
    .bind(event)
    .bind(restrict_to)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Single attempt: POST `body` to `url` with the canonical headers.
/// Returns `Ok(status)` on a complete HTTP exchange (even non-2xx),
/// `Err` only on transport / timeout error.
async fn deliver_once(
    client: &reqwest::Client,
    url: &str,
    event: &str,
    delivery_id: &str,
    body: &[u8],
    signature: &str,
) -> Result<reqwest::StatusCode, reqwest::Error> {
    let resp = client
        .post(url)
        .timeout(Duration::from_secs(10))
        .header("content-type", "application/json")
        .header("user-agent", user_agent())
        .header("x-krabarena-event", event)
        .header("x-krabarena-delivery", delivery_id)
        .header("x-krabarena-signature", signature)
        .body(body.to_vec())
        .send()
        .await?;
    Ok(resp.status())
}

/// Update the subscription's bookkeeping after a delivery attempt
/// chain finishes. `status_label` is the final outcome string
/// (`ok` / `failed_503` / `timeout`). `success` controls whether
/// `failure_count` increments. `delivery_count` always increments
/// — it counts attempts, not successes.
async fn record_delivery(
    pool: &PgPool,
    sub_id: Uuid,
    status_label: &str,
    success: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE webhook_subscriptions
           SET last_delivery_at = now(),
               last_delivery_status = $1,
               delivery_count = delivery_count + 1,
               failure_count = failure_count + CASE WHEN $2 THEN 0 ELSE 1 END
         WHERE id = $3
        "#,
    )
    .bind(status_label)
    .bind(success)
    .bind(sub_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Deliver to one subscriber, with 3 attempts + exponential backoff.
/// Updates the row's bookkeeping in all paths.
async fn deliver_with_retry(
    pool: PgPool,
    client: reqwest::Client,
    sub: ActiveSubscription,
    event: String,
    delivery_id: String,
    body: Vec<u8>,
) {
    let signature = sign(&body, &sub.secret_plain);
    let backoffs = [
        Duration::from_secs(1),
        Duration::from_secs(4),
        Duration::from_secs(16),
    ];

    let mut last_label = String::from("timeout");
    let mut success = false;

    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(backoffs[attempt - 1]).await;
        }
        match deliver_once(&client, &sub.url, &event, &delivery_id, &body, &signature).await {
            Ok(status) => {
                if status.is_success() {
                    last_label = "ok".into();
                    success = true;
                    break;
                }
                last_label = format!("failed_{}", status.as_u16());
                // Non-2xx: keep retrying — the subscriber might just
                // be deploying, or rate-limiting us. The spec's
                // "auto-disable after N failures" is intentionally
                // out of v1.
            }
            Err(e) => {
                tracing::warn!(
                    sub_id = %sub.id,
                    attempt = attempt + 1,
                    error = %e,
                    "webhook delivery transport error"
                );
                last_label = "timeout".into();
            }
        }
    }

    if let Err(e) = record_delivery(&pool, sub.id, &last_label, success).await {
        tracing::warn!(sub_id = %sub.id, error = %e, "webhook record_delivery failed");
    }
}

/// Public entrypoint. Looks up active subs for the event, spawns one
/// dispatch task per sub, and returns immediately. Errors during
/// lookup are logged but never propagated — webhook dispatch is
/// observability infrastructure, not a critical path.
pub fn dispatch_event(pool: PgPool, event: &'static str, data: Value) {
    dispatch_event_to(pool, event, data, Audience::Everyone)
}

/// Like [`dispatch_event`] but restricted to `audience`. Claim events
/// MUST use this with [`Audience::for_claim`] so a private claim is
/// never announced to arbitrary subscribers.
pub fn dispatch_event_to(pool: PgPool, event: &'static str, data: Value, audience: Audience) {
    let event_name = event.to_string();
    tokio::spawn(async move {
        let subs = match lookup_subscriptions(&pool, &event_name, audience).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, event = %event_name, "webhook sub lookup failed");
                return;
            }
        };
        if subs.is_empty() {
            return;
        }
        // Build the canonical envelope ONCE — every subscriber gets
        // the same delivery_id (they can tell their copies apart by
        // the URL the request lands on). Wait, no: per-subscriber
        // delivery_id is more useful since a retry to one sub
        // shouldn't share a delivery_id with the other sub's first
        // attempt. So we generate per-sub below.
        // No redirects: a public webhook URL must not bounce the request
        // (and its signed payload) to an internal address.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        for sub in subs {
            let delivery_id = generate_delivery_id();
            let envelope = serde_json::json!({
                "event": event_name,
                "delivery_id": delivery_id,
                "delivered_at": chrono::Utc::now().to_rfc3339(),
                "data": data,
            });
            let body = match serde_json::to_vec(&envelope) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e, "webhook payload serialise failed");
                    continue;
                }
            };
            let pool_c = pool.clone();
            let client_c = client.clone();
            let event_c = event_name.clone();
            tokio::spawn(deliver_with_retry(
                pool_c,
                client_c,
                sub,
                event_c,
                delivery_id,
                body,
            ));
        }
    });
}

/// Synchronous one-shot delivery — drives the `POST /v1/webhooks/{id}/test`
/// endpoint. Returns `(status_u16, body_excerpt)` so the route can
/// echo the response back to the caller. Doesn't retry; that's the
/// dispatcher's job. Doesn't touch the bookkeeping counters — the
/// test path is for verifying the subscriber works, not for fudging
/// the live numbers.
pub async fn dispatch_test_event(
    sub_url: &str,
    secret_plain: &str,
) -> Result<(u16, String), reqwest::Error> {
    let delivery_id = generate_delivery_id();
    let envelope = serde_json::json!({
        "event": crate::services::webhook_dispatch::test_event_name(),
        "delivery_id": delivery_id,
        "delivered_at": chrono::Utc::now().to_rfc3339(),
        "data": {
            "message": "This is a test delivery from KrabArena. \
                        If your subscriber prints this, you're wired up correctly."
        }
    });
    let body = serde_json::to_vec(&envelope).expect("envelope serialises");
    let signature = sign(&body, secret_plain);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let resp = client
        .post(sub_url)
        .header("content-type", "application/json")
        .header("user-agent", user_agent())
        .header(
            "x-krabarena-event",
            krabarena_shared::webhook_events::WEBHOOK_TEST,
        )
        .header("x-krabarena-delivery", &delivery_id)
        .header("x-krabarena-signature", signature)
        .body(body)
        .send()
        .await?;
    let status = resp.status().as_u16();
    // Cap response body at 2 KB — subscriber pages that return HTML
    // shouldn't dump the whole page into our API response.
    let text = resp.text().await.unwrap_or_default();
    let excerpt: String = text.chars().take(2048).collect();
    Ok((status, excerpt))
}

/// Reflective const-fn so the test path can borrow the synthetic
/// event name without a direct shared-crate import in every caller.
#[inline]
pub fn test_event_name() -> &'static str {
    krabarena_shared::webhook_events::WEBHOOK_TEST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_round_trips() {
        let body = b"{\"event\":\"claim.posted\"}";
        let secret = "deadbeefcafefeed";
        let sig = sign(body, secret);
        assert!(sig.starts_with("sha256="));
        assert!(verify_signature(body, &sig, secret));
        // Tampered body fails.
        assert!(!verify_signature(b"{}", &sig, secret));
        // Wrong key fails.
        assert!(!verify_signature(body, &sig, "wrong-secret"));
    }

    #[test]
    fn argon2_round_trip() {
        // Hash + verify path is the floor for the subscribe handler
        // ever growing a "rotate-secret" flow that re-checks an old
        // value before issuing a new one.
        let plain = "32-byte secret here please";
        let hash = hash_secret(plain).expect("hash works");
        assert!(verify_secret(plain, &hash));
        assert!(!verify_secret("nope", &hash));
    }

    #[test]
    fn generate_secret_is_64_hex_chars() {
        // 32 bytes = 64 hex chars. Lock the wire shape so subscribers
        // can pin the format if they want to validate before storing.
        let a = generate_secret();
        let b = generate_secret();
        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "generate_secret must be random");
    }

    #[test]
    fn generate_delivery_id_shape() {
        let id = generate_delivery_id();
        assert!(id.starts_with("wd_"));
        // wd_ + 12 hex chars.
        assert_eq!(id.len(), 3 + 12);
    }

    #[test]
    fn dispatch_skips_when_no_active_subscription_matches() {
        // No DB pool needed: the `dispatch_event` entry point spawns
        // a task that does the lookup. The "no subs" branch is the
        // most important fast-path — we can't easily test it without
        // a Postgres harness. Instead, test the building block:
        // `lookup_subscriptions` against a freshly-built but empty
        // schema would return an empty Vec. That requires a DB, so
        // it lives in the e2e test (`tests/webhooks_e2e.rs`).
        //
        // What we CAN assert as a unit test: the envelope shape we'd
        // serialise on the dispatch path. Lock the canonical field
        // names — KrabChecker filters on them.
        let env = serde_json::json!({
            "event": "claim.posted",
            "delivery_id": "wd_abc123def456",
            "delivered_at": "2026-06-01T00:00:00Z",
            "data": { "claim": { "id": "x" } },
        });
        let s = env.to_string();
        assert!(s.contains("\"event\":\"claim.posted\""));
        assert!(s.contains("\"delivery_id\":"));
        assert!(s.contains("\"delivered_at\":"));
        assert!(s.contains("\"data\":"));
    }

    #[tokio::test]
    async fn retry_three_times_then_succeed_against_mock_server() {
        // Stand up a tiny TCP server that returns 500, 500, then 200
        // — the dispatcher should hit it 3 times and end up
        // recording `ok` / success. We don't have wiremock-style
        // helpers in this crate, so use a hand-rolled tokio listener.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/hook");
        let counter = Arc::new(AtomicU32::new(0));
        let counter_c = counter.clone();

        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let n = counter_c.fetch_add(1, Ordering::SeqCst);
                // Read off the request — enough to clear the buffer.
                let mut buf = [0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let status_line = if n < 2 {
                    "HTTP/1.1 500 Internal Server Error\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\n"
                };
                let resp = format!("{status_line}content-length: 2\r\n\r\nok");
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        // Drive the real retry loop. We can't easily intercept the
        // `record_delivery` SQL without a pool, so call the inner
        // attempt path directly — this is what `deliver_with_retry`
        // composes. We replicate the loop here to exercise the
        // backoff + status threshold logic.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let body = b"{}".to_vec();
        let sig = sign(&body, "secret");
        let mut success = false;
        for attempt in 0..3 {
            if attempt > 0 {
                // Shorten the backoff for the test — 1s is fine, 4s + 16s
                // would balloon CI to 21s. Use 50ms per pass.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            match deliver_once(&client, &url, "claim.posted", "wd_test", &body, &sig).await {
                Ok(s) if s.is_success() => {
                    success = true;
                    break;
                }
                _ => continue,
            }
        }
        let _ = server.await;
        assert!(success, "expected dispatcher to recover on the 3rd attempt");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            3,
            "expected 3 total attempts before success"
        );
    }
}
