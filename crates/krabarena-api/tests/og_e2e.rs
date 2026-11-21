//! End-to-end tests for per-entity OpenGraph meta + PNG card routes.
//!
//! Verifies that:
//!   - `/battles/{slug}` HTML contains `<meta property="og:title">`
//!     pointing at the battle's actual title.
//!   - `/battles/{slug}` HTML contains `<meta property="og:image">`
//!     whose URL points at `/og/battles/{uuid}`.
//!   - `GET /og/battles/{uuid}` returns `Content-Type: image/png`
//!     and a body starting with the PNG magic bytes.
//!   - Same shape holds for `/claims/{...}` / `/og/claims/{...}`.
//!
//! Marked `#[ignore]` so they only run via
//!   `cargo test -p krabarena-api --test og_e2e -- --ignored`
//! (mirrors `review_e2e.rs` / `redirect_e2e.rs`). Default target is
//! prod; override with `KRAB_TEST_BASE=https://krabarena-api-…` to
//! run against the test Cloud Run service.

use reqwest::{Client, StatusCode};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://krabarena.com";
/// "What is the best Web Access API for AI Agents?" — same prod
/// battle the review_e2e suite uses. Lives at a stable slug + UUID.
const TEST_BATTLE_UUID: &str = "09ab746f-52fd-44e2-9db7-71fa00e63bd7";
const TEST_BATTLE_SLUG: &str = "what-is-the-best-web-access-api-for-ai-agents";

fn base_url() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn http() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build http client")
}

/// PNG signature — `89 50 4E 47 0D 0A 1A 0A`. Every well-formed
/// PNG file (and HTTP body) starts with these 8 bytes regardless of
/// dimensions / colour space / encoder.
const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

#[tokio::test]
#[ignore]
async fn battle_page_emits_per_entity_og_meta() {
    let url = format!("{}/battles/{}", base_url(), TEST_BATTLE_SLUG);
    let resp = http().get(&url).send().await.expect("GET battle page");
    assert_eq!(resp.status(), StatusCode::OK, "battle page should 200");
    let html = resp.text().await.expect("read body");

    // The og:title tag should carry the battle's actual title (not
    // the static "KrabArena" default). We only check a stable
    // substring so prompt-driven title edits don't break the test.
    assert!(
        html.contains(r#"<meta property="og:title" content="What is the best Web Access API"#),
        "expected per-battle og:title in HTML; got: {}",
        snippet(&html, "og:title")
    );

    // The og:image should point at the per-entity card route. We
    // match the UUID-form because that's what the handler emits
    // (slug-form would also be valid; lock in the current
    // implementation).
    let expected_og_img = format!("/og/battles/{}", TEST_BATTLE_UUID);
    assert!(
        html.contains(&expected_og_img),
        "expected og:image URL containing {expected_og_img}; got: {}",
        snippet(&html, "og:image")
    );

    // Twitter card variant should be the large summary card so the
    // 1200x630 image is honoured by X / Twitter previews.
    assert!(
        html.contains(r#"<meta name="twitter:card" content="summary_large_image">"#),
        "expected twitter:card=summary_large_image"
    );
}

#[tokio::test]
#[ignore]
async fn battle_og_card_returns_png_image() {
    let url = format!("{}/og/battles/{}", base_url(), TEST_BATTLE_UUID);
    let resp = http().get(&url).send().await.expect("GET og battle PNG");
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert_eq!(ct, "image/png", "Content-Type should be image/png");

    let bytes = resp.bytes().await.expect("read body");
    assert!(
        bytes.starts_with(PNG_MAGIC),
        "response body should begin with PNG magic bytes; got first 8: {:02x?}",
        &bytes[..bytes.len().min(8)]
    );
    // Sanity check — a 1200x630 card encodes to well over 5 KB even
    // on best-case compression. Less than that means the body is
    // probably an error stub.
    assert!(
        bytes.len() > 5_000,
        "PNG body unexpectedly small: {} bytes",
        bytes.len()
    );
}

#[tokio::test]
#[ignore]
async fn claim_og_card_returns_png_for_known_claim() {
    // We don't pin a specific claim UUID — claims rotate, and the
    // route returns the static fallback for unknown IDs anyway. So
    // this exercises the routing + content-type contract regardless
    // of whether the test env has the specific claim.
    //
    // Use a deterministic UUID; the handler must return 200 PNG
    // (either generated or the bundled fallback). 404 here would
    // mean the route is wired wrong.
    let url = format!(
        "{}/og/claims/00000000-0000-0000-0000-000000000000",
        base_url()
    );
    let resp = http().get(&url).send().await.expect("GET og claim PNG");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "OG endpoint should never 404"
    );
    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert_eq!(ct, "image/png");
    let bytes = resp.bytes().await.expect("read body");
    assert!(
        bytes.starts_with(PNG_MAGIC),
        "fallback or generated PNG body should start with PNG magic; got first 8: {:02x?}",
        &bytes[..bytes.len().min(8)]
    );
}

/// `/og-html/battles/{uuid}` returns a 200 HTML page that carries the
/// battle title, the brand header, the self-hosted Inter @font-face
/// (NOT Google Fonts), and the leaderboard scaffold. This is the page
/// the `og-render` headless-Chromium service screenshots.
#[tokio::test]
#[ignore]
async fn battle_og_html_renders_leaderboard_card() {
    let url = format!("{}/og-html/battles/{}", base_url(), TEST_BATTLE_UUID);
    let resp = http().get(&url).send().await.expect("GET og-html battle");
    assert_eq!(resp.status(), StatusCode::OK, "og-html battle should 200");
    let html = resp.text().await.expect("read body");

    assert!(
        html.contains("What is the best Web Access API"),
        "expected battle title in og-html card; got: {}",
        snippet(&html, "<h1")
    );
    assert!(html.contains("BATTLE"), "expected the BATTLE kind pill");
    // Self-hosted font — the render service may be network-restricted,
    // so the card must not depend on Google Fonts at render time.
    assert!(
        html.contains("/static/fonts/Inter-Black.ttf"),
        "expected self-hosted Inter @font-face"
    );
    assert!(
        !html.contains("fonts.googleapis.com"),
        "card must NOT pull fonts from Google at render time"
    );
    // The leaderboard scaffold (rows or the empty-state) and the
    // footer stats are always present.
    assert!(
        html.contains(r#"class="lb""#) || html.contains("No claims yet"),
        "expected leaderboard list or empty-state"
    );
    assert!(html.contains("krabarena.com"), "expected footer domain");
}

/// `/og-html/claims/{uuid}` for an unknown claim 404s (so the render
/// service can tell "no card" from a transient error). Mirrors the
/// PNG route's never-404 contract only applying to `/og/...`; the
/// og-html template route is allowed to 404.
#[tokio::test]
#[ignore]
async fn claim_og_html_404s_for_unknown_claim() {
    let url = format!(
        "{}/og-html/claims/00000000-0000-0000-0000-000000000000",
        base_url()
    );
    let resp = http().get(&url).send().await.expect("GET og-html claim");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "og-html for an unknown claim should 404"
    );
}

/// Grab a short window around `needle` so an assertion failure has
/// useful context. Hidden test helper; returns "(no match)" if the
/// substring isn't there at all.
fn snippet(haystack: &str, needle: &str) -> String {
    let Some(idx) = haystack.find(needle) else {
        return "(no match)".into();
    };
    let start = idx.saturating_sub(20);
    let end = (idx + needle.len() + 200).min(haystack.len());
    haystack[start..end].to_string()
}
