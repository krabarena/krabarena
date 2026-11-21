//! End-to-end test for the generic `POST /v1/images` upload endpoint.
//!
//! Asserts the round trip: send a tiny PNG, get back
//! `{ url, suggested_alt, markdown }`, then fetch the returned URL
//! and verify it carries the PNG magic bytes (so the upload actually
//! landed on the public bucket, not just a hopeful redirect).
//!
//! Marked `#[ignore]` and gated on `KRAB_TEST_BASE` so it only runs
//! against the test Cloud Run service from CI. Skips cleanly when
//! `TEST_AUTH_SECRET` isn't set so the local `cargo test` run
//! doesn't explode.

use reqwest::multipart;
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
        .timeout(Duration::from_secs(120))
        .build()
        .expect("build http client")
}

/// Tiny but real PNG: 1×1 transparent. `infer` only sniffs the first
/// ~12 bytes (the PNG signature + IHDR), so this passes the MIME
/// check without resorting to a heavier fixture.
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

#[tokio::test]
#[ignore]
async fn upload_image_returns_url_alt_and_markdown_snippet() {
    let Some(agent) = mint_token("test-agent").await else {
        return;
    };

    let form = multipart::Form::new().part(
        "image",
        multipart::Part::bytes(TINY_PNG.to_vec()).file_name("benchmark.png"),
    );

    let url = format!("{}/v1/images", base_url());
    let resp = http()
        .post(&url)
        .header("authorization", format!("Bearer {}", agent))
        .multipart(form)
        .send()
        .await
        .expect("POST /v1/images");
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    assert_eq!(
        status,
        StatusCode::CREATED,
        "POST /v1/images unexpected status {status}: {body}"
    );

    // Response shape: { url, suggested_alt, markdown }.
    let url = body["data"]["url"]
        .as_str()
        .unwrap_or_else(|| panic!("response missing data.url: {body}"))
        .to_string();
    let suggested_alt = body["data"]["suggested_alt"]
        .as_str()
        .unwrap_or_else(|| panic!("response missing data.suggested_alt: {body}"));
    let markdown = body["data"]["markdown"]
        .as_str()
        .unwrap_or_else(|| panic!("response missing data.markdown: {body}"));

    assert!(
        url.contains("/images/") && url.ends_with(".png"),
        "url should point to images/<sha>.png — got: {url}"
    );
    assert!(
        !suggested_alt.is_empty(),
        "suggested_alt should never be empty — got: {suggested_alt:?}"
    );
    // The ready-to-paste snippet should be a well-formed markdown
    // image reference pointing at the same URL the response surfaced.
    assert!(
        markdown.starts_with("![") && markdown.contains(&url),
        "markdown snippet missing alt or url — got: {markdown}"
    );

    // Fetch the URL and check the magic bytes — confirms the upload
    // actually landed on the bucket, not just a hopeful response.
    let bytes = http()
        .get(&url)
        .send()
        .await
        .expect("fetch uploaded image")
        .bytes()
        .await
        .expect("read fetched bytes");
    assert!(
        bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "fetched bytes don't start with PNG magic — got first 8 bytes: {:?}",
        &bytes.iter().take(8).copied().collect::<Vec<_>>()
    );
}

#[tokio::test]
#[ignore]
async fn upload_image_rejects_unauthenticated() {
    // Skip if the test env isn't set up.
    if test_auth_secret().is_none() {
        eprintln!("SKIP: TEST_AUTH_SECRET not set");
        return;
    }
    let form = multipart::Form::new().part(
        "image",
        multipart::Part::bytes(TINY_PNG.to_vec()).file_name("x.png"),
    );

    let url = format!("{}/v1/images", base_url());
    let resp = http()
        .post(&url)
        .multipart(form)
        .send()
        .await
        .expect("POST /v1/images (unauth)");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated POST /v1/images should be 401"
    );
}
