//! Integration tests against the live KrabArena API.
//!
//! These tests are marked `#[ignore]` so they don't run in CI.
//! Run manually with:
//!   cargo test -p krabarena-api --test api_integration -- --ignored
//!
//! Requires: network access to https://krabarena.com

const BASE_URL: &str = "https://krabarena.com";

// Use a claim that exists in production with at least one
// verification — the "Rust appears in 26/80 AI-era paper titles"
// claim on the `python-vs-typescript-vs-rust-vs-go` battle has the
// most verify activity at the time of writing (5 verifications).
// Refreshed: `psql ... SELECT id FROM claims … HAVING COUNT(v) > 0 ORDER BY … LIMIT 1`.
const TEST_CLAIM_ID: &str = "175cb300-9049-4cd3-8838-500524e00fac";

async fn get(path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{}", BASE_URL, path))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .expect("HTTP request failed")
}

async fn get_json(path: &str) -> serde_json::Value {
    let resp = get(path).await;
    assert!(
        resp.status().is_success(),
        "GET {} returned {}",
        path,
        resp.status()
    );
    resp.json().await.expect("Failed to parse JSON")
}

fn api_path(path: &str, params: &[(&str, &str)]) -> String {
    let mut url =
        reqwest::Url::parse(&format!("https://example.test{path}")).expect("parse test API path");
    if !params.is_empty() {
        let mut query = url.query_pairs_mut();
        for (key, value) in params {
            query.append_pair(key, value);
        }
    }
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    }
}

// --- Arena ---

#[tokio::test]
#[ignore]
async fn arena_page_returns_200_html() {
    let resp = get("/arena").await;
    assert_eq!(resp.status().as_u16(), 200);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/html"), "Expected HTML, got: {}", ct);
}

#[tokio::test]
#[ignore]
async fn arena_api_returns_json_array() {
    let json = get_json("/v1/arena").await;
    assert!(json["data"].is_array(), "Arena data should be an array");
}

// --- OAuth ---

#[tokio::test]
#[ignore]
async fn voter_me_without_token_returns_401() {
    let resp = get("/auth/voter/me").await;
    assert_eq!(resp.status().as_u16(), 401);
}

// --- Claims ---

#[tokio::test]
#[ignore]
async fn latest_claims_endpoint_returns_newest_claims() {
    let json = get_json(&api_path("/v1/claims", &[("limit", "5")])).await;
    let data = json["data"].as_array().expect("claims should be an array");
    assert!(!data.is_empty(), "latest claims should not be empty");
    assert!(data.len() <= 5, "limit=5 should cap results: {data:?}");
    assert!(
        data[0]["created_at"].as_str().is_some(),
        "claim rows should carry created_at for watermark polling"
    );
    assert!(
        data[0]["slug"].as_str().is_some() || data[0]["id"].as_str().is_some(),
        "claim rows should carry a stable id/slug key"
    );
}

// --- Verifications & Activity ---

#[tokio::test]
#[ignore]
async fn list_verifications_for_existing_claim() {
    let json = get_json(&format!("/v1/claims/{}/verifications", TEST_CLAIM_ID)).await;
    let data = json["data"]
        .as_array()
        .expect("verifications should be an array");
    assert!(
        !data.is_empty(),
        "Claim should have at least one verification"
    );
}

#[tokio::test]
#[ignore]
async fn activity_feed_includes_verifications() {
    let json = get_json(&format!("/v1/claims/{}/activity", TEST_CLAIM_ID)).await;
    let data = json["data"]
        .as_array()
        .expect("activity should be an array");
    assert!(!data.is_empty(), "Activity feed should not be empty");
}

// --- Comments ---

#[tokio::test]
#[ignore]
async fn list_comments_endpoint_works() {
    let json = get_json(&format!("/v1/claims/{}/comments", TEST_CLAIM_ID)).await;
    assert!(json["data"].is_array(), "comments should be an array");
}

// --- Claim detail page ---

#[tokio::test]
#[ignore]
async fn claim_detail_page_renders() {
    let resp = get(&format!("/claims/{}", TEST_CLAIM_ID)).await;
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().await.unwrap();
    // Match a substring that's specific to TEST_CLAIM_ID's
    // current content (the "Rust appears in 26/80 AI-era paper
    // titles…" claim on the python-vs-typescript-vs-rust-vs-go
    // battle). Update if the claim's title/body changes.
    assert!(
        body.contains("Rust") || body.contains("AI-era paper"),
        "Claim page should show the benchmark claim body"
    );
}

// --- Manifest ---

#[tokio::test]
#[ignore]
async fn manifest_endpoint_works() {
    let json = get_json("/v1/cli/manifest").await;
    assert!(json["commands"].is_array());
    assert!(json["version"].as_u64().unwrap() >= 1);
}

// --- Smoke tests ---

#[tokio::test]
#[ignore]
async fn smoke_test_about_returns_200() {
    let resp = get("/about").await;
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
#[ignore]
async fn smoke_test_health_returns_200() {
    let resp = get("/health").await;
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
#[ignore]
async fn smoke_test_index_returns_200() {
    let resp = get("/").await;
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
#[ignore]
async fn smoke_test_agents_returns_200() {
    let resp = get("/agents").await;
    assert_eq!(resp.status().as_u16(), 200);
}
