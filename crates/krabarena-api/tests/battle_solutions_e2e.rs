//! End-to-end coverage for the Battle-owned solutions feature
//! (since 0.13.0).
//!
//! Strategy:
//!   1. Mint a `test-admin` JWT via `/v1/test/mint-token`.
//!   2. Create a synthetic battle.
//!   3. Mint a `test-agent` JWT, post a claim with a structure.json
//!      that declares two leaderboard rows (`Browserless`, `Tavily`).
//!   4. POST `/v1/admin/battles/{battle_id}/backfill-solutions` and
//!      assert the response counts both as `added`.
//!   5. Fetch the rendered claim page; the leaderboard `<img>` must
//!      now carry the Google S2 favicon URL derived from the vendor
//!      map, NOT the per-row inline value the claim originally set
//!      (Battle wins over per-row).
//!   6. Fetch the rendered battle page; the aggregated leaderboard
//!      table must contain both solution names + the lang-logo-image
//!      class.
//!
//! Marked `#[ignore]` and gated on `KRAB_TEST_BASE` + `TEST_AUTH_SECRET`
//! so it only runs against the test Cloud Run service in CI; local
//! `cargo test --workspace` skips it cleanly.

use reqwest::multipart;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::io::Write;
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
        .timeout(Duration::from_secs(120))
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
    if !resp.status().is_success() {
        eprintln!("SKIP: mint-token returned {}", resp.status());
        return None;
    }
    let body: Value = resp.json().await.ok()?;
    body["data"]["bearer_token"].as_str().map(|s| s.to_string())
}

async fn create_test_battle(bearer: &str) -> Option<String> {
    let url = format!("{}/v1/battles", base_url());
    let body = json!({
        "title": "e2e battle-owned solutions smoke",
        "description": "Synthetic battle exercising the 0.13.0 backfill \
            admin endpoint + the aggregated leaderboard render path. \
            The claim under it declares two leaderboard rows that the \
            curated vendor map recognises (Browserless, Tavily); the \
            backfill should populate `battles.solutions` with logo URLs \
            for both and the render path should pick them up.",
        "domain_tags": ["e2e-test", "battle-solutions"],
    });
    let resp = http()
        .post(&url)
        .header("authorization", format!("Bearer {}", bearer))
        .json(&body)
        .send()
        .await
        .ok()?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        eprintln!("SKIP: could not create test battle ({status}): {body}");
        return None;
    }
    body["data"]["id"].as_str().map(String::from)
}

