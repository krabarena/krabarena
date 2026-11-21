//! E2E regression guard for the home feed — the unified Battle+Claim
//! list ranked by effort temperature.
//!
//! BUG (0.20.0): the home feed shipped EMPTY. The `effort_decayed` SQL
//! expression (`price_cents/100.0 * exp(...)`, wrapped in
//! `COALESCE(…, 0.0)`) resolves to Postgres `numeric`, which sqlx
//! cannot decode into the declared Rust `f64`. Both feed queries hit
//! their `Err` arm, so the page rendered "Nothing in the feed yet"
//! even though the DB held battles + claims. The page still returned
//! HTTP 200, so no status check caught it — only a content assertion
//! does. Fixed by casting `effort_decayed` to `::float8` in both feed
//! queries (web/pages.rs `index`).
//!
//! These are `#[ignore]` (opt-in; need a populated `$KRAB_TEST_BASE`).
//! Run:
//!   KRAB_TEST_BASE=<url> cargo test -p krabarena-api --test feed_e2e -- --ignored

const DEFAULT_BASE_URL: &str = "https://krabarena.com";

fn base_url() -> String {
    std::env::var("KRAB_TEST_BASE").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

#[derive(Default)]
struct FeedUrlParams<'a> {
    tab: Option<&'a str>,
    status: Option<&'a str>,
    tag: Option<&'a str>,
    q: Option<&'a str>,
    sort: Option<&'a str>,
    page: Option<u32>,
}

fn home_feed_url(base: &str, params: FeedUrlParams<'_>) -> String {
    feed_url(base, "/", params)
}

fn home_tag_suggestions_url(base: &str, params: FeedUrlParams<'_>) -> String {
    feed_url(base, "/fragments/home/tags", params)
}

fn feed_url(base: &str, path: &str, params: FeedUrlParams<'_>) -> String {
    let mut url = reqwest::Url::parse(base).expect("parse test base URL");
    url.set_path(path);
    url.set_query(None);

    let has_query = params.tab.is_some()
        || params.status.is_some()
        || params.tag.is_some()
        || params.q.is_some()
        || params.sort.is_some()
        || params.page.is_some();
    if has_query {
        let page = params.page.map(|value| value.to_string());
        let mut query = url.query_pairs_mut();
        if let Some(tab) = params.tab {
            query.append_pair("tab", tab);
        }
        if let Some(status) = params.status {
            query.append_pair("status", status);
        }
        if let Some(tag) = params.tag {
            query.append_pair("tag", tag);
        }
        if let Some(q) = params.q {
            query.append_pair("q", q);
        }
        if let Some(sort) = params.sort {
            query.append_pair("sort", sort);
        }
        if let Some(page) = page.as_deref() {
            query.append_pair("page", page);
        }
    }

    url.to_string()
}

fn contains_opening_html_tag(body: &str, tag: &str) -> bool {
    let needle = format!("<{tag}");
    body.match_indices(&needle).any(|(index, _)| {
        body[index + needle.len()..]
            .chars()
            .next()
            .is_some_and(|ch| ch == '>' || ch.is_ascii_whitespace())
    })
}

#[test]
fn html_tag_detector_does_not_match_header() {
    assert!(contains_opening_html_tag("<head>", "head"));
    assert!(contains_opening_html_tag("<head data-test>", "head"));
    assert!(contains_opening_html_tag("<head\n>", "head"));
    assert!(contains_opening_html_tag("<html lang=\"en\">", "html"));
    assert!(!contains_opening_html_tag(
        r#"<header class="post-meta">"#,
        "head"
    ));
}

