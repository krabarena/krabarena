//! End-to-end check that the deployed apex correctly 301s
//! `krabarena.org` → `krabarena.com` (added in 0.10.0).
//!
//! Marked `#[ignore]` so unit-only CI stays fast. Run manually
//! after a prod deploy:
//!   cargo test -p krabarena-api --test redirect_e2e -- --ignored
//!
//! Unit coverage for the middleware logic itself lives next to
//! the implementation in `src/web/redirect.rs`.

use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode};

async fn no_follow() -> Client {
    Client::builder()
        .redirect(Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}

#[tokio::test]
#[ignore]
async fn apex_org_301s_to_com_preserving_path() {
    let resp = no_follow()
        .await
        .get("https://krabarena.org/v1/about")
        .send()
        .await
        .expect("request failed");
    // axum's Redirect::permanent() emits 308 (RFC 7538 Permanent
    // Redirect), not 301 — same SEO semantics, but 308 explicitly
    // forbids method downgrade (POST → GET) on redirect.
    assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("missing Location header")
        .to_str()
        .unwrap();
    assert_eq!(location, "https://krabarena.com/v1/about");
}

#[tokio::test]
#[ignore]
async fn www_org_301s_to_www_com_preserving_query() {
    let resp = no_follow()
        .await
        .get("https://www.krabarena.org/battles/foo?cb=1")
        .send()
        .await
        .expect("request failed");
    // axum's Redirect::permanent() emits 308 (RFC 7538 Permanent
    // Redirect), not 301 — same SEO semantics, but 308 explicitly
    // forbids method downgrade (POST → GET) on redirect.
    assert_eq!(resp.status(), StatusCode::PERMANENT_REDIRECT);
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .expect("missing Location header")
        .to_str()
        .unwrap();
    assert_eq!(location, "https://www.krabarena.com/battles/foo?cb=1");
}

#[tokio::test]
#[ignore]
async fn com_apex_serves_directly_without_redirect() {
    let resp = no_follow()
        .await
        .get("https://krabarena.com/health")
        .send()
        .await
        .expect("request failed");
    assert_eq!(resp.status(), StatusCode::OK);
}
