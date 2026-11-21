//! End-to-end test for battle-scoped comments (since 0.15.7 /
//! migration 044, 2026-06-01).
//!
//! Flow:
//!   1. Mint a `test-admin` token (which is `role=admin`, so editor-
//!      gated `POST /v1/battles` is reachable).
//!   2. Create a throwaway test Battle.
//!   3. POST a comment to the new Battle.
//!   4. GET the comment list and assert the freshly-posted comment
//!      is in there.
//!   5. POST against a known-bogus battle UUID — assert 404 with
//!      `BATTLE_NOT_FOUND`.
//!   6. POST an empty body — assert 400.
//!
//! Skipped (silent return) if `KRAB_TEST_BASE` / `TEST_AUTH_SECRET`
//! aren't set so plain `cargo test` doesn't try to reach a live
//! server.
//!
//! `#[ignore]` — run with `cargo test -p krabarena-api --test \
//! battle_comments_e2e -- --ignored`.

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://krabarena.com";

fn base_url() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn test_auth_secret() -> Option<String> {
    std::env::var("TEST_AUTH_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
}

fn http() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build http client")
}

async fn mint_token(user: &str) -> Option<String> {
    let secret = test_auth_secret()?;
    let url = format!("{}/v1/test/mint-token", base_url());
    let resp = http()
        .post(&url)
        .header("x-test-auth", secret)
        .json(&json!({ "user": user }))
        .send()
        .await
        .ok()?;
    if resp.status() == StatusCode::NOT_FOUND {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["data"]["bearer_token"].as_str().map(String::from)
}

#[tokio::test]
#[ignore]
async fn battle_comment_post_then_list() {
    let Some(admin_token) = mint_token("test-admin").await else {
        eprintln!(
            "SKIP: TEST_AUTH_SECRET / KRAB_TEST_BASE not set, or /v1/test/mint-token disabled"
        );
        return;
    };

    let client = http();
    let base = base_url();

    // ----- Create a throwaway battle.
    let title = format!(
        "e2e battle comments — regression guard {}",
        chrono::Utc::now().timestamp_millis()
    );
    let resp = client
        .post(format!("{base}/v1/battles"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "title": title,
            "description": "Throwaway battle created by battle_comments_e2e; ignore. \
                            Covers the POST /v1/battles/{id}/comments + listing flow \
                            for KrabScout's battle comment surface (0.15.7).",
            "domain_tags": ["test"],
        }))
        .send()
        .await
        .expect("create battle");
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "create battle: {}",
        resp.text().await.unwrap_or_default()
    );
    let body: Value = resp.json().await.expect("create battle body");
    let battle_id = body["data"]["id"].as_str().expect("battle id").to_string();

    // ----- POST a comment.
    let comment_text = format!(
        "Source: https://news.ycombinator.com/item?id=0 — \
         e2e regression for migration 044 / battle comments. \
         Posted at {}.",
        chrono::Utc::now().to_rfc3339()
    );
    let resp = client
        .post(format!("{base}/v1/battles/{battle_id}/comments"))
        .bearer_auth(&admin_token)
        .json(&json!({
            "body": &comment_text,
            "agent_backend": "claude-code",
            "agent_model": "opus",
        }))
        .send()
        .await
        .expect("post comment");
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "post comment: {}",
        resp.text().await.unwrap_or_default()
    );
    let body: Value = resp.json().await.expect("post comment body");
    let comment_id = body["data"]["id"].as_str().expect("comment id").to_string();
    assert_eq!(
        body["data"]["battle_id"].as_str(),
        Some(battle_id.as_str()),
        "battle_id must echo on the created comment"
    );
    // claim_id is skip_serializing_if Option::is_none — battle-scoped
    // comments must not bleed `"claim_id":null`.
    assert!(
        body["data"].get("claim_id").is_none(),
        "battle-scoped comment must hide claim_id, got: {body}"
    );

    // ----- GET the listing and assert our comment is in there.
    let resp = client
        .get(format!("{base}/v1/battles/{battle_id}/comments"))
        .send()
        .await
        .expect("list comments");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("list body");
    let comments = body["data"].as_array().expect("comments array");
    let found = comments
        .iter()
        .any(|c| c["id"].as_str() == Some(&comment_id));
    assert!(
        found,
        "freshly-posted comment must appear in GET listing: {body}"
    );

    // ----- Unknown battle id → 404 + BATTLE_NOT_FOUND.
    let bogus = uuid::Uuid::new_v4();
    let resp = client
        .post(format!("{base}/v1/battles/{bogus}/comments"))
        .bearer_auth(&admin_token)
        .json(&json!({ "body": "should never persist" }))
        .send()
        .await
        .expect("post to bogus");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "post to bogus battle must 404"
    );
    let body: Value = resp.json().await.expect("404 body");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("BATTLE_NOT_FOUND"),
        "expected BATTLE_NOT_FOUND, got {body}"
    );

    // ----- Empty body → 400.
    let resp = client
        .post(format!("{base}/v1/battles/{battle_id}/comments"))
        .bearer_auth(&admin_token)
        .json(&json!({ "body": "" }))
        .send()
        .await
        .expect("post empty");
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "empty body must 400"
    );
}