/// The home feed must contain at least one item. The markdown (agent /
/// CLI) variant deterministically prints `Hottest N items` — parse N
/// and assert it's > 0. This is the trivial guard that the decode
/// regression above would have failed (it showed `Hottest 0 items`).
#[tokio::test]
#[ignore]
async fn home_feed_is_not_empty() {
    let base = base_url();
    let client = reqwest::Client::new();

    let resp = client
        .get(home_feed_url(&base, FeedUrlParams::default()))
        .header("Accept", "text/markdown")
        .send()
        .await
        .expect("GET / failed");
    assert!(
        resp.status().is_success(),
        "GET / returned {}",
        resp.status()
    );
    let body = resp.text().await.expect("read / body");

    // "## Hottest N items by $ effort (time-decayed)"
    let n: u64 = body
        .split("Hottest ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            panic!("could not find 'Hottest N items' in home feed markdown:\n{body}")
        });

    assert!(
        n > 0,
        "HOME FEED IS EMPTY (Hottest {n} items). Battles/claims exist but the feed query \
         returned nothing — likely a sqlx decode regression on the feed query (e.g. an \
         expression resolving to `numeric` vs the declared `f64`). See web/pages.rs `index`."
    );
}

/// Parse the deterministic "## Hottest N items …" count out of the home
/// feed markdown variant. The markdown feed is built from the same
/// filtered Battle+Claim queries as the HTML panels, so it doubles as a
/// data-independent probe of the `?q=` filter.
async fn hottest_count(client: &reqwest::Client, url: &str) -> u64 {
    let resp = client
        .get(url)
        .header("Accept", "text/markdown")
        .send()
        .await
        .unwrap_or_else(|e| panic!("GET {url} failed: {e}"));
    assert!(
        resp.status().is_success(),
        "GET {url} returned {}",
        resp.status()
    );
    let body = resp.text().await.expect("read body");
    body.split("Hottest ")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("could not find 'Hottest N items' in:\n{body}"))
}

/// FEATURE (0.25.0): the home filter bar gained a free-text keyword
/// filter (`?q=`) alongside the tag pills — `ILIKE` substring across the
/// three feed panels. Guard the three things that can silently break:
///   1. a no-match keyword narrows the feed to empty (the filter is
///      actually applied, not ignored),
///   2. the typed term is echoed back into the input (`value="…"`) so a
///      direct `/?q=` link / reload / back-forward restores it,
///   3. a keyword full of `LIKE` wildcards is escaped, not treated as a
///      wildcard or a SQL error (still HTTP 200, still narrows to empty).
#[tokio::test]
#[ignore]
async fn home_keyword_filter_narrows_and_prefills() {
    let base = base_url();
    let client = reqwest::Client::new();

    // A term that cannot occur in any title / topic / claim body.
    let nonsense = "zqxjvkbwmp-nomatch-0xdeadbeef";

    let unfiltered = hottest_count(&client, &home_feed_url(&base, FeedUrlParams::default())).await;
    assert!(
        unfiltered > 0,
        "precondition: home feed should be non-empty"
    );

    let filtered = hottest_count(
        &client,
        &home_feed_url(
            &base,
            FeedUrlParams {
                q: Some(nonsense),
                ..FeedUrlParams::default()
            },
        ),
    )
    .await;
    assert_eq!(
        filtered, 0,
        "keyword `{nonsense}` matches nothing, so the feed must be empty \
         (got Hottest {filtered}); the `?q=` ILIKE filter is not being applied"
    );

    // Wildcard-only term must be escaped (literal search), never a SQL
    // error or a match-everything wildcard: HTTP 200 + empty feed.
    let wildcard = hottest_count(
        &client,
        &home_feed_url(
            &base,
            FeedUrlParams {
                q: Some("%_%"),
                ..FeedUrlParams::default()
            },
        ),
    )
    .await;
    assert_eq!(
        wildcard, 0,
        "wildcard term `%_%` should be escaped to a literal substring search \
         (got Hottest {wildcard}); LIKE metacharacters are leaking unescaped"
    );

    // The typed term is reflected into the filter input for `/?q=` deep
    // links / reload / back-forward restore.
    let html = client
        .get(home_feed_url(
            &base,
            FeedUrlParams {
                q: Some("foofilter"),
                ..FeedUrlParams::default()
            },
        ))
        .header("Accept", "text/html")
        .send()
        .await
        .expect("GET /?q=foofilter failed")
        .text()
        .await
        .expect("read html body");
    assert!(
        html.contains("value=\"foofilter\""),
        "the home filter input should pre-fill `value=\"foofilter\"` from `?q=` \
         so a direct link / reload restores the keyword"
    );
}

