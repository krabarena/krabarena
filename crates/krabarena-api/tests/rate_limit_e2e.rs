//! End-to-end check for the Redis rate limiter (added 0.26.0).
//!
//! Marked `#[ignore]` so unit-only CI stays fast. Requires a running
//! server with `REDIS_URL` set (the limiter is a no-op otherwise). Run
//! against a local docker-compose stack:
//!
//!   docker-compose up --build           # api + db + redis
//!   cargo test -p krabarena-api --test rate_limit_e2e -- --ignored
//!
//! Base URL comes from `KRAB_TEST_BASE` (defaults to the compose port
//! `http://localhost:3001`). Each test pins a distinct
//! `X-Forwarded-For` so the server keys the fixed window on that IP;
//! because the window is 60s, re-running the same test twice inside a
//! minute can see a pre-warmed counter — bump the test IP or wait out
//! the window if that bites.
//!
//! The authenticated test (`global_tier_keys_authenticated_user_...`)
//! mints a JWT offline with the compose `JWT_SECRET` (`dev-secret`) and
//! a fresh random `sub`, so its bucket is always cold regardless of when
//! the suite last ran.
//!
//! Unit coverage for `classify` / `client_ip` / `rate_key` /
//! `verified_user_id` lives next to the implementation in
//! `src/web/rate_limit.rs`.

use reqwest::{Client, StatusCode};

/// Secret the compose `api` service signs JWTs with (see
/// docker-compose.yml `JWT_SECRET`). Lets the test mint a token the
/// running server will accept without an auth round-trip.
const COMPOSE_JWT_SECRET: &str = "dev-secret";

fn base() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| "http://localhost:3001".to_string())
}

fn client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}

/// Read the `RateLimit-Remaining` header as an i64, or fail loudly.
fn remaining(resp: &reqwest::Response) -> i64 {
    resp.headers()
        .get("ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .expect("response must carry a numeric RateLimit-Remaining")
}

/// Strict tier (10/min): the 11th request from one IP to an auth-surface
/// endpoint must be throttled with a well-formed 429.
#[tokio::test]
#[ignore]
async fn strict_tier_throttles_auth_surface() {
    let c = client();
    // A strict, read-only endpoint: email poll just reports pending.
    let url = format!(
        "{}/v1/auth/email/poll?email=ratelimit-e2e@example.com",
        base()
    );
    let test_ip = "198.51.100.21"; // TEST-NET-2, pinned per test.

    let mut got_429 = false;
    let mut last: Option<reqwest::Response> = None;
    // 10 allowed + a margin; the limiter must trip within these.
    for _ in 0..14 {
        let resp = c
            .get(&url)
            .header("x-forwarded-for", test_ip)
            .send()
            .await
            .expect("request failed");
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            got_429 = true;
            last = Some(resp);
            break;
        }
    }

    assert!(
        got_429,
        "expected a 429 within 14 requests to a strict-tier endpoint"
    );
    let resp = last.unwrap();
    // Standard rate-limit headers present.
    assert!(
        resp.headers().get("retry-after").is_some(),
        "429 must carry Retry-After"
    );
    assert_eq!(
        resp.headers()
            .get("ratelimit-remaining")
            .and_then(|v| v.to_str().ok()),
        Some("0"),
        "RateLimit-Remaining must be 0 when throttled"
    );
    // Body uses the standard error envelope with our code.
    let body: serde_json::Value = resp.json().await.expect("429 body must be JSON");
    assert_eq!(body["error"]["code"], "RATE_LIMITED");
}

/// Global tier (120/min) must comfortably allow a burst that would have
/// tripped the strict tier — proves the path classification end-to-end.
#[tokio::test]
#[ignore]
async fn global_tier_allows_burst_past_strict_cap() {
    let c = client();
    let url = format!("{}/health", base());
    let test_ip = "198.51.100.77";

    for i in 0..20 {
        let resp = c
            .get(&url)
            .header("x-forwarded-for", test_ip)
            .send()
            .await
            .expect("request failed");
        assert_ne!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "global tier throttled at request {i}; should allow well past 20"
        );
        // Successful global-tier responses advertise the limit.
        if i == 0 {
            assert!(
                resp.headers().get("ratelimit-limit").is_some(),
                "global-tier responses should carry RateLimit-Limit"
            );
        }
    }
}

