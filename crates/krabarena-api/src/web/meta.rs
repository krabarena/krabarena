//! Page metadata shared across HTML templates (SEO / OpenGraph).

use std::sync::OnceLock;

use serde::Serialize;

use crate::web::assets::{ViteEntryAssets, frontend_entry_assets};

/// Cached `GA_MEASUREMENT_ID` env read. Resolved on first access and
/// reused for the process lifetime — env vars don't change at
/// runtime, and `PageMeta::new` is on the per-request hot path.
fn ga_measurement_id() -> Option<&'static str> {
    static CACHED: OnceLock<Option<String>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            std::env::var("GA_MEASUREMENT_ID")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .as_deref()
}

/// Cached `DISCORD_SUPPORT_INVITE_URL` env read. Same `OnceLock`
/// pattern as `ga_measurement_id` — resolved once, reused forever.
/// When set, every page renders a "Our support" Discord button in
/// the header and a help link inside claim/verification modals.
fn discord_support_invite_url() -> Option<&'static str> {
    static CACHED: OnceLock<Option<String>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            std::env::var("DISCORD_SUPPORT_INVITE_URL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .as_deref()
}

/// Cached `GITHUB_REPO_URL` env read (e.g.
/// `https://github.com/krabarena/krabarena`). When set, every page
/// renders a "GitHub" button in the header and the mobile drawer.
/// Env-gated so the link only goes live once the repo is public.
fn github_repo_url() -> Option<&'static str> {
    static CACHED: OnceLock<Option<String>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            std::env::var("GITHUB_REPO_URL")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .as_deref()
}

/// All the per-page SEO data consumed by `base.html`.
///
/// Every template struct that extends `base.html` carries a `meta: PageMeta`
/// field so the header can render meta description, canonical link, and
/// OpenGraph / Twitter Card tags without reaching for globals.
#[derive(Debug, Clone)]
pub struct PageMeta {
    pub title: String,
    pub analytics_page_title: String,
    pub description: String,
    pub canonical_url: String,
    pub robots: String,
    /// "website" or "article".
    pub og_type: String,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_url: Option<String>,
    /// Absolute URL.
    pub og_image: String,
    pub og_image_alt: String,
    pub twitter_card: String,
    /// HTML-safe JSON-LD script bodies rendered only by the full
    /// browser layout. Values are serialized through `serde_json` and
    /// escaped for `<script type="application/ld+json">`.
    pub json_ld: Vec<String>,
    /// Raw-markdown twin of this page (`<link rel="alternate"
    /// type="text/markdown">`). Set on Battle and Claim pages so AI
    /// crawlers that never negotiate `Accept: text/markdown` can still
    /// discover the clean, chrome-free version (`/claims/<slug>.md`).
    pub markdown_alternate: Option<String>,
    /// RFC 3339 first-publication time — `article:published_time`.
    pub published_time: Option<String>,
    /// RFC 3339 last-modified time — `article:modified_time`, and the
    /// visible "Updated …" date on Battle pages (answer engines prefer
    /// dated answers).
    pub modified_time: Option<String>,
    /// Human-readable form of `modified_time` (e.g. "Oct 27, 2026").
    pub modified_label: Option<String>,
    /// GA4 Measurement ID (`G-XXXXXXXXXX`) — populated from
    /// `AppState::ga_measurement_id`. When `None`, `base.html` skips
    /// the gtag.js snippet entirely so dev / test envs don't pollute
    /// the prod analytics dashboard. Plumbed through `PageMeta`
    /// rather than as a separate template field so every existing
    /// page that already carries `meta: PageMeta` picks it up
    /// automatically — no per-handler change needed.
    pub ga_measurement_id: Option<String>,
    /// Discord support server invite URL — populated from
    /// `DISCORD_SUPPORT_INVITE_URL`. When `Some`, `base.html` renders
    /// a "Our support" button in the header and modals show a help link.
    pub discord_support_invite_url: Option<String>,
    /// Public source repository URL — populated from `GITHUB_REPO_URL`.
    /// When `Some`, the header and mobile drawer link to it.
    pub github_repo_url: Option<String>,
    /// Frontend bundle assets resolved from the Vite manifest.
    pub frontend_assets: ViteEntryAssets,
    /// Per-response CSP nonce (see `web::security_headers`). Inline
    /// `<script>` blocks must carry `nonce="{{ meta.csp_nonce }}"` —
    /// `script-src` has no `'unsafe-inline'`. Empty outside the
    /// security-header middleware (unit tests rendering directly).
    pub csp_nonce: String,
}

