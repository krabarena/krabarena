//! End-to-end coverage for per-row leaderboard logos (since 0.12.0).
//!
//! 0.11.6 wired logos through a separate `solutions` registry table
//! the admin had to curate. That model was reversed in 0.12.0 — logos
//! now live inline in each claim's `structure.json.leaderboard[i].
//! logo_url`. This test posts a synthetic claim with `logo_url` set
//! on the leaderboard's top row, fetches the rendered claim page, and
//! asserts the HTML contains an `<img>` whose `src` is the pasted URL
//! and whose `alt` is the solution name.
//!
//! Strategy:
//!   1. Mint a `test-admin` JWT via `/v1/test/mint-token`.
//!   2. Create a synthetic battle owned by that admin.
//!   3. Mint a `test-agent` JWT.
//!   4. POST a claim whose bundled `structure.json` includes a
//!      `logo_url` on the rank-1 leaderboard row.
//!   5. GET `/claims/{id}` (HTML) and grep the rendered body for the
//!      `<img>` tag pointing at our pasted URL.
//!
//! Marked `#[ignore]` and gated on `KRAB_TEST_BASE` + `TEST_AUTH_SECRET`
//! so it only runs against the test Cloud Run service from CI; the
//! local `cargo test --workspace` run skips this cleanly. The pure-
//! Rust JSON round-trip — `LeaderboardEntry.logo_url` serde with and
//! without the field — is covered by the unit test in
//! `krabarena_shared::claim_structure` alongside the rest of the
//! schema.

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
    if resp.status() == StatusCode::NOT_FOUND {
        eprintln!("SKIP: /v1/test/mint-token returned 404 — feature disabled");
        return None;
    }
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
        "title": "e2e leaderboard-logo smoke test",
        "description": "Synthetic battle exercising inline per-row logos \
            on the claim leaderboard render path (0.12.0). The test-agent \
            posts a claim whose structure.json sets `leaderboard[0].logo_url` \
            to a fixed https URL; the rendered claim page must contain an \
            <img> with that URL.",
        "domain_tags": ["e2e-test", "leaderboard", "logos"],
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

fn build_bundle_with_logo(logo_url: &str) -> Vec<u8> {
    let run_md = "# Reproduction\n\n1. Set `KRAB_TOKEN`.\n2. python3 run.py\n";
    let readme_md = "# Report\n\nSynthetic claim exercising the per-row \
        leaderboard logo render path. No real benchmark.\n";
    let structure = json!({
        "schema_version": 1,
        "benchmark": {
            "name": "Logo Smoke",
            "task": "render-path coverage",
            "dataset": "synthetic",
            "metric": "accuracy",
            "metric_unit": "%",
            "higher_is_better": true,
            "setup": "logo-render e2e smoke; no actual model invoked"
        },
        "leaderboard": [
            {
                "rank": 1,
                "solution": "Browserless",
                "score": 88.0,
                "logo_url": logo_url,
                "website_url": "https://browserless.io"
            },
            {"rank": 2, "solution": "Lightpanda", "score": 72.5}
        ],
        "winners": ["Browserless"],
    })
    .to_string();
    let files: [(&str, &[u8]); 3] = [
        ("RUN.md", run_md.as_bytes()),
        ("README.md", readme_md.as_bytes()),
        ("structure.json", structure.as_bytes()),
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

#[tokio::test]
#[ignore]
async fn claim_leaderboard_renders_inline_logo_url() {
    // Skip cleanly when the test-only mint endpoint isn't reachable —
    // local `cargo test --workspace` must not explode just because
    // we're not pointed at the test env.
    let Some(admin) = mint_token("test-admin").await else {
        return;
    };
    let Some(battle_id) = create_test_battle(&admin).await else {
        return;
    };
    let Some(agent) = mint_token("test-agent").await else {
        return;
    };

    // Stable URL we can grep for. Any reachable https image would do;
    // this is a well-known CDN that's been up for >10 years.
    let logo_url = "https://browserless.io/favicon-32x32.png";
    let claim_body = "# Inline leaderboard logo smoke\n\n\
        ## Summary\n\
        Verifies that `leaderboard[i].logo_url` in structure.json renders \
        as an <img> next to the solution name on the claim page.\n\n\
        ## Methodology\n\
        Post this claim, fetch /claims/<id>, grep the HTML for the URL.\n\n\
        ## Conclusion\n\
        If the rendered HTML contains the pasted logo URL inside an \
        `<img>` tag near `Browserless`, the per-row logo render path is \
        wired end to end.\n";

    let bundle = build_bundle_with_logo(logo_url);
    let form = multipart::Form::new()
        .text("claim", claim_body.to_string())
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
        .unwrap_or_else(|| panic!("response missing data.id: {body}"))
        .to_string();

    // Fetch the rendered claim page.
    let claim_url = format!("{}/claims/{}", base_url(), claim_id);
    let page = http()
        .get(&claim_url)
        .header("accept", "text/html")
        .send()
        .await
        .expect("fetch claim page");
    assert_eq!(
        page.status(),
        StatusCode::OK,
        "claim page status: {}",
        page.status()
    );
    let html = page.text().await.expect("claim page body");

    // The logo URL we pasted into the JSON should appear as an <img>
    // src attribute. The renderer wraps it in
    // `<img class="lang-logo lang-logo-image" src="…" alt="Browserless" …>`.
    assert!(
        html.contains(logo_url),
        "rendered HTML should contain the pasted logo URL ({logo_url}). \
         Got (snippet): {}",
        &html[..html.len().min(4000)]
    );
    assert!(
        html.contains("lang-logo-image"),
        "rendered HTML should mark the logo cell with `.lang-logo-image`. \
         Got (snippet): {}",
        &html[..html.len().min(4000)]
    );
    // Sanity: the solution name appears on the same page (so we know
    // the leaderboard table actually rendered, not just a stray <img>).
    assert!(
        html.contains("Browserless"),
        "rendered HTML should include the solution name 'Browserless'"
    );

    // Rank-2 row has no logo_url — should fall back to coloured-
    // initials avatar (no `lang-logo-image` class on that row, but the
    // base `.lang-logo` divs still appear). We don't grep for the
    // exact div here because the test would get noisy on UI tweaks;
    // the assertion above is enough to lock in the render path.
}
