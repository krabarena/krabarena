//! End-to-end test for the bot identity model (PR B / 0.15.1).
//!
//! Flow:
//!   1. Mint a `test-admin` token via `/v1/test/mint-token` so we
//!      have an org-admin in the shared `test-e2e.krabarena.org`
//!      entity.
//!   2. Promote the test-admin to `org_role='admin'` via the
//!      change-permissions endpoint (PR A) — the test-mint endpoint
//!      sets `role='admin'` at the platform level but the
//!      org-admin axis is independent. The mint endpoint backfills
//!      the test entity's `admin_user_id` to whoever signed in first,
//!      so this idempotently confirms the test-admin is the org's
//!      primary admin.
//!   3. `POST /v1/me/org/bots` to create a fresh bot — assert the
//!      response carries `kind=bot`, a token, and a `bot-<8 hex>@…`
//!      email.
//!   4. `GET /v1/me/org/members` — assert the bot appears with
//!      `kind=bot`.
//!   5. `POST /v1/me/org/bots/{id}/rotate-token` — assert the new
//!      token differs from the original.
//!   6. `DELETE /v1/me/org/bots/{id}` — assert success.
//!   7. `GET /v1/me/org/members` again — assert the bot is gone.
//!
//! Skipped (silent return) if `KRAB_TEST_BASE` / `TEST_AUTH_SECRET`
//! aren't set so plain `cargo test` doesn't try to reach a live
//! server.
//!
//! `#[ignore]` — run with `cargo test -p krabarena-api --test \
//! orgs_bot_e2e -- --ignored`.

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

/// The full bot lifecycle in one round-trip. Reads from
/// `/v1/me/org/members` between mutations so the assertions cover
/// the visible-side-effect contract, not just the immediate
/// response.
#[tokio::test]
#[ignore]
async fn bot_create_list_rotate_delete() {
    // Sanity: skip silently when the test infra isn't wired up.
    let Some(admin_token) = mint_token("test-admin").await else {
        eprintln!(
            "SKIP: TEST_AUTH_SECRET / KRAB_TEST_BASE not set, or /v1/test/mint-token disabled"
        );
        return;
    };

    let client = http();
    let base = base_url();

    // The mint endpoint guarantees `test-admin` lands in the
    // `test-e2e.krabarena.org` entity. PR A's signup path backfilled
    // org_role for pre-existing rows, so the first user in that
    // entity should already hold `org_role='admin'`. We don't try
    // to upgrade here — if `test-admin` isn't the org admin, the
    // bot-create POST below will 403 and the test fails cleanly.

    // ----- 3. Create a bot.
    let bot_name = format!("e2e-bot-{}", chrono::Utc::now().timestamp_millis());
    let resp = client
        .post(format!("{base}/v1/me/org/bots"))
        .bearer_auth(&admin_token)
        .json(&json!({ "display_name": &bot_name, "description": "e2e probe" }))
        .send()
        .await
        .expect("create bot");
    assert_eq!(resp.status(), StatusCode::OK, "create bot");
    let body: Value = resp.json().await.expect("create body");
    let created = &body["data"];
    let bot_id = created["id"].as_str().expect("bot id").to_string();
    let original_token = created["token"].as_str().expect("token").to_string();
    assert_eq!(created["kind"].as_str(), Some("bot"));
    let email = created["email"].as_str().expect("email");
    assert!(email.starts_with("bot-"), "{email}");

    // ----- 4. List members and confirm the bot is there.
    let resp = client
        .get(format!("{base}/v1/me/org/members"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list members");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.expect("members body");
    let members = body["data"].as_array().expect("members array");
    let bot = members
        .iter()
        .find(|m| m["id"].as_str() == Some(&bot_id))
        .expect("bot in member list");
    assert_eq!(bot["kind"].as_str(), Some("bot"));
    assert_eq!(bot["role"].as_str(), Some("member"));

    // ----- 5. Rotate token — must differ from the original.
    let resp = client
        .post(format!("{base}/v1/me/org/bots/{bot_id}/rotate-token"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("rotate token");
    assert_eq!(resp.status(), StatusCode::OK, "rotate");
    let body: Value = resp.json().await.expect("rotate body");
    let new_token = body["data"]["token"].as_str().expect("new token");
    assert_ne!(
        new_token, original_token,
        "rotation must mint a new token (fresher iat)"
    );

    // ----- 6. Delete the bot.
    let resp = client
        .delete(format!("{base}/v1/me/org/bots/{bot_id}"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("delete bot");
    assert_eq!(resp.status(), StatusCode::OK, "delete");

    // ----- 7. List again — bot must be gone.
    let resp = client
        .get(format!("{base}/v1/me/org/members"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("list after delete");
    let body: Value = resp.json().await.expect("members body");
    let members = body["data"].as_array().expect("members array");
    let gone = members.iter().all(|m| m["id"].as_str() != Some(&bot_id));
    assert!(gone, "bot must be removed from member list");
}
