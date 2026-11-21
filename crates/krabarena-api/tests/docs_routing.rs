//! Integration tests for `/docs/{name}` content negotiation + safety.
//!
//! Drops the per-document Rust routes in favour of a single handler
//! that reads from the `docs/` folder and content-negotiates HTML vs
//! raw markdown. These tests pin the contract that came out of that
//! refactor:
//!
//! 1. Browser UA on `/docs/proxy-spec` → 200, `text/html`, page title
//!    contains "Battle Proxy Spec".
//! 2. curl UA on `/docs/proxy-spec` → 200, `text/markdown`, body starts
//!    with `# Battle Proxy Spec`.
//! 3. Browser UA on `/docs/claim-structure` → `text/html`.
//! 4. `/docs/claim-structure.md` (any UA) → `text/markdown` (the `.md`
//!    extension overrides UA negotiation).
//! 5. `/docs/nope` → 404.
//! 6. Path-traversal attempts (`..` segments) → 404.
//!
//! `#[ignore]`d by default so a plain `cargo test` skips them. Run:
//!
//!   KRAB_TEST_BASE=http://127.0.0.1:3100 \
//!     cargo test -p krabarena-api --test docs_routing -- --ignored
//!
//! Server must be 0.4.7+ (when this handler shipped) and must have
//! the `docs/` folder on its working directory — the binary reads
//! markdown from disk on every request.

use reqwest::header::USER_AGENT;
use reqwest::{Client, StatusCode};
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3100";

const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";
const CURL_UA: &str = "curl/8.6.0";

// `/docs` index (added 0.9.6). Pins the curation contract — only the
// allowlisted public docs appear on the listing, internal runbooks
// like /docs/og-render stay reachable by direct URL but stay off
// the index. Both content-negotiated variants must work.
#[tokio::test]
#[ignore]
async fn docs_index_browser_returns_html_with_public_docs() {
    let resp = client()
        .get(format!("{}/docs", base()))
        .header(USER_AGENT, BROWSER_UA)
        .send()
        .await
        .expect("GET /docs");
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/html"), "want html, got {ct}");
    let body = resp.text().await.expect("body");
    for stem in [
        "proxy-spec",
        "battle-structure",
        "claim-structure",
        "battles",
        "containerised-battles",
    ] {
        assert!(
            body.contains(&format!("/docs/{stem}")),
            "missing public doc link /docs/{stem}"
        );
    }
    for stem in ["og-render", "ui", "admin-cli"] {
        assert!(
            !body.contains(&format!("/docs/{stem}")),
            "internal doc /docs/{stem} leaked into /docs index"
        );
    }
    // The glossary is the page's human-facing intro, rendered inline at
    // the top of the index — not a listed "doc". Its term headings get
    // markdown anchor ids (`<h2 id="battle">…`) that only exist when the
    // glossary markdown is rendered inline — reference-doc rows never
    // produce them. It must NOT appear as a `/docs/glossary` row/link
    // either (that would be the duplicate we deliberately removed).
    assert!(
        body.contains("id=\"battle\"") && body.contains("id=\"proxy\""),
        "glossary terms not inlined on /docs index"
    );
    assert!(
        !body.contains("/docs/glossary"),
        "glossary should not be a listed/linked doc — it's inlined"
    );
}

#[tokio::test]
#[ignore]
async fn docs_index_markdown_returns_link_list() {
    let resp = client()
        .get(format!("{}/docs", base()))
        .header(USER_AGENT, CURL_UA)
        .send()
        .await
        .expect("GET /docs md");
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/markdown"), "want markdown, got {ct}");
    let body = resp.text().await.expect("md body");
    assert!(body.starts_with("# KrabArena docs"), "has heading");
    assert!(
        body.contains("[Battle Proxy Spec](/docs/proxy-spec)"),
        "proxy-spec entry"
    );
    assert!(
        !body.contains("/docs/og-render"),
        "internal doc leaked into markdown index"
    );
}

#[tokio::test]
#[ignore]
async fn internal_doc_still_reachable_by_direct_url() {
    // Curation only affects the index — direct links remain live.
    let resp = client()
        .get(format!("{}/docs/og-render", base()))
        .header(USER_AGENT, BROWSER_UA)
        .send()
        .await
        .expect("GET /docs/og-render");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "internal docs must keep serving by direct URL"
    );
}

fn base() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build http client")
}

async fn fetch_with_ua(path: &str, ua: &str) -> (StatusCode, String, String) {
    let resp = client()
        .get(format!("{}{}", base(), path))
        .header(USER_AGENT, ua)
        .send()
        .await
        .expect("HTTP request failed");
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp.text().await.unwrap_or_default();
    (status, content_type, body)
}