impl PageMeta {
    /// Default metadata for a generic page. Callers typically override
    /// `title` and `description` immediately after.
    pub fn new(public_base_url: &str, path: &str) -> Self {
        let base = public_base_url.trim_end_matches('/');
        Self {
            title: "KrabArena".to_string(),
            analytics_page_title: "KrabArena".to_string(),
            description:
                "KrabArena — a contest arena for AI agents. Battles, claims, verifications."
                    .to_string(),
            canonical_url: format!("{}{}", base, path),
            robots: "index,follow".to_string(),
            og_type: "website".to_string(),
            og_title: None,
            og_description: None,
            og_url: None,
            og_image: format!("{}/static/og-default.png", base),
            og_image_alt: "KrabArena — Know which tool wins for your job".to_string(),
            twitter_card: "summary_large_image".to_string(),
            json_ld: Vec::new(),
            markdown_alternate: None,
            published_time: None,
            modified_time: None,
            modified_label: None,
            ga_measurement_id: ga_measurement_id().map(str::to_string),
            discord_support_invite_url: discord_support_invite_url().map(str::to_string),
            github_repo_url: github_repo_url().map(str::to_string),
            frontend_assets: frontend_entry_assets(),
            csp_nonce: crate::web::security_headers::current_nonce().unwrap_or_default(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = unbrand_title(title.into());
        self.analytics_page_title = branded_title(&self.title);
        self
    }

    pub fn with_analytics_title(mut self, title: impl Into<String>) -> Self {
        self.analytics_page_title = branded_title(&unbrand_title(title.into()));
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        let d = description.into();
        self.description = truncate_description(&d, 180);
        self
    }

    pub fn with_robots(mut self, robots: impl Into<String>) -> Self {
        self.robots = robots.into();
        self
    }

    pub fn noindex_follow(self) -> Self {
        self.with_robots("noindex,follow")
    }

    pub fn with_og_type(mut self, og_type: impl Into<String>) -> Self {
        self.og_type = og_type.into();
        self
    }

    pub fn with_og_title(mut self, title: impl Into<String>) -> Self {
        self.og_title = Some(title.into());
        self
    }

    pub fn with_og_description(mut self, description: impl Into<String>) -> Self {
        self.og_description = Some(truncate_description(&description.into(), 180));
        self
    }

    pub fn with_og_url(mut self, url: impl Into<String>) -> Self {
        self.og_url = Some(url.into());
        self
    }

    /// Override the per-page OpenGraph / Twitter card image URL.
    /// Used by `battle_detail` / `claim_detail` to point social
    /// previews at the per-entity card served from `/og/{kind}/{id}.png`
    /// instead of the static homepage fallback.
    pub fn with_og_image(mut self, og_image: impl Into<String>) -> Self {
        self.og_image = og_image.into();
        self
    }

    pub fn with_og_image_alt(mut self, alt: impl Into<String>) -> Self {
        self.og_image_alt = truncate_description(&alt.into(), 180);
        self
    }

    pub fn with_twitter_card(mut self, card: impl Into<String>) -> Self {
        self.twitter_card = card.into();
        self
    }

    pub fn with_json_ld(mut self, json_ld: impl Into<String>) -> Self {
        self.json_ld.push(json_ld.into());
        self
    }

    pub fn with_markdown_alternate(mut self, url: impl Into<String>) -> Self {
        self.markdown_alternate = Some(url.into());
        self
    }

    /// Stamp publication / modification times (rendered as
    /// `article:published_time` / `article:modified_time`).
    pub fn with_times(
        mut self,
        published: chrono::DateTime<chrono::Utc>,
        modified: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let modified = modified.max(published);
        self.published_time = Some(published.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        self.modified_time = Some(modified.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        self.modified_label = Some(modified.format("%b %-d, %Y").to_string());
        self
    }

    pub fn with_ga_measurement_id(mut self, id: Option<String>) -> Self {
        self.ga_measurement_id = id;
        self
    }

    pub fn og_title_value(&self) -> &str {
        self.og_title.as_deref().unwrap_or(&self.title)
    }

    pub fn og_description_value(&self) -> &str {
        self.og_description.as_deref().unwrap_or(&self.description)
    }

    pub fn og_url_value(&self) -> &str {
        self.og_url.as_deref().unwrap_or(&self.canonical_url)
    }

    pub fn icon(&self, name: &str) -> String {
        crate::web::icons::decorative(name, self.frontend_assets.icon_sprite.as_deref())
    }

    pub fn labeled_icon(&self, name: &str, label: &str) -> String {
        crate::web::icons::labeled(name, label, self.frontend_assets.icon_sprite.as_deref())
    }
}

#[derive(Debug, Clone)]
pub struct BreadcrumbItem {
    pub name: String,
    pub url: String,
}

impl BreadcrumbItem {
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: json_ld_text(&name.into(), 120),
            url: url.into(),
        }
    }
}

#[derive(Serialize)]
struct WebSiteJsonLd {
    #[serde(rename = "@context")]
    context: &'static str,
    #[serde(rename = "@type")]
    kind: &'static str,
    name: &'static str,
    url: String,
    #[serde(rename = "potentialAction")]
    potential_action: SearchActionJsonLd,
}

#[derive(Serialize)]
struct SearchActionJsonLd {
    #[serde(rename = "@type")]
    kind: &'static str,
    target: SearchTargetJsonLd,
    #[serde(rename = "query-input")]
    query_input: &'static str,
}

#[derive(Serialize)]
struct SearchTargetJsonLd {
    #[serde(rename = "@type")]
    kind: &'static str,
    #[serde(rename = "urlTemplate")]
    url_template: String,
}

#[derive(Serialize)]
struct BreadcrumbListJsonLd {
    #[serde(rename = "@context")]
    context: &'static str,
    #[serde(rename = "@type")]
    kind: &'static str,
    #[serde(rename = "itemListElement")]
    item_list_element: Vec<ListItemJsonLd>,
}

#[derive(Serialize)]
struct ListItemJsonLd {
    #[serde(rename = "@type")]
    kind: &'static str,
    position: usize,
    name: String,
    item: String,
}

#[derive(Serialize)]
struct CreativeWorkJsonLd {
    #[serde(rename = "@context")]
    context: &'static str,
    #[serde(rename = "@type")]
    kind: String,
    name: String,
    description: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<NamedThingJsonLd>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headline: Option<String>,
    #[serde(rename = "datePublished", skip_serializing_if = "Option::is_none")]
    date_published: Option<String>,
    #[serde(rename = "dateModified", skip_serializing_if = "Option::is_none")]
    date_modified: Option<String>,
    #[serde(rename = "mainEntityOfPage", skip_serializing_if = "Option::is_none")]
    main_entity_of_page: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<OrganizationJsonLd>,
    #[serde(rename = "isPartOf", skip_serializing_if = "Option::is_none")]
    is_part_of: Option<PartOfJsonLd>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    keywords: Vec<String>,
}

#[derive(Serialize)]
struct OrganizationJsonLd {
    #[serde(rename = "@type")]
    kind: &'static str,
    name: &'static str,
    url: String,
    logo: String,
}

#[derive(Serialize)]
struct PartOfJsonLd {
    #[serde(rename = "@type")]
    kind: &'static str,
    name: String,
    url: String,
}

/// Optional enrichments for [`creative_work_json_ld_rich`]. Everything
/// here must mirror what the page visibly shows (dates, parent Battle,
/// tags) — structured data that disagrees with the page is ignored or
/// penalised by search / answer engines.
#[derive(Default)]
pub struct CreativeWorkExtras<'a> {
    pub public_base_url: &'a str,
    pub date_published: Option<chrono::DateTime<chrono::Utc>>,
    pub date_modified: Option<chrono::DateTime<chrono::Utc>>,
    /// `(name, url)` of the parent Battle for a Claim.
    pub is_part_of: Option<(&'a str, &'a str)>,
    pub keywords: &'a [String],
}

#[derive(Serialize)]
struct NamedThingJsonLd {
    #[serde(rename = "@type")]
    kind: &'static str,
    name: String,
}

pub fn website_json_ld(public_base_url: &str) -> String {
    let base = public_base_url.trim_end_matches('/');
    json_ld_script(&WebSiteJsonLd {
        context: "https://schema.org",
        kind: "WebSite",
        name: "KrabArena",
        url: format!("{base}/"),
        potential_action: SearchActionJsonLd {
            kind: "SearchAction",
            target: SearchTargetJsonLd {
                kind: "EntryPoint",
                url_template: format!("{base}/search?q={{search_term_string}}"),
            },
            query_input: "required name=search_term_string",
        },
    })
}

pub fn breadcrumb_json_ld(items: Vec<BreadcrumbItem>) -> String {
    json_ld_script(&BreadcrumbListJsonLd {
        context: "https://schema.org",
        kind: "BreadcrumbList",
        item_list_element: items
            .into_iter()
            .enumerate()
            .map(|(idx, item)| ListItemJsonLd {
                kind: "ListItem",
                position: idx + 1,
                name: item.name,
                item: item.url,
            })
            .collect(),
    })
}

pub fn creative_work_json_ld(
    kind: &str,
    name: &str,
    description: &str,
    url: &str,
    image: Option<&str>,
    author: Option<&str>,
) -> String {
    json_ld_script(&CreativeWorkJsonLd {
        context: "https://schema.org",
        kind: kind.to_string(),
        name: json_ld_text(name, 160),
        description: json_ld_text(description, 240),
        url: url.to_string(),
        image: image.map(str::to_string),
        author: author.map(|name| NamedThingJsonLd {
            kind: "Person",
            name: json_ld_text(name, 120),
        }),
        headline: None,
        date_published: None,
        date_modified: None,
        main_entity_of_page: None,
        publisher: None,
        is_part_of: None,
        keywords: Vec::new(),
    })
}

/// [`creative_work_json_ld`] plus the fields answer engines use to
/// judge freshness and provenance: `headline`, `datePublished`,
/// `dateModified`, `publisher`, `mainEntityOfPage`, `isPartOf`
/// (Claim → parent Battle) and `keywords`.
pub fn creative_work_json_ld_rich(
    kind: &str,
    name: &str,
    description: &str,
    url: &str,
    image: Option<&str>,
    author: Option<&str>,
    extras: CreativeWorkExtras<'_>,
) -> String {
    let base = extras.public_base_url.trim_end_matches('/');
    let fmt =
        |d: chrono::DateTime<chrono::Utc>| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let modified = match (extras.date_published, extras.date_modified) {
        (Some(p), Some(m)) => Some(m.max(p)),
        (p, m) => m.or(p),
    };
    json_ld_script(&CreativeWorkJsonLd {
        context: "https://schema.org",
        kind: kind.to_string(),
        name: json_ld_text(name, 160),
        description: json_ld_text(description, 240),
        url: url.to_string(),
        image: image.map(str::to_string),
        author: author.map(|name| NamedThingJsonLd {
            kind: "Person",
            name: json_ld_text(name, 120),
        }),
        headline: Some(json_ld_text(name, 110)),
        date_published: extras.date_published.map(fmt),
        date_modified: modified.map(fmt),
        main_entity_of_page: Some(url.to_string()),
        publisher: Some(OrganizationJsonLd {
            kind: "Organization",
            name: "KrabArena",
            url: format!("{base}/"),
            logo: format!("{base}/static/og-default.png"),
        }),
        is_part_of: extras.is_part_of.map(|(n, u)| PartOfJsonLd {
            kind: "CreativeWork",
            name: json_ld_text(n, 160),
            url: u.to_string(),
        }),
        keywords: extras
            .keywords
            .iter()
            .map(|k| json_ld_text(k, 60))
            .filter(|k| !k.trim().is_empty())
            .take(12)
            .collect(),
    })
}

fn json_ld_script<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "{}".to_string())
        .replace("</", "<\\/")
}

