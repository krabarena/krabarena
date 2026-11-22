//! `robots.txt` and `sitemap.xml` for search engines.
//!
//! The sitemap is computed from the live `battles` and `claims` tables so
//! it stays in sync without a separate build step. For a small arena this
//! is fine to render on every request; we can cache if it grows.

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::AppState;
use crate::services::zoo;

/// AI search, answer-engine and training crawlers that get an explicit
/// group. robots.txt semantics: a crawler obeys only the most specific
/// group naming it, so every group repeats [`zoo::DISALLOWED_PREFIXES`]. KrabArena's
/// content exists to be cited and learned from, so training crawlers
/// are allowed too — to opt out of training, flip the `TRAINING` list
/// to `Disallow: /` (search/retrieval bots are unaffected).
const AI_SEARCH_AGENTS: &[&str] = &[
    "OAI-SearchBot",
    "ChatGPT-User",
    "Claude-SearchBot",
    "Claude-User",
    "PerplexityBot",
    "Perplexity-User",
    "Googlebot",
    "Bingbot",
    "Applebot",
    "DuckAssistBot",
    "MistralAI-User",
];
const AI_TRAINING_AGENTS: &[&str] = &[
    "GPTBot",
    "ClaudeBot",
    "Google-Extended",
    "Applebot-Extended",
    "CCBot",
    "Meta-ExternalAgent",
    "Amazonbot",
];

pub fn render_robots_txt(base: &str) -> String {
    let rules: String = zoo::DISALLOWED_PREFIXES
        .iter()
        .map(|p| format!("Disallow: {p}\n"))
        .collect();
    let group = |agents: &[&str]| -> String {
        let mut g: String = agents
            .iter()
            .map(|a| format!("User-agent: {a}\n"))
            .collect();
        g.push_str("Allow: /\n");
        g.push_str(&rules);
        g
    };
    let mut body = String::new();
    body.push_str("# KrabArena — benchmark results for AI agents, built to be read and cited.\n");
    body.push_str(&format!(
        "# Machine-readable index: {base}/llms.txt · full dump: {base}/llms-full.txt · feed: {base}/feed.json\n\n"
    ));
    body.push_str("# AI search / answer engines and user-triggered fetchers\n");
    body.push_str(&group(AI_SEARCH_AGENTS));
    body.push_str("\n# AI training crawlers (allowed)\n");
    body.push_str(&group(AI_TRAINING_AGENTS));
    body.push_str("\n# Everyone else\n");
    body.push_str(&group(&["*"]));
    body.push_str(&format!("\nSitemap: {base}/sitemap.xml\n"));
    body.push_str(
        "\n# Dear robots: you are on display at /zoo. \
         The room at /zoo/trap/ is staff only. We will know.\n",
    );
    body
}

pub async fn robots_txt(State(state): State<AppState>) -> Response {
    let base = state.public_base_url.trim_end_matches('/');
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        render_robots_txt(base),
    )
        .into_response()
}

pub const SECURITY_CONTACT: &str = "security@krabarena.com";

pub async fn security_txt(State(state): State<AppState>) -> Response {
    let base = state.public_base_url.trim_end_matches('/');
    let expires = Utc::now() + chrono::Duration::days(180);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        security_body(base, expires),
    )
        .into_response()
}

pub fn security_body(base: &str, expires: DateTime<Utc>) -> String {
    format!(
        "Contact: mailto:{SECURITY_CONTACT}\n\
         Expires: {}\n\
         Preferred-Languages: en\n\
         Canonical: {base}/.well-known/security.txt\n",
        expires.format("%Y-%m-%dT%H:%M:%SZ")
    )
}

pub async fn sitemap_xml(State(state): State<AppState>) -> Response {
    let base = state.public_base_url.trim_end_matches('/');
    let mut urls = static_sitemap_entries(base);
    // Battles: prefer slug, fall back to UUID.
    let battles = sqlx::query_as::<_, (Option<String>, Uuid, DateTime<Utc>)>(
        "SELECT slug, id, updated_at FROM battles WHERE deleted_at IS NULL ORDER BY updated_at DESC LIMIT 5000",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    for (slug, id, updated) in battles {
        let path_seg = slug.unwrap_or_else(|| id.to_string());
        urls.push(SitemapEntry {
            loc: format!("{}/battles/{}", base, path_seg),
            lastmod: Some(updated.to_rfc3339()),
            priority: Some("0.8".to_string()),
        });
    }

    // Claims — sitemap lists lineage HEADs only. `adopted` Claims
    // (standalone Claims later attached to a Battle) are public on their
    // Battle page, so they're listed too — before 0.46.3 they were
    // missing (~20% of the Claims shown on Battle pages). A superseded version
    // (the old version an edit replaced) is still reachable by URL but
    // is excluded from the sitemap so crawlers index the current
    // version. See `models::claim::not_superseded`.
    let superseded = crate::models::claim::not_superseded("c");
    let claims = sqlx::query_as::<_, (Option<String>, Uuid, DateTime<Utc>)>(&format!(
        "SELECT c.slug, c.id, GREATEST(c.created_at, c.updated_at) FROM claims c \
         LEFT JOIN battles b ON b.id = c.battle_id \
         WHERE (c.visibility IS NULL OR c.visibility IN ('public', 'adopted')) \
           AND c.deleted_at IS NULL \
           AND (c.battle_id IS NULL OR b.deleted_at IS NULL) \
           {superseded} \
         ORDER BY c.created_at DESC LIMIT 45000"
    ))
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    for (slug, id, created) in claims {
        let path_seg = slug.unwrap_or_else(|| id.to_string());
        urls.push(SitemapEntry {
            loc: format!("{}/claims/{}", base, path_seg),
            lastmod: Some(created.to_rfc3339()),
            priority: Some("0.6".to_string()),
        });
    }

    // Crawler Zoo specimen pages for every species seen in 30 days
    // (cached snapshot — no extra scan per sitemap fetch).
    if let Some(snap) = zoo::snapshot(&state.pool).await {
        for f in &snap.families {
            urls.push(SitemapEntry {
                loc: format!("{}/zoo/bots/{}", base, f.info.slug),
                lastmod: Some(f.last_seen.date_naive().to_string()),
                priority: Some("0.4".to_string()),
            });
            // Its food bowl: the first bot to fetch it eats what's inside.
            urls.push(SitemapEntry {
                loc: format!("{}/zoo/bowls/{}.txt", base, f.info.slug),
                lastmod: None,
                priority: Some("0.2".to_string()),
            });
        }
    }

    let mut body = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    body.push_str("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for entry in urls {
        body.push_str("  <url>\n");
        body.push_str(&format!("    <loc>{}</loc>\n", xml_escape(&entry.loc)));
        if let Some(lm) = entry.lastmod {
            body.push_str(&format!("    <lastmod>{}</lastmod>\n", xml_escape(&lm)));
        }
        if let Some(p) = entry.priority {
            body.push_str(&format!("    <priority>{}</priority>\n", p));
        }
        body.push_str("  </url>\n");
    }
    body.push_str("</urlset>\n");

    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml; charset=utf-8"),
    );
    resp
}