/// Pull the first feed item with a ≥3-word title from the markdown
/// variant and return its first + last ASCII-alphanumeric words — two
/// words that share one item's title but are NOT adjacent there.
async fn nonadjacent_title_words(client: &reqwest::Client, base: &str) -> (String, String) {
    let body = client
        .get(home_feed_url(base, FeedUrlParams::default()))
        .header("Accept", "text/markdown")
        .send()
        .await
        .expect("GET / failed")
        .text()
        .await
        .expect("read body");
    for line in body.lines() {
        // "### 🦀 Battle — <title>" / "### 💬 Claim — <title>"
        if !line.starts_with("### ") {
            continue;
        }
        let Some(idx) = line.find(" — ") else {
            continue;
        };
        let words: Vec<String> = line[idx + " — ".len()..]
            .split_whitespace()
            .map(|w| {
                w.chars()
                    .filter(char::is_ascii_alphanumeric)
                    .collect::<String>()
            })
            .filter(|w| w.len() >= 3)
            .collect();
        if words.len() >= 3 {
            return (
                words.first().unwrap().clone(),
                words.last().unwrap().clone(),
            );
        }
    }
    panic!("no ≥3-word feed item title found in markdown:\n{body}");
}

/// FEATURE (0.25.0): a multi-word `?q=` AND-matches each word
/// independently — NOT as a contiguous substring. The two words below
/// come from one real title but sit at its opposite ends, so the old
/// `%first last%` contiguous match would have returned an empty feed.
#[tokio::test]
#[ignore]
async fn home_keyword_multiword_is_and_not_contiguous() {
    let base = base_url();
    let client = reqwest::Client::new();
    let (w1, w2) = nonadjacent_title_words(&client, &base).await;

    let both_query = format!("{w1} {w2}");
    let both = hottest_count(
        &client,
        &home_feed_url(
            &base,
            FeedUrlParams {
                q: Some(&both_query),
                ..FeedUrlParams::default()
            },
        ),
    )
    .await;
    assert!(
        both > 0,
        "multi-word `{w1} {w2}` (non-adjacent words from one real title) returned \
         an empty feed — `?q=` is matching words as a contiguous substring instead \
         of AND-ing them"
    );

    // Adding a word that can't occur must empty the feed: every word has
    // to match (AND), so one non-matching word excludes every row.
    let with_nonsense_query = format!("{w1} zqxjvkbwmp-0xdeadbeef");
    let with_nonsense = hottest_count(
        &client,
        &home_feed_url(
            &base,
            FeedUrlParams {
                q: Some(&with_nonsense_query),
                ..FeedUrlParams::default()
            },
        ),
    )
    .await;
    assert_eq!(
        with_nonsense, 0,
        "`{w1} <nonsense>` must be empty — every word must match (AND), but a \
         non-matching word failed to exclude the rows"
    );
}

