//! Integration test: every battle/claim API endpoint accepts *both* a
//! canonical UUID and a human-readable slug in the `{id}` path segment.
//!
//! Scenario this guards: an agent is handed a browser URL
//! `http://127.0.0.1:3100/battles/what-is-the-best-web-access-api-for-ai-agents`
//! and runs `krab battle get <slug>` or `krab claim list <battle_slug>`.
//! The JSON API used to 400 on that because `Path<Uuid>` couldn't parse
//! the slug — the agent had no way to talk to the API without first
//! resolving the slug to a UUID by hand. Now the handler calls
//! `slug_resolver::resolve_{battle,claim}()` which parses UUID first
//! and falls back to a DB lookup on `slug`.
//!
//! These tests are `#[ignore]` so they don't run in default `cargo
//! test` — they need a KrabArena server running on 0.4.5+ with known
//! battle / claim rows. Run:
//!
//!   KRAB_TEST_BASE=http://127.0.0.1:3100 \
//!   KRAB_TEST_BATTLE_SLUG=what-is-the-best-web-access-api-for-ai-agents \
//!   KRAB_TEST_BATTLE_UUID=09ab746f-52fd-44e2-9db7-71fa00e63bd7 \
//!   KRAB_TEST_CLAIM_SLUG=keenable-top-10-news-freshness-... \
//!   KRAB_TEST_CLAIM_UUID=3c7875fb-717a-4f76-87c6-e0c827e8270d \
//!     cargo test -p krabarena-api --test slug_resolution -- --ignored --nocapture
//!
//! Any missing env var skips cleanly so CI doesn't explode on envs
//! that haven't been seeded.

use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3100";

fn base() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn skip_unless(var: &str) -> Option<String> {
    let v = std::env::var(var).ok()?;
    if v.trim().is_empty() {
        eprintln!("skip: ${} is empty", var);
        return None;
    }
    Some(v)
}

async fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

async fn get_json(c: &Client, path: &str) -> (StatusCode, Value) {
    let resp = c
        .get(format!("{}{}", base(), path))
        .send()
        .await
        .expect("HTTP request failed");
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, body)
}

// --- Battle endpoints ---

#[tokio::test]
#[ignore]
async fn battle_get_by_slug_returns_same_row_as_by_uuid() {
    let Some(slug) = skip_unless("KRAB_TEST_BATTLE_SLUG") else {
        return;
    };
    let Some(uuid) = skip_unless("KRAB_TEST_BATTLE_UUID") else {
        return;
    };
    let c = client().await;
    let (s1, by_slug) = get_json(&c, &format!("/v1/battles/{}", slug)).await;
    let (s2, by_uuid) = get_json(&c, &format!("/v1/battles/{}", uuid)).await;
    assert_eq!(s1, StatusCode::OK, "slug GET failed: {by_slug}");
    assert_eq!(s2, StatusCode::OK, "uuid GET failed: {by_uuid}");
    assert_eq!(
        by_slug["data"]["id"], by_uuid["data"]["id"],
        "slug and uuid must resolve to the same battle"
    );
    assert_eq!(by_slug["data"]["id"].as_str().unwrap(), uuid);
}

#[tokio::test]
#[ignore]
async fn battle_proxy_info_by_slug() {
    let Some(slug) = skip_unless("KRAB_TEST_BATTLE_SLUG") else {
        return;
    };
    let c = client().await;
    let (s, _body) = get_json(&c, &format!("/v1/battles/{}/proxy-info", slug)).await;
    // 200 if the battle has a proxy spec, 404 if it doesn't — either way
    // the slug-vs-UUID resolution shouldn't fail with a UUID parse error.
    assert!(
        s == StatusCode::OK || s == StatusCode::NOT_FOUND,
        "unexpected status {s}"
    );
}

#[tokio::test]
#[ignore]
async fn list_claims_by_battle_slug() {
    let Some(slug) = skip_unless("KRAB_TEST_BATTLE_SLUG") else {
        return;
    };
    let c = client().await;
    let (s, body) = get_json(&c, &format!("/v1/battles/{}/claims", slug)).await;
    assert_eq!(s, StatusCode::OK, "claim list failed: {body}");
    assert!(body["data"].is_array(), "expected claim list array");
}

// --- Claim endpoints ---

#[tokio::test]
#[ignore]
async fn claim_get_by_slug_returns_same_row_as_by_uuid() {
    let Some(slug) = skip_unless("KRAB_TEST_CLAIM_SLUG") else {
        return;
    };
    let Some(uuid) = skip_unless("KRAB_TEST_CLAIM_UUID") else {
        return;
    };
    let c = client().await;
    let (s1, by_slug) = get_json(&c, &format!("/v1/claims/{}", slug)).await;
    let (s2, by_uuid) = get_json(&c, &format!("/v1/claims/{}", uuid)).await;
    assert_eq!(s1, StatusCode::OK, "slug GET failed: {by_slug}");
    assert_eq!(s2, StatusCode::OK, "uuid GET failed: {by_uuid}");
    assert_eq!(by_slug["data"]["id"], by_uuid["data"]["id"]);
    assert_eq!(by_slug["data"]["id"].as_str().unwrap(), uuid);
}

#[tokio::test]
#[ignore]
async fn claim_activity_by_slug() {
    let Some(slug) = skip_unless("KRAB_TEST_CLAIM_SLUG") else {
        return;
    };
    let c = client().await;
    let (s, body) = get_json(&c, &format!("/v1/claims/{}/activity", slug)).await;
    assert_eq!(s, StatusCode::OK, "claim activity failed: {body}");
    assert!(body["data"].is_array(), "expected activity list array");
}

#[tokio::test]
#[ignore]
async fn claim_verifications_by_slug() {
    let Some(slug) = skip_unless("KRAB_TEST_CLAIM_SLUG") else {
        return;
    };
    let c = client().await;
    let (s, body) = get_json(&c, &format!("/v1/claims/{}/verifications", slug)).await;
    assert_eq!(s, StatusCode::OK, "verifications failed: {body}");
    assert!(body["data"].is_array());
}

#[tokio::test]
#[ignore]
async fn claim_comments_by_slug() {
    let Some(slug) = skip_unless("KRAB_TEST_CLAIM_SLUG") else {
        return;
    };
    let c = client().await;
    let (s, body) = get_json(&c, &format!("/v1/claims/{}/comments", slug)).await;
    assert_eq!(s, StatusCode::OK, "comments failed: {body}");
    assert!(body["data"].is_array());
}

// --- Error paths ---

#[tokio::test]
#[ignore]
async fn unknown_battle_slug_returns_battle_not_found() {
    let c = client().await;
    let (s, body) = get_json(&c, "/v1/battles/this-slug-definitely-does-not-exist").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        body["error"]["code"].as_str().unwrap_or(""),
        "BATTLE_NOT_FOUND",
        "expected BATTLE_NOT_FOUND, got: {body}"
    );
}

#[tokio::test]
#[ignore]
async fn unknown_claim_slug_returns_claim_not_found() {
    let c = client().await;
    let (s, body) = get_json(&c, "/v1/claims/this-slug-definitely-does-not-exist").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        body["error"]["code"].as_str().unwrap_or(""),
        "CLAIM_NOT_FOUND",
        "expected CLAIM_NOT_FOUND, got: {body}"
    );
}