struct SitemapEntry {
    loc: String,
    lastmod: Option<String>,
    priority: Option<String>,
}

fn static_sitemap_entries(base: &str) -> Vec<SitemapEntry> {
    let pages = [
        ("/", "1.0"),
        ("/agents", "0.7"),
        ("/arena", "0.7"),
        ("/about", "0.5"),
        ("/docs", "0.5"),
        ("/claims/standalone", "0.4"),
        ("/changelog", "0.3"),
        ("/zoo", "0.6"),
        ("/zoo/leaderboard", "0.5"),
    ]
    .into_iter()
    .map(|(path, priority)| (path.to_string(), priority));
    let docs = crate::routes::docs::PUBLIC_DOCS
        .iter()
        .map(|d| (format!("/docs/{}", d.stem), "0.5"));
    pages
        .chain(docs)
        .map(|(path, priority)| SitemapEntry {
            loc: format!("{base}{path}"),
            lastmod: None,
            priority: Some(priority.to_string()),
        })
        .collect()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_disallows_every_zoo_prefix_including_the_trap() {
        let body = render_robots_txt("https://krabarena.com");
        for p in zoo::DISALLOWED_PREFIXES {
            assert!(body.contains(&format!("Disallow: {p}\n")), "{p}");
        }
        assert!(body.contains("Disallow: /zoo/trap/\n"));
        assert!(
            !body.contains("Disallow: /zoo\n"),
            "the zoo itself is public"
        );
        assert!(body.contains("Sitemap: https://krabarena.com/sitemap.xml"));
    }

    #[test]
    fn security_txt_has_rfc9116_required_fields() {
        let expires = DateTime::parse_from_rfc3339("2027-05-19T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let body = security_body("https://krabarena.com", expires);
        assert!(body.contains("Contact: mailto:security@krabarena.com\n"));
        assert!(body.contains("Expires: 2027-05-19T00:00:00Z\n"));
        assert!(body.contains("Canonical: https://krabarena.com/.well-known/security.txt\n"));
    }

    #[test]
    fn robots_txt_names_ai_crawlers_and_keeps_disallows_per_group() {
        let body = render_robots_txt("https://krabarena.com");
        for agent in [
            "GPTBot",
            "OAI-SearchBot",
            "ChatGPT-User",
            "PerplexityBot",
            "ClaudeBot",
            "Claude-SearchBot",
            "Google-Extended",
            "Bingbot",
        ] {
            assert!(
                body.contains(&format!("User-agent: {agent}\n")),
                "{agent} missing"
            );
        }
        // Three groups (search, training, *) — each repeats every Disallow.
        assert_eq!(body.matches("Allow: /\n").count(), 3);
        assert_eq!(body.matches("Disallow: /v1/\n").count(), 3);
        assert!(
            !body.contains("Disallow: /\n"),
            "nothing is blocked wholesale"
        );
        assert!(body.contains("\nSitemap: https://krabarena.com/sitemap.xml\n"));
    }

    #[test]
    fn sitemap_lists_every_public_doc_and_no_unlisted_ones() {
        let locs: Vec<String> = static_sitemap_entries("https://krabarena.com")
            .into_iter()
            .map(|e| e.loc)
            .collect();
        for d in crate::routes::docs::PUBLIC_DOCS {
            let want = format!("https://krabarena.com/docs/{}", d.stem);
            assert!(locs.contains(&want), "missing {want}");
        }
        assert!(locs.contains(&"https://krabarena.com/docs".to_string()));
        for stem in ["admin-cli", "ui", "og-render"] {
            let unwanted = format!("https://krabarena.com/docs/{stem}");
            assert!(!locs.contains(&unwanted), "{unwanted} must stay unlisted");
        }
    }

    #[test]
    fn xml_escape_basics() {
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("<tag>"), "&lt;tag&gt;");
        assert_eq!(xml_escape("\"hi\""), "&quot;hi&quot;");
    }
}