pub fn json_ld_text(s: &str, max: usize) -> String {
    let markdown_softened: String = s
        .chars()
        .map(|ch| match ch {
            '#' | '*' | '`' | '_' | '>' | '[' | ']' | '!' => ' ',
            _ => ch,
        })
        .collect();
    let redacted = markdown_softened
        .split_whitespace()
        .filter(|token| !is_sensitive_json_ld_token(token))
        .collect::<Vec<_>>()
        .join(" ");
    truncate_description(&redacted, max)
}

fn is_sensitive_json_ld_token(token: &str) -> bool {
    let trimmed = token.trim_matches(|ch: char| {
        matches!(
            ch,
            '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | '.' | ';' | ':'
        )
    });
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("blob:")
        || lower.starts_with("gs://")
        || lower.contains("/admin")
        || lower.contains("private-artifact")
        || lower.contains("private_artifact")
        || looks_like_jwt(trimmed)
}

fn looks_like_jwt(token: &str) -> bool {
    token.len() > 80 && token.split('.').count() == 3 && token.starts_with("eyJ")
}

fn unbrand_title(title: String) -> String {
    title
        .strip_suffix(" — KrabArena")
        .unwrap_or(&title)
        .to_string()
}

fn branded_title(title: &str) -> String {
    if title == "KrabArena" {
        title.to_string()
    } else {
        format!("{title} — KrabArena")
    }
}

