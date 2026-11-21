//! End-to-end test for standalone Claims (migration 035).
//!
//! Exercises the full lifecycle:
//!   1. Post a standalone Claim via the CLI flow (`POST /v1/claims`
//!      with multipart `topic` + `topic_tags`).
//!   2. Assert it lands with `battle_id IS NULL`, `visibility =
//!      'private'`, and that anon GETs return 404.
//!   3. Admin-promote to public.
//!   4. Assert it shows up on `GET /v1/claims/standalone`.
//!   5. Adopt into the Browsers battle.
//!   6. Assert it's now in the battle's claim list and no longer on
//!      `/v1/claims/standalone`.
//!
//! Marked `#[ignore]` and gated on `KRAB_TEST_BASE` + `TEST_AUTH_SECRET`
//! so the unit `cargo test` run doesn't try to hit a live server.
//!
//! Run with `cargo test -p krabarena-api --test standalone_claims_e2e \
//! -- --ignored` after setting both env vars.

use reqwest::multipart;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::io::Write;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://krabarena.com";
/// Browsers Battle — a real, public Battle with claims already
/// attached. Adoption into it is a non-destructive move for the
/// purposes of this test (we use a unique title every run so we
/// can't collide with someone else's content).
const ADOPT_TARGET_BATTLE: &str = "fcc1740b-3a0e-4b1a-9c5e-0e10b00e0c4f";

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
        .timeout(Duration::from_secs(120))
        .build()
        .expect("build http client")
}

/// Smallest valid bundle the reviewer accepts: README.md + RUN.md +
/// structure.json. Gemini doesn't see the artifact in detail at this
/// size, so the warn/pass verdict comes back fast.
fn build_bundle(topic: &str) -> Vec<u8> {
    let run_md = format!(
        "# Reproduction\n\n\
         Synthetic standalone-claims e2e probe.\n\n\
         ## Steps\n\n\
         1. Visit krabarena.com.\n\
         2. Observe that claim posted standalone with topic: {topic}.\n",
    );
    let readme_md = format!(
        "# Report\n\n\
         ## Summary\n\nThis is a synthetic standalone claim e2e probe.\n\n\
         ## Methodology\n\nRunning the standalone_claims_e2e test fixture against \
         topic: {topic}.\n\n\
         ## Conclusion\n\nIf you can read this, the probe ran.\n"
    );
    let structure_json = r#"{
        "schema_version": 1,
        "benchmark": {
            "name": "synthetic",
            "task": "e2e_smoke",
            "dataset": "n/a",
            "metric": "passed",
            "higher_is_better": true,
            "setup": "synthetic"
        },
        "leaderboard": [
            {"rank": 1, "solution": "test-agent", "score": 1.0, "notes": "ran"}
        ],
        "winners": ["test-agent"],
        "cost": {"tokens": 100, "usd": 0.005}
    }"#;
    let files: [(&str, &[u8]); 3] = [
        ("RUN.md", run_md.as_bytes()),
        ("README.md", readme_md.as_bytes()),
        ("structure.json", structure_json.as_bytes()),
    ];
    let mut tar_bytes = Vec::new();
    {
        let mut b = tar::Builder::new(&mut tar_bytes);
        for (path, bytes) in files.iter() {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, path, *bytes).expect("append");
        }
        b.finish().expect("finish tar");
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&tar_bytes).expect("gz write");
    gz.finish().expect("gz finish")
}

async fn mint_token(user: &str) -> Option<(String, String)> {
    let Some(secret) = test_auth_secret() else {
        eprintln!("SKIP: TEST_AUTH_SECRET not set");
        return None;
    };
    let url = format!("{}/v1/test/mint-token", base_url());
    let resp = http()
        .post(&url)
        .header("x-test-auth", secret)
        .json(&json!({ "user": user, "role": "user" }))
        .send()
        .await
        .expect("mint-token request failed");
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        eprintln!("SKIP: /v1/test/mint-token returned 404 — feature disabled on server?");
        return None;
    }
    let body: Value = resp.json().await.expect("mint-token returned non-JSON");
    let token = body["data"]["bearer_token"]
        .as_str()
        .expect("bearer_token missing")
        .to_string();
    let user_id = body["data"]["user_id"]
        .as_str()
        .unwrap_or("test-agent")
        .to_string();
    Some((token, user_id))
}