#[tokio::test]
#[ignore]
async fn home_htmx_fragments_are_server_rendered() {
    let base = base_url();
    let client = reqwest::Client::new();

    let full = client
        .get(home_feed_url(
            &base,
            FeedUrlParams {
                tab: Some("claims"),
                q: Some("krab"),
                ..FeedUrlParams::default()
            },
        ))
        .header("Accept", "text/html")
        .send()
        .await
        .expect("GET full home failed");
    assert!(full.status().is_success());
    let full_body = full.text().await.expect("read full body");
    assert!(contains_opening_html_tag(&full_body, "html"));
    assert!(full_body.contains("id=\"home-feed\""));
    assert!(full_body.contains("data-home-feed-page-number=\"1\""));
    assert!(full_body.contains("href=\"/?tab=claims"));
    assert!(full_body.contains(r#"<link rel="canonical" href=""#));
    assert!(full_body.contains(r#"<meta name="robots" content="noindex,follow">"#));
    assert!(full_body.contains(r#"<meta property="og:title""#));
    assert!(full_body.contains(r#"<meta name="twitter:card""#));

    let partial = client
        .get(home_feed_url(
            &base,
            FeedUrlParams {
                tab: Some("claims"),
                q: Some("krab"),
                ..FeedUrlParams::default()
            },
        ))
        .header("Accept", "text/html")
        .header("HX-Request", "true")
        .send()
        .await
        .expect("GET HTMX home failed");
    assert!(partial.status().is_success());
    let vary = partial
        .headers()
        .get("vary")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        vary.to_ascii_lowercase().contains("hx-request"),
        "HTMX partial must vary on HX-Request; got {vary:?}"
    );
    let partial_body = partial.text().await.expect("read partial body");
    assert!(!contains_opening_html_tag(&partial_body, "html"));
    assert!(!contains_opening_html_tag(&partial_body, "head"));
    assert!(!partial_body.contains("<!DOCTYPE"));
    assert!(!partial_body.contains(r#"<link rel="canonical""#));
    assert!(!partial_body.contains(r#"<meta name="robots""#));
    assert!(!partial_body.contains(r#"<meta property="og:"#));
    assert!(!partial_body.contains(r#"<meta name="twitter:"#));
    assert!(!partial_body.contains("application/ld+json"));
    assert!(partial_body.contains("id=\"home-feed\""));
    assert!(partial_body.contains("data-home-feed-page-number=\"1\""));
    assert!(!partial_body.contains("input changed delay:200ms from:#tag-search-input"));
    assert!(partial_body.contains("id=\"tag-search-input\""));
    assert!(partial_body.contains("hx-push-url=\"false\""));
    assert!(partial_body.contains("name=\"q\""));

    let page_2 = client
        .get(home_feed_url(
            &base,
            FeedUrlParams {
                tab: Some("claims"),
                page: Some(2),
                ..FeedUrlParams::default()
            },
        ))
        .header("Accept", "text/html")
        .header("HX-Request", "true")
        .send()
        .await
        .expect("GET HTMX page 2 failed");
    assert!(page_2.status().is_success());
    let page_2_body = page_2.text().await.expect("read page 2 body");
    assert!(!page_2_body.contains("id=\"home-feed\""));
    assert!(page_2_body.contains("data-home-feed-page-number=\"2\""));
    assert!(!contains_opening_html_tag(&page_2_body, "html"));
    assert!(!contains_opening_html_tag(&page_2_body, "head"));
    assert!(!page_2_body.contains(r#"<link rel="canonical""#));
    assert!(!page_2_body.contains(r#"<meta name="robots""#));
    assert!(!page_2_body.contains("application/ld+json"));

    let full_page_2 = client
        .get(home_feed_url(
            &base,
            FeedUrlParams {
                tab: Some("claims"),
                page: Some(2),
                ..FeedUrlParams::default()
            },
        ))
        .header("Accept", "text/html")
        .send()
        .await
        .expect("GET full page 2 failed");
    assert!(full_page_2.status().is_success());
    let full_page_2_body = full_page_2.text().await.expect("read full page 2");
    assert!(contains_opening_html_tag(&full_page_2_body, "html"));
    assert!(full_page_2_body.contains("id=\"home-feed\""));
    assert!(full_page_2_body.contains("data-home-feed-page-number=\"2\""));
}

#[tokio::test]
#[ignore]
async fn home_tag_suggestions_are_html_fragments() {
    let base = base_url();
    let client = reqwest::Client::new();

    let resp = client
        .get(home_tag_suggestions_url(
            &base,
            FeedUrlParams {
                tab: Some("claims"),
                status: Some("open"),
                tag: Some("rust"),
                q: Some("ai"),
                sort: Some("new"),
                ..FeedUrlParams::default()
            },
        ))
        .header("Accept", "text/html")
        .header("HX-Request", "true")
        .send()
        .await
        .expect("GET tag suggestions failed");
    assert!(resp.status().is_success());
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/html"),
        "suggestions should be HTML, got {content_type:?}"
    );
    let body = resp.text().await.expect("read suggestions body");
    assert!(body.contains("id=\"tag-suggest\""));
    assert!(
        !body.trim_start().starts_with('{'),
        "suggestions route must not return JSON"
    );
    if body.contains("tag-suggest-item") {
        assert!(body.contains("tab=claims"));
        assert!(body.contains("status=open"));
        assert!(body.contains("sort=new"));
    }
}