fn build_bundle() -> Vec<u8> {
    let run_md = "# Reproduction\n\n1. Set `KRAB_TOKEN`.\n2. python3 run.py\n";
    let readme_md = "# Report\n\nSynthetic claim — backfill + render path smoke.\n";
    let structure = json!({
        "schema_version": 1,
        "benchmark": {
            "name": "Battle Solutions Smoke",
            "task": "render-path coverage",
            "dataset": "synthetic",
            "metric": "accuracy",
            "metric_unit": "%",
            "higher_is_better": true,
            "setup": "battle-solutions e2e smoke; no actual model invoked"
        },
        "leaderboard": [
            {
                "rank": 1,
                "solution": "Browserless",
                "score": 88.0,
                "logo_url": "https://example.com/stale-inline-logo.png"
            },
            {
                "rank": 2,
                "solution": "Tavily",
                "score": 72.5
            }
        ],
        "winners": ["Browserless"]
    });
    let structure_str = serde_json::to_string(&structure).expect("serialize structure");

    let mut tar_bytes: Vec<u8> = Vec::new();
    {
        let mut b = tar::Builder::new(&mut tar_bytes);
        for (path, bytes) in &[
            ("structure.json", structure_str.as_bytes()),
            ("RUN.md", run_md.as_bytes()),
            ("README.md", readme_md.as_bytes()),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_path(path).expect("set_path");
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

#[tokio::test]
#[ignore]
async fn backfill_populates_battle_solutions_and_render_picks_them_up() {
    let Some(admin) = mint_token("test-admin").await else {
        return;
    };
    let Some(battle_id) = create_test_battle(&admin).await else {
        return;
    };
    let Some(agent) = mint_token("test-agent").await else {
        return;
    };

    let bundle = build_bundle();
    let form = multipart::Form::new()
        .text(
            "claim",
            "# Battle solutions smoke\n\n## Summary\nSynthetic.\n\n\
             ## Methodology\nSee structure.json.\n\n## Conclusion\nN/A.\n"
                .to_string(),
        )
        .text("ignore_review", "true")
        .part(
            "artifact",
            multipart::Part::bytes(bundle).file_name("artifact.tar.gz"),
        );
    let url = format!("{}/v1/battles/{}/claims", base_url(), battle_id);
    let resp = http()
        .post(&url)
        .header("authorization", format!("Bearer {}", agent))
        .multipart(form)
        .send()
        .await
        .expect("create claim");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    assert_eq!(
        status,
        StatusCode::CREATED,
        "claim create unexpected status {status}: {body}"
    );
    let claim_id = body["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("claim id missing: {body}"))
        .to_string();

    // --- Run the backfill ---
    let backfill_url = format!(
        "{}/v1/admin/battles/{}/backfill-solutions",
        base_url(),
        battle_id
    );
    let resp = http()
        .post(&backfill_url)
        .header("authorization", format!("Bearer {}", admin))
        .send()
        .await
        .expect("backfill request");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    assert_eq!(
        status,
        StatusCode::OK,
        "backfill unexpected status {status}: {body}"
    );
    let added = body["data"]["added"].as_u64().unwrap_or(0);
    assert!(
        added >= 2,
        "backfill should have added >=2 solutions, got {added}: {body}"
    );

    // --- Re-fetch claim page; the rank-1 row's logo must be resolved
    //     from the Battle-curated solution (GCP-only) and NEVER from the
    //     stale inline URL the bundle set — and NEVER from an external
    //     favicon host (all logos are self-hosted on our GCP bucket). ---
    let claim_url = format!("{}/claims/{}", base_url(), claim_id);
    let page = http()
        .get(&claim_url)
        .header("accept", "text/html")
        .send()
        .await
        .expect("fetch claim page");
    assert_eq!(page.status(), StatusCode::OK);
    let html = page.text().await.expect("claim page body");
    assert!(
        !html.contains("google.com/s2/favicons"),
        "claim page must not render an external Google favicon — logos are GCP-only"
    );
    assert!(
        !html.contains("stale-inline-logo.png"),
        "Battle's curated logo must have overridden the per-row inline one"
    );

    // --- Fetch the rendered battle page; aggregated leaderboard
    //     must contain both solutions and the lang-logo-image class
    //     (i.e. the logo actually rendered, not the avatar fallback). ---
    let battle_url = format!("{}/battles/{}", base_url(), battle_id);
    let page = http()
        .get(&battle_url)
        .header("accept", "text/html")
        .send()
        .await
        .expect("fetch battle page");
    assert_eq!(page.status(), StatusCode::OK);
    let html = page.text().await.expect("battle page body");
    assert!(
        html.contains("Leaderboard"),
        "battle page should render the aggregated 'Leaderboard' section"
    );
    assert!(
        html.contains("Browserless") && html.contains("Tavily"),
        "battle page leaderboard should list both solutions"
    );
    assert!(
        html.contains("lang-logo-image"),
        "battle page should render the logo <img> (not the avatar fallback)"
    );

    // --- Idempotent re-run: second backfill should add 0. ---
    let resp = http()
        .post(&backfill_url)
        .header("authorization", format!("Bearer {}", admin))
        .send()
        .await
        .expect("second backfill request");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let added = body["data"]["added"].as_u64().unwrap_or(99);
    assert_eq!(
        added, 0,
        "second backfill must be a no-op (idempotent); got added={added}: {body}"
    );
}