#[tokio::test]
#[ignore]
async fn browser_get_proxy_spec_renders_html_in_site_chrome() {
    let (status, ct, body) = fetch_with_ua("/docs/proxy-spec", BROWSER_UA).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.starts_with("text/html"), "expected text/html, got: {ct}");
    // Site chrome — the doc renders inside base.html.
    assert!(
        body.contains("<title>Battle Proxy Spec — KrabArena</title>"),
        "expected page title 'Battle Proxy Spec — KrabArena' in body"
    );
    // The actual doc heading is also rendered. Since 0.4.7 the
    // markdown renderer injects GFM-style heading anchors
    // (`<h1 id="battle-proxy-spec">`) so we match the closing tag +
    // text rather than the bare opening — the anchor id form is
    // semantically equivalent.
    assert!(
        body.contains("Battle Proxy Spec</h1>"),
        "expected rendered Battle Proxy Spec heading. got body:\n{}",
        &body[..body.len().min(2000)]
    );
    // Site nav comes through (proves base.html wrapped the content).
    assert!(
        body.contains("KrabArena"),
        "site chrome (KrabArena nav) should be present"
    );
}

#[tokio::test]
#[ignore]
async fn curl_get_proxy_spec_returns_raw_markdown() {
    let (status, ct, body) = fetch_with_ua("/docs/proxy-spec", CURL_UA).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ct.starts_with("text/markdown"),
        "expected text/markdown, got: {ct}"
    );
    // Raw markdown — starts with the `# Title` heading, not HTML.
    assert!(
        body.trim_start().starts_with("# Battle Proxy Spec"),
        "expected raw markdown, got: {}",
        &body[..body.len().min(200)]
    );
    assert!(
        !body.contains("<title>"),
        "raw markdown response should not contain HTML <title>"
    );
}

#[tokio::test]
#[ignore]
async fn browser_get_claim_structure_returns_html() {
    let (status, ct, _body) = fetch_with_ua("/docs/claim-structure", BROWSER_UA).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ct.starts_with("text/html"),
        "expected text/html for browser UA, got: {ct}"
    );
}

#[tokio::test]
#[ignore]
async fn explicit_md_extension_always_returns_markdown() {
    // `.md` extension wins over UA negotiation — even a browser UA
    // gets raw markdown when it explicitly asks for the `.md` URL.
    let (status, ct, body) = fetch_with_ua("/docs/claim-structure.md", BROWSER_UA).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        ct.starts_with("text/markdown"),
        "expected text/markdown for `.md` URL, got: {ct}"
    );
    assert!(
        body.trim_start().starts_with("# Claim Structure"),
        "expected raw `# Claim Structure` markdown body"
    );
    assert!(
        !body.contains("<title>"),
        "`.md` URL should never return HTML"
    );
}

#[tokio::test]
#[ignore]
async fn unknown_doc_returns_404() {
    let (status, _ct, _body) =
        fetch_with_ua("/docs/this-doc-definitely-does-not-exist", CURL_UA).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// The glossary lives only inline on the `/docs` index — its source moved
// out of `docs/` into `content/glossary.md` (compiled in), so the old
// standalone page must 404. Guards against the duplicate-presentation
// regression: glossary on the page AND as a separate doc.
#[tokio::test]
#[ignore]
async fn glossary_has_no_standalone_page() {
    let (status, _ct, _body) = fetch_with_ua("/docs/glossary", CURL_UA).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status_md, _ct, _body) = fetch_with_ua("/docs/glossary.md", CURL_UA).await;
    assert_eq!(status_md, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore]
async fn path_traversal_attempts_return_404() {
    // Reqwest's URL parser would normalize literal `..` into a higher
    // path segment, so we test by URL-encoding the `..` so it reaches
    // the handler intact, plus a few other shapes the handler
    // explicitly rejects (slashes, leading dot).
    let traversal_paths = [
        "/docs/..%2Fetc%2Fpasswd", // URL-encoded `../etc/passwd`
        "/docs/.hidden",           // leading dot
        "/docs/sub%2Fdir",         // slash inside the name
    ];
    for p in &traversal_paths {
        let (status, _ct, _body) = fetch_with_ua(p, CURL_UA).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "expected 404 for traversal-shaped path {p}, got {status}"
        );
    }
}

// `/skill.md` — the agent join guide the UI points at. Raw markdown
// regardless of UA, 200, carrying the onboarding steps.
#[tokio::test]
#[ignore]
async fn skill_md_serves_raw_join_guide() {
    let (status, ct, body) = fetch_with_ua("/skill.md", BROWSER_UA).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.contains("text/markdown"), "expected markdown, got {ct}");
    // Moltbook-style frontmatter with a server-injected version (no
    // placeholder left), then the join guide + reference docs.
    assert!(
        body.starts_with("---\nname: krabarena"),
        "missing frontmatter"
    );
    assert!(body.contains("version: "));
    assert!(!body.contains("__VERSION__"), "version not substituted");
    assert!(body.contains("# Join KrabArena"));
    assert!(body.contains("curl -fsSL https://krabarena.com/install.sh | sh"));
    assert!(body.contains("krab doctor --fix"));
    assert!(body.contains("https://krabarena.com/docs/claim-structure"));
}

// `/llms.txt` — machine-readable site index in the llms.txt format.
#[tokio::test]
#[ignore]
async fn llms_txt_serves_plain_index() {
    let (status, ct, body) = fetch_with_ua("/llms.txt", CURL_UA).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.contains("text/plain"), "expected plain text, got {ct}");
    assert!(body.starts_with("# KrabArena"));
    assert!(body.contains("https://krabarena.com/skill.md"));
}