/// Authenticated global-tier requests are keyed on the verified
/// `user_id`, not the IP. Proof: the same user's counter keeps
/// decrementing across a change of `X-Forwarded-For` — if it were
/// IP-keyed, the second (different-IP) request would land on a fresh
/// bucket and `Remaining` would reset instead of stepping down by one.
#[tokio::test]
#[ignore]
async fn global_tier_keys_authenticated_user_by_id_not_ip() {
    use krabarena_api::services::auth_service;
    use uuid::Uuid;

    let c = client();
    let url = format!("{}/health", base());

    // Fresh random sub → guaranteed-cold bucket, robust to re-runs.
    let sub = Uuid::new_v4();
    let token = auth_service::create_jwt(sub, Uuid::new_v4(), "user", COMPOSE_JWT_SECRET)
        .expect("mint jwt");

    // Same user, two *different* source IPs, back to back.
    let r1 = c
        .get(&url)
        .header("authorization", format!("Bearer {token}"))
        .header("x-forwarded-for", "203.0.113.51")
        .send()
        .await
        .expect("request 1 failed");
    let rem1 = remaining(&r1);

    let r2 = c
        .get(&url)
        .header("authorization", format!("Bearer {token}"))
        .header("x-forwarded-for", "203.0.113.52") // different IP, same user
        .send()
        .await
        .expect("request 2 failed");
    let rem2 = remaining(&r2);

    assert_eq!(
        rem2,
        rem1 - 1,
        "authenticated counter must follow the user across IPs \
         (rem1={rem1}, rem2={rem2}); a reset would mean it's IP-keyed"
    );
}

/// Anonymous global-tier requests stay IP-keyed: two different IPs each
/// touch a fresh bucket, so both report the same first-hit `Remaining`.
/// The complement to the authenticated test above.
#[tokio::test]
#[ignore]
async fn global_tier_anonymous_stays_ip_keyed() {
    let c = client();
    let url = format!("{}/health", base());

    let first = |ip: &'static str| {
        let c = c.clone();
        let url = url.clone();
        async move {
            let resp = c
                .get(&url)
                .header("x-forwarded-for", ip)
                .send()
                .await
                .expect("request failed");
            remaining(&resp)
        }
    };

    // Two distinct, otherwise-unused TEST-NET-3 addresses.
    let a = first("203.0.113.41").await;
    let b = first("203.0.113.42").await;

    assert_eq!(
        a, b,
        "anonymous requests from different IPs should each hit a fresh \
         bucket and report equal Remaining (a={a}, b={b})"
    );
}

/// Static assets are exempt from the limiter: hammering one well past the
/// global cap never 429s, and the responses carry no `RateLimit-*`
/// headers (the middleware passes through before stamping them).
#[tokio::test]
#[ignore]
async fn static_assets_are_exempt() {
    let c = client();
    let url = format!("{}/static/style.css", base());
    let test_ip = "203.0.113.61";

    let mut last: Option<reqwest::Response> = None;
    for i in 0..150 {
        let resp = c
            .get(&url)
            .header("x-forwarded-for", test_ip)
            .send()
            .await
            .expect("request failed");
        assert_ne!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "static asset throttled at request {i}; assets must be exempt"
        );
        last = Some(resp);
    }
    let resp = last.unwrap();
    assert!(
        resp.headers().get("ratelimit-limit").is_none()
            && resp.headers().get("ratelimit-remaining").is_none(),
        "exempt static responses must not carry RateLimit-* headers"
    );
}

/// A browser *navigation* (Accept: text/html) that trips the limiter gets
/// a human-friendly HTML 429 with a `<meta http-equiv="refresh">`, not
/// the JSON envelope. Uses the strict tier so the trip is cheap — the
/// 429 representation depends on `Accept`, not the route.
#[tokio::test]
#[ignore]
async fn navigation_429_is_html_with_auto_refresh() {
    let c = client();
    let url = format!(
        "{}/v1/auth/email/poll?email=ratelimit-html-e2e@example.com",
        base()
    );
    let test_ip = "203.0.113.71";

    let mut throttled: Option<reqwest::Response> = None;
    for _ in 0..14 {
        let resp = c
            .get(&url)
            .header("x-forwarded-for", test_ip)
            .header("accept", "text/html,application/xhtml+xml")
            .send()
            .await
            .expect("request failed");
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            throttled = Some(resp);
            break;
        }
    }

    let resp = throttled.expect("expected a 429 within 14 strict-tier requests");
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/html"),
        "navigation 429 must be HTML, got content-type {ct:?}"
    );
    assert!(
        resp.headers().get("retry-after").is_some(),
        "navigation 429 must still carry Retry-After"
    );
    let body = resp.text().await.expect("429 body");
    assert!(
        body.contains("http-equiv=\"refresh\""),
        "HTML 429 must auto-refresh via a meta-refresh tag (no-JS fallback)"
    );
    assert!(
        body.contains("id=\"rl-secs\"") && body.contains("location.reload"),
        "HTML 429 must carry the live countdown script that reloads at zero"
    );
    assert!(
        body.contains("request limit"),
        "HTML 429 should explain the rate limit to the user"
    );
}
