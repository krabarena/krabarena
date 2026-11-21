//! End-to-end test for the inline-media-in-claim pipeline.
//!
//! Mints a token via `/v1/test/mint-token`, posts a claim whose body
//! contains `![demo](demo.png)` against a tar.gz that has a tiny PNG at
//! that path, and asserts:
//!
//! 1. The claim was created (HTTP 201).
//! 2. The claim's body in the response no longer contains `demo.png`,
//!    and instead contains a `https://storage.googleapis.com/krabarena-artifacts/media/sha256:`
//!    URL.
//! 3. That URL serves the PNG with `content-type: image/png`.
//!
//! Marked `#[ignore]` and gated on `KRAB_TEST_BASE` so it only runs
//! against the test Cloud Run service from CI. Like `review_e2e`, the
//! test skips cleanly when `TEST_AUTH_SECRET` isn't set so the local
//! `cargo test` run doesn't explode.
//!
//! The rejection path (Vision blocking adult / violence) is covered in
//! the unit tests — maintaining a "slightly racy" production fixture
//! would be more pain than signal.

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

/// Tiny but real PNG: 1×1 transparent. `infer` only sniffs the first
/// ~12 bytes (the PNG signature + IHDR length / chunk type), so this
/// passes the MIME check.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
    0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R', // IHDR chunk
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
    0x08, 0x06, 0x00, 0x00, 0x00, // bit depth / color / etc
    0x1f, 0x15, 0xc4, 0x89, // CRC
    0x00, 0x00, 0x00, 0x0a, // IDAT length
    b'I', b'D', b'A', b'T', 0x78, 0x9c, 0x62, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0xe5, 0x27, 0xde,
    0xfc, // IDAT
    0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82, // IEND
];

fn build_bundle() -> Vec<u8> {
    // Minimal viable artifact: RUN.md, README.md, and the PNG. The
    // KrabReviewer pre-filter requires RUN.md / README.md at the bundle
    // root; without them the claim is blocked before media processing
    // even runs.
    let run_md = "# Reproduction\n\n\
        Open `demo.png` to see the embedded image. The screenshot is \
        included so reviewers can sanity-check the experimental setup \
        without re-running the script.\n";
    let readme_md = "# Report\n\n\
        Demonstrates that the KrabArena pipeline correctly extracts and \
        republishes inline images embedded in claim reports. The PNG \
        bundled here is a 1×1 placeholder.\n";
    // structure.json is required at the bundle root since 0.18.7 — the
    // pre-filter declines (non-overridably) without a valid leaderboard,
    // so even this media-focused smoke test must ship one.
    let structure_json = br#"{
        "schema_version": 1,
        "benchmark": {
            "name": "Inline image pipeline", "task": "Republish bundled images to the CDN",
            "dataset": "1 bundled 1x1 PNG", "metric": "images_rewritten",
            "metric_unit": "count", "higher_is_better": true,
            "setup": "Post a claim embedding demo.png; assert the CDN URL appears"
        },
        "leaderboard": [{"rank": 1, "solution": "KrabArena media pipeline", "score": 1.0}],
        "winners": ["KrabArena media pipeline"]
    }"#;
    let files: [(&str, &[u8]); 4] = [
        ("RUN.md", run_md.as_bytes()),
        ("README.md", readme_md.as_bytes()),
        ("structure.json", structure_json),
        ("demo.png", TINY_PNG),
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

async fn mint_token(user: &str) -> Option<String> {
    let Some(secret) = test_auth_secret() else {
        eprintln!("SKIP: TEST_AUTH_SECRET not set");
        return None;
    };
    let url = format!("{}/v1/test/mint-token", base_url());
    let resp = http()
        .post(&url)
        .header("x-test-auth", secret)
        .json(&json!({ "user": user }))
        .send()
        .await
        .expect("mint-token request failed");
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        eprintln!("SKIP: /v1/test/mint-token returned 404 — feature disabled on server?");
        return None;
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        panic!("mint-token unexpected status {status}: {body}");
    }
    let body: Value = resp.json().await.expect("mint-token returned non-JSON");
    body["data"]["bearer_token"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| {
            panic!("mint-token response missing data.bearer_token: {body}");
        })
}

/// Create a fresh test battle so each run is isolated. We can't reuse
/// the `review_e2e.rs` battle constant because that exists on prod;
/// this test runs against the test environment where battles are
/// per-run.
async fn create_test_battle(bearer: &str) -> Option<String> {
    let url = format!("{}/v1/battles", base_url());
    let body = json!({
        "title": "e2e media-in-claims smoke test",
        "description": "Synthetic battle exercising the inline-image pipeline. \
            A test-agent claim against this battle embeds a 1×1 PNG via \
            `![demo](demo.png)` and asserts the public CDN URL appears in \
            the persisted body.",
        "domain_tags": ["e2e-test", "media"],
    });
    let resp = http()
        .post(&url)
        .header("authorization", format!("Bearer {}", bearer))
        .json(&body)
        .send()
        .await
        .expect("create battle");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        eprintln!("SKIP: could not create test battle ({status}): {body}");
        return None;
    }
    body["data"]["id"].as_str().map(String::from)
}

#[tokio::test]
#[ignore]
async fn claim_with_inline_image_publishes_public_cdn_url() {
    // mint-token is the gating signal: without it nothing in this
    // file can run, so we skip cleanly rather than fail.
    let Some(admin) = mint_token("test-admin").await else {
        return;
    };
    let Some(battle_id) = create_test_battle(&admin).await else {
        return;
    };
    let Some(agent) = mint_token("test-agent").await else {
        return;
    };

    let claim_body = "# Inline image smoke test\n\n\
        ## Summary\n\
        This claim embeds a 1x1 PNG bundled inside the artifact at \
        `demo.png`. The pipeline should rewrite the body to point at a \
        public CDN URL.\n\n\
        ![demo](demo.png)\n\n\
        ## Methodology\n\
        Run the included artifact; the demo image is a 1x1 transparent PNG.\n\n\
        ## Conclusion\n\
        If the rewritten body contains a `https://storage.googleapis.com/...media/sha256:` \
        URL where the original `demo.png` reference was, the pipeline works.\n";

    let bundle = build_bundle();
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

    let stored = body["data"]["claim"]
        .as_str()
        .or_else(|| body["data"]["claim_body"].as_str())
        .unwrap_or_else(|| panic!("response missing claim/claim_body: {body}"));
    assert!(
        !stored.contains("(demo.png)"),
        "rewritten body still contains the bundle path: {stored}"
    );
    assert!(
        stored.contains("https://storage.googleapis.com/krabarena-artifacts/media/sha256:"),
        "rewritten body should contain a public CDN URL: {stored}"
    );

    // Pull the URL out of the body and fetch it; the file should be
    // publicly readable with the right content-type.
    let re = regex::Regex::new(
        r"https://storage\.googleapis\.com/krabarena-artifacts/media/sha256:[0-9a-f]+\.(png|jpg|webp|gif)",
    )
    .expect("static regex");
    let public_url = re
        .find(stored)
        .map(|m| m.as_str().to_string())
        .expect("could not extract public URL from rewritten body");

    let img_resp = http()
        .get(&public_url)
        .send()
        .await
        .expect("fetch public URL");
    assert_eq!(
        img_resp.status(),
        StatusCode::OK,
        "public CDN URL did not return 200: {}",
        img_resp.status()
    );
    let ct = img_resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(
        ct.starts_with("image/png"),
        "expected image/png content-type, got {ct}"
    );
}