/// Resolve which visual theme the page should render against.
///
/// Server-side mirror of the inline cookie/query script in `base.html`,
/// so handlers can decide which template branches to populate before
/// the page even reaches the browser. Pure read of the request — no
/// `Set-Cookie` here; the inline JS handles persistence (the JS sees
/// `?theme=arena` first and writes the cookie before this handler is
/// ever consulted on the next request).
///
/// Order of precedence:
/// 1. `?theme=...` query param — explicit, always wins.
/// 2. `theme` cookie — sticks across navigations.
/// 3. `"arena"` — default (the new editorial theme; classic is opt-out
///    via `?theme=classic`).
pub fn theme_from_request(headers: &axum::http::HeaderMap, query: Option<&str>) -> String {
    const KNOWN: &[&str] = &["classic", "arena"];

    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("theme=") {
                if KNOWN.contains(&v) {
                    return v.to_string();
                }
            }
        }
    }

    if let Some(cookie) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        for pair in cookie.split(';') {
            let pair = pair.trim();
            if let Some(v) = pair.strip_prefix("theme=") {
                if KNOWN.contains(&v) {
                    return v.to_string();
                }
            }
        }
    }

    "arena".to_string()
}

/// Collapse whitespace and truncate to `max` chars on a word boundary,
/// suitable for a meta description or OG summary.
pub fn truncate_description(s: &str, max: usize) -> String {
    let cleaned: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= max {
        return cleaned;
    }
    let mut end = 0usize;
    for (count, (idx, _)) in cleaned.char_indices().enumerate() {
        if count >= max {
            break;
        }
        end = idx;
    }
    // Back off to previous whitespace for a nicer cut.
    if let Some(last_space) = cleaned[..end].rfind(' ') {
        if last_space > max.saturating_sub(40) {
            end = last_space;
        }
    }
    let mut out = cleaned[..end].trim_end().to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_url_composes_correctly() {
        let m = PageMeta::new("https://krabarena.com/", "/battles/foo");
        assert_eq!(m.canonical_url, "https://krabarena.com/battles/foo");
    }

    #[test]
    fn analytics_title_follows_visible_title_by_default() {
        let m = PageMeta::new("https://krabarena.com", "/changelog").with_title("Changelog");
        assert_eq!(m.title, "Changelog");
        assert_eq!(m.analytics_page_title, "Changelog — KrabArena");
    }

    #[test]
    fn analytics_title_can_override_visible_title() {
        let m = PageMeta::new("https://krabarena.com", "/battles/demo")
            .with_title("Specific Battle Title")
            .with_analytics_title("Battle");
        assert_eq!(m.title, "Specific Battle Title");
        assert_eq!(m.analytics_page_title, "Battle — KrabArena");
    }

    #[test]
    fn title_branding_is_not_duplicated() {
        let m = PageMeta::new("https://krabarena.com", "/profile/email-preferences")
            .with_title("Email preferences — KrabArena");
        assert_eq!(m.title, "Email preferences");
        assert_eq!(m.analytics_page_title, "Email preferences — KrabArena");

        let m = PageMeta::new("https://krabarena.com", "/battles/demo")
            .with_title("Specific Battle")
            .with_analytics_title("Battle — KrabArena");
        assert_eq!(m.analytics_page_title, "Battle — KrabArena");
    }

    #[test]
    fn meta_robots_default_to_index_follow_and_can_noindex() {
        let m = PageMeta::new("https://krabarena.com", "/");
        assert_eq!(m.robots, "index,follow");

        let m = m.noindex_follow();
        assert_eq!(m.robots, "noindex,follow");
    }

    #[test]
    fn meta_open_graph_values_fall_back_to_page_values() {
        let m = PageMeta::new("https://krabarena.com", "/search")
            .with_title("Search")
            .with_description("Find battles and claims.");

        assert_eq!(m.og_title_value(), "Search");
        assert_eq!(m.og_description_value(), "Find battles and claims.");
        assert_eq!(m.og_url_value(), "https://krabarena.com/search");

        let m = m
            .with_og_title("OG Search")
            .with_og_description("OG description")
            .with_og_url("https://example.test/search");
        assert_eq!(m.og_title_value(), "OG Search");
        assert_eq!(m.og_description_value(), "OG description");
        assert_eq!(m.og_url_value(), "https://example.test/search");
    }

    #[test]
    fn seo_website_json_ld_contains_search_action() {
        let body = website_json_ld("https://krabarena.com/");
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json-ld");
        assert_eq!(value["@type"], "WebSite");
        assert_eq!(value["url"], "https://krabarena.com/");
        assert_eq!(value["potentialAction"]["@type"], "SearchAction");
        assert_eq!(
            value["potentialAction"]["target"]["urlTemplate"],
            "https://krabarena.com/search?q={search_term_string}"
        );
    }

    #[test]
    fn rich_creative_work_json_ld_has_dates_publisher_and_parent() {
        let published = chrono::DateTime::parse_from_rfc3339("2026-10-27T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let modified = chrono::DateTime::parse_from_rfc3339("2026-10-28T11:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let tags = vec!["ai-agents".to_string()];
        let body = creative_work_json_ld_rich(
            "Article",
            "Mem0 vs Cognee",
            "Recall benchmark",
            "https://krabarena.com/claims/x",
            None,
            Some("krabagent"),
            CreativeWorkExtras {
                public_base_url: "https://krabarena.com/",
                date_published: Some(published),
                date_modified: Some(modified),
                is_part_of: Some(("Memory Battle", "https://krabarena.com/battles/m")),
                keywords: &tags,
            },
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["@type"], "Article");
        assert_eq!(v["headline"], "Mem0 vs Cognee");
        assert_eq!(v["datePublished"], "2026-10-27T10:00:00Z");
        assert_eq!(v["dateModified"], "2026-10-28T11:30:00Z");
        assert_eq!(v["publisher"]["name"], "KrabArena");
        assert_eq!(v["publisher"]["url"], "https://krabarena.com/");
        assert_eq!(v["isPartOf"]["url"], "https://krabarena.com/battles/m");
        assert_eq!(v["mainEntityOfPage"], "https://krabarena.com/claims/x");
        assert_eq!(v["keywords"][0], "ai-agents");
    }

    #[test]
    fn page_meta_times_are_clamped_and_labelled() {
        let published = chrono::DateTime::parse_from_rfc3339("2026-10-27T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let earlier = chrono::DateTime::parse_from_rfc3339("2026-10-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let meta =
            PageMeta::new("https://krabarena.com", "/battles/x").with_times(published, earlier);
        assert_eq!(meta.published_time.as_deref(), Some("2026-10-27T10:00:00Z"));
        assert_eq!(meta.modified_time.as_deref(), Some("2026-10-27T10:00:00Z"));
        assert_eq!(meta.modified_label.as_deref(), Some("Oct 27, 2026"));
    }

    #[test]
    fn seo_breadcrumb_json_ld_contains_ordered_items() {
        let body = breadcrumb_json_ld(vec![
            BreadcrumbItem::new("KrabArena", "https://krabarena.com/"),
            BreadcrumbItem::new("Docs", "https://krabarena.com/docs"),
        ]);
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json-ld");
        assert_eq!(value["@type"], "BreadcrumbList");
        assert_eq!(value["itemListElement"][0]["position"], 1);
        assert_eq!(value["itemListElement"][1]["name"], "Docs");
    }

    #[test]
    fn seo_creative_work_json_ld_uses_plain_safe_text() {
        let body = creative_work_json_ld(
            "Article",
            "# Claim </script>",
            "## Summary with *markdown* and [links]",
            "https://krabarena.com/claims/demo",
            None,
            Some("Test Agent"),
        );

        assert!(!body.contains("</script>"));
        let value: serde_json::Value = serde_json::from_str(&body).expect("valid json-ld");
        assert_eq!(value["@type"], "Article");
        assert_eq!(value["name"], "Claim </script");
        assert_eq!(value["author"]["name"], "Test Agent");
        assert!(
            !value["description"]
                .as_str()
                .expect("description")
                .contains('*')
        );
    }

    #[test]
    fn seo_json_ld_text_removes_urls_and_token_like_values() {
        let token = "eyJaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.bbbbb.ccccc";
        let text = json_ld_text(
            &format!(
                "Public summary https://example.test/private-artifact/blob {token} still readable"
            ),
            240,
        );

        assert_eq!(text, "Public summary still readable");
    }

    #[test]
    fn truncate_short_passthrough() {
        assert_eq!(truncate_description("hello world", 50), "hello world");
    }

    #[test]
    fn truncate_trims_on_word_boundary() {
        let s = "a".repeat(30) + " " + &"b".repeat(200);
        let out = truncate_description(&s, 60);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 61);
    }

    #[test]
    fn truncate_collapses_whitespace() {
        assert_eq!(
            truncate_description("foo\n\n  bar   baz", 50),
            "foo bar baz"
        );
    }
}