async fn mint_admin_token() -> Option<String> {
    let Some(secret) = test_auth_secret() else {
        return None;
    };
    let url = format!("{}/v1/test/mint-token", base_url());
    let resp = http()
        .post(&url)
        .header("x-test-auth", secret)
        .json(&json!({ "user": "admin-test-agent", "role": "admin" }))
        .send()
        .await
        .expect("mint admin");
    if !resp.status().is_success() {
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["data"]["bearer_token"].as_str().map(String::from)
}

#[tokio::test]
#[ignore]
async fn standalone_claim_lifecycle_private_then_promote_then_adopt() {
    let Some((bearer, _user_id)) = mint_token("standalone-e2e-author").await else {
        return;
    };

    let topic = format!(
        "Standalone e2e probe @ {}",
        chrono::Utc::now().timestamp_millis()
    );
    let bundle = build_bundle(&topic);
    let claim_text = format!(
        "# Standalone e2e — {}\n\n\
         ## Summary\n\nSynthetic standalone claim probe.\n\n\
         ## Methodology\n\nRuns through the standalone-claim CLI flow.\n\n\
         ## Conclusion\n\nIf the test reaches here, the probe ran end-to-end.\n",
        chrono::Utc::now().timestamp_millis()
    );

    // --- 1. Post the standalone claim ---
    let form = multipart::Form::new()
        .text("claim", claim_text.clone())
        .text("topic", topic.clone())
        .text("topic_tags", "e2e-test,standalone")
        .text("agent_backend", "krabarena-e2e")
        .text("agent_model", "synthetic-standalone")
        .text("ignore_review", "true")
        .part(
            "artifact",
            multipart::Part::bytes(bundle)
                .file_name("standalone-probe.tar.gz")
                .mime_str("application/gzip")
                .unwrap(),
        );
    let resp = http()
        .post(format!("{}/v1/claims", base_url()))
        .header("authorization", format!("Bearer {}", bearer))
        .multipart(form)
        .send()
        .await
        .expect("post standalone");
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .expect("post standalone returned non-JSON");
    assert!(
        status == StatusCode::CREATED,
        "expected 201 CREATED on standalone post, got {status}: {body}"
    );
    let claim_id = body["data"]["id"].as_str().expect("id").to_string();
    let claim_slug = body["data"]["slug"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| claim_id.clone());
    assert!(
        body["data"]["battle_id"].is_null(),
        "standalone claim must have null battle_id, got {body}"
    );
    assert_eq!(
        body["data"]["visibility"].as_str(),
        Some("private"),
        "standalone claim must default to visibility='private': {body}"
    );
    assert_eq!(
        body["data"]["topic"].as_str(),
        Some(topic.as_str()),
        "topic should round-trip"
    );

    // --- 2. Anon fetch should 404 (private) ---
    let resp = http()
        .get(format!("{}/v1/claims/{}", base_url(), claim_slug))
        .send()
        .await
        .expect("anon get");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "private standalone claim must 404 to anonymous callers"
    );

    // --- 3. Admin-promote to public ---
    let Some(admin_token) = mint_admin_token().await else {
        eprintln!("SKIP: admin mint unavailable — stopping after private-gate assertion");
        return;
    };
    let resp = http()
        .post(format!(
            "{}/v1/admin/claims/{}/promote",
            base_url(),
            claim_id
        ))
        .header("authorization", format!("Bearer {}", admin_token))
        .json(&json!({"visibility": "public"}))
        .send()
        .await
        .expect("promote");
    assert!(
        resp.status().is_success(),
        "promote should succeed: {}",
        resp.status()
    );

    // Anon fetch should now work.
    let resp = http()
        .get(format!("{}/v1/claims/{}", base_url(), claim_slug))
        .send()
        .await
        .expect("anon get after promote");
    assert_eq!(resp.status(), StatusCode::OK);

    // --- 4. Listed on /v1/claims/standalone ---
    let resp = http()
        .get(format!("{}/v1/claims/standalone", base_url()))
        .send()
        .await
        .expect("list standalone");
    let body: Value = resp.json().await.expect("list standalone JSON");
    let appeared = body["data"]
        .as_array()
        .map(|arr| arr.iter().any(|c| c["id"].as_str() == Some(&claim_id)))
        .unwrap_or(false);
    assert!(
        appeared,
        "newly-promoted standalone claim should be listed: {body}"
    );

    // --- 5. Adopt into the Browsers battle ---
    let resp = http()
        .post(format!("{}/v1/claims/{}/adopt", base_url(), claim_id))
        .header("authorization", format!("Bearer {}", bearer))
        .json(&json!({"battle_id": ADOPT_TARGET_BATTLE}))
        .send()
        .await
        .expect("adopt");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    assert!(
        status.is_success(),
        "adopt should succeed, got {status}: {body}"
    );
    let claim_after = &body["data"]["claim"];
    assert_eq!(
        claim_after["battle_id"].as_str(),
        Some(ADOPT_TARGET_BATTLE),
        "battle_id should be set on adopted claim: {body}"
    );
    assert_eq!(
        claim_after["visibility"].as_str(),
        Some("adopted"),
        "visibility should flip to 'adopted': {body}"
    );

    // --- 6. No longer on the standalone list ---
    let resp = http()
        .get(format!("{}/v1/claims/standalone", base_url()))
        .send()
        .await
        .expect("list standalone again");
    let body: Value = resp.json().await.expect("list standalone JSON");
    let still_there = body["data"]
        .as_array()
        .map(|arr| arr.iter().any(|c| c["id"].as_str() == Some(&claim_id)))
        .unwrap_or(false);
    assert!(
        !still_there,
        "adopted claim must no longer be on /v1/claims/standalone"
    );

    // --- 7. Idempotent re-adoption into the same battle ---
    let resp = http()
        .post(format!("{}/v1/claims/{}/adopt", base_url(), claim_id))
        .header("authorization", format!("Bearer {}", bearer))
        .json(&json!({"battle_id": ADOPT_TARGET_BATTLE}))
        .send()
        .await
        .expect("re-adopt same battle");
    assert!(
        resp.status().is_success(),
        "re-adoption into the same battle should be a no-op success"
    );
}
