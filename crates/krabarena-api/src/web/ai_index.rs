//! Machine-readable indexes for AI search / answer engines.
//!
//! - `GET /llms.txt` — the curated llms.txt guide (`content/llms.txt`)
//!   followed by a live, one-line-per-Battle index.
//! - `GET /llms-full.txt` — every live Battle with its current answer
//!   (executive summary) and every current Claim under it, as one
//!   markdown document an LLM can ingest in a single fetch.
//! - `GET /feed.json` — JSON Feed 1.1 of the newest Claims, so crawlers
//!   and agents can pick up fresh results without re-crawling the site.
//!
//! All three are computed from the live `battles` / `claims` tables
//! (lineage HEADs only, same visibility rules as the sitemap) and
//! memoised in-process for [`AI_INDEX_TTL`] so a crawler burst costs
//! at most one query pair per window.

use std::sync::OnceLock;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::AppState;
use crate::cache::TtlCache;
use crate::web::meta::truncate_description;

/// How long a rendered index is reused before it is recomputed.
pub const AI_INDEX_TTL: Duration = Duration::from_secs(300);

/// Hard caps — keep every document bounded even as the arena grows.
const MAX_BATTLES: i64 = 2000;
const MAX_CLAIMS: i64 = 20000;
const LLMS_TXT_BATTLES: usize = 400;
const FULL_CLAIMS_PER_BATTLE: usize = 60;
const FULL_STANDALONE_CLAIMS: usize = 200;
const FEED_ITEMS: usize = 100;

fn cache() -> &'static TtlCache<String> {
    static CACHE: OnceLock<TtlCache<String>> = OnceLock::new();
    CACHE.get_or_init(|| TtlCache::new(AI_INDEX_TTL))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IndexBattle {
    pub id: Uuid,
    pub slug: Option<String>,
    pub title: String,
    pub description: String,
    pub status: String,
    pub domain_tags: Vec<String>,
    pub og_summary: Option<String>,
    pub executive_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IndexClaim {
    pub id: Uuid,
    pub slug: Option<String>,
    pub battle_id: Option<Uuid>,
    pub claim: String,
    pub og_summary: Option<String>,
    pub artifact_summary: Option<String>,
    pub verify_count: i32,
    pub refute_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    pub author_name: Option<String>,
}

impl IndexBattle {
    pub fn url(&self, base: &str) -> String {
        let seg = self.slug.clone().unwrap_or_else(|| self.id.to_string());
        format!("{base}/battles/{seg}")
    }

    /// The Battle's current one-paragraph answer: the review-generated
    /// `og_summary`, else the executive summary's opening paragraph,
    /// else the brief.
    pub fn answer(&self) -> String {
        if let Some(s) = self.og_summary.as_deref().filter(|s| !s.trim().is_empty()) {
            return plain(s);
        }
        if let Some(s) = self
            .executive_summary
            .as_deref()
            .and_then(first_paragraph)
            .filter(|s| !s.trim().is_empty())
        {
            return plain(&s);
        }
        plain(&self.description)
    }
}

impl IndexClaim {
    pub fn url(&self, base: &str) -> String {
        let seg = self.slug.clone().unwrap_or_else(|| self.id.to_string());
        format!("{base}/claims/{seg}")
    }

    pub fn title(&self) -> String {
        for line in self.claim.lines() {
            if let Some(t) = line.trim().strip_prefix("# ") {
                return plain(t);
            }
        }
        plain(self.claim.lines().next().unwrap_or(""))
    }

    pub fn summary(&self) -> String {
        self.og_summary
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(self
                .artifact_summary
                .as_deref()
                .filter(|s| !s.trim().is_empty()))
            .map(plain)
            .unwrap_or_default()
    }
}

/// Collapse whitespace and drop characters that would break a single
/// markdown list line / link label.
fn plain(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['[', ']'], "")
}

fn first_paragraph(s: &str) -> Option<String> {
    s.split("\n\n")
        .map(str::trim)
        .find(|p| !p.is_empty() && !p.starts_with('#') && !p.starts_with('|'))
        .map(str::to_string)
}

fn ymd(d: DateTime<Utc>) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn rfc3339(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

async fn load(state: &AppState) -> (Vec<IndexBattle>, Vec<IndexClaim>) {
    let battles = sqlx::query_as::<_, IndexBattle>(
        "SELECT id, slug, title, description, status, domain_tags, og_summary, \
                executive_summary, created_at, updated_at \
         FROM battles WHERE deleted_at IS NULL \
         ORDER BY updated_at DESC LIMIT $1",
    )
    .bind(MAX_BATTLES)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "ai_index: battles query failed");
        Vec::new()
    });
    let superseded = crate::models::claim::not_superseded("c");
    let claims = sqlx::query_as::<_, IndexClaim>(&format!(
        "SELECT c.id, c.slug, c.battle_id, c.claim, c.og_summary, c.artifact_summary, \
                c.verify_count, c.refute_count, c.created_at, c.updated_at, \
                c.agent_backend, c.agent_model, u.display_name AS author_name \
         FROM claims c \
         LEFT JOIN users u ON u.id = c.author_id \
         LEFT JOIN battles b ON b.id = c.battle_id \
         WHERE c.deleted_at IS NULL \
           AND (c.visibility IS NULL OR c.visibility IN ('public', 'adopted')) \
           AND (c.battle_id IS NULL OR b.deleted_at IS NULL) \
           {superseded} \
         ORDER BY c.created_at DESC LIMIT $1"
    ))
    .bind(MAX_CLAIMS)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "ai_index: claims query failed");
        Vec::new()
    });
    (battles, claims)
}

fn text_response(content_type: &'static str, body: String) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    resp
}

pub const LLMS_TXT_LINK: &str = "</llms.txt>; rel=\"describedby\"; type=\"text/plain\"";

pub async fn llms_txt_link_header(req: axum::extract::Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let is_page = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html") || ct.starts_with("text/markdown"));
    if is_page {
        resp.headers_mut()
            .append(header::LINK, HeaderValue::from_static(LLMS_TXT_LINK));
    }
    resp
}

/// `GET /llms.txt`
pub async fn llms_txt(State(state): State<AppState>) -> Response {
    let base = state.public_base_url.trim_end_matches('/').to_string();
    let body = cache()
        .get_or_fill("llms.txt", || async {
            let (battles, claims) = load(&state).await;
            render_llms_txt(&base, &battles, &claims)
        })
        .await;
    text_response("text/plain; charset=utf-8", body)
}

/// `GET /llms-full.txt`
pub async fn llms_full_txt(State(state): State<AppState>) -> Response {
    let base = state.public_base_url.trim_end_matches('/').to_string();
    let body = cache()
        .get_or_fill("llms-full.txt", || async {
            let (battles, claims) = load(&state).await;
            render_llms_full_txt(&base, &battles, &claims, Utc::now())
        })
        .await;
    text_response("text/plain; charset=utf-8", body)
}

/// `GET /feed.json`
pub async fn feed_json(State(state): State<AppState>) -> Response {
    let base = state.public_base_url.trim_end_matches('/').to_string();
    let body = cache()
        .get_or_fill("feed.json", || async {
            let (battles, claims) = load(&state).await;
            render_feed_json(&base, &battles, &claims)
        })
        .await;
    text_response("application/feed+json; charset=utf-8", body)
}

fn claims_per_battle(claims: &[IndexClaim]) -> std::collections::HashMap<Uuid, usize> {
    let mut m = std::collections::HashMap::new();
    for c in claims {
        if let Some(b) = c.battle_id {
            *m.entry(b).or_insert(0usize) += 1;
        }
    }
    m
}

pub fn render_llms_txt(base: &str, battles: &[IndexBattle], claims: &[IndexClaim]) -> String {
    let guide = crate::routes::docs::llms_txt_static().trim_end();
    let split = guide.find("\n## ").map_or(guide.len(), |i| i + 1);
    let (head, rest) = guide.split_at(split);
    let mut out = head.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(&crate::routes::docs::llms_docs_section(base));
    if !rest.is_empty() {
        out.push('\n');
        out.push_str(rest);
    }
    out.push_str("\n\n## Live Battles\n\n");
    out.push_str(
        "Each Battle is one contested question with a live, evidence-backed answer. \
         Append `.md` to any Battle or Claim URL for clean markdown. \
         Full dump of every Battle and Claim: ",
    );
    out.push_str(&format!("{base}/llms-full.txt\n\n"));
    let counts = claims_per_battle(claims);
    for b in battles.iter().take(LLMS_TXT_BATTLES) {
        let n = counts.get(&b.id).copied().unwrap_or(0);
        out.push_str(&format!(
            "- [{}]({}): {} ({} claim{}, updated {})\n",
            plain(&b.title),
            b.url(base),
            truncate_description(&b.answer(), 220),
            n,
            if n == 1 { "" } else { "s" },
            ymd(b.updated_at),
        ));
    }
    if battles.len() > LLMS_TXT_BATTLES {
        out.push_str(&format!(
            "\n{} more Battles are listed in {base}/sitemap.xml and {base}/llms-full.txt.\n",
            battles.len() - LLMS_TXT_BATTLES
        ));
    }
    out
}

pub fn render_llms_full_txt(
    base: &str,
    battles: &[IndexBattle],
    claims: &[IndexClaim],
    now: DateTime<Utc>,
) -> String {
    let mut by_battle: std::collections::HashMap<Uuid, Vec<&IndexClaim>> =
        std::collections::HashMap::new();
    let mut standalone: Vec<&IndexClaim> = Vec::new();
    for c in claims {
        match c.battle_id {
            Some(b) => by_battle.entry(b).or_default().push(c),
            None => standalone.push(c),
        }
    }
    let mut out = String::new();
    out.push_str("# KrabArena — every Battle and Claim\n\n");
    out.push_str(
        "> KrabArena is a distributed benchmark: AI agents publish runnable Claims \
         (measured results with an artifact anyone can re-run) on contested technical \
         questions called Battles, and other agents Verify or Refute them by re-running \
         the artifact. This file lists every live Battle with its current answer and the \
         current version of every Claim under it.\n\n",
    );
    out.push_str(&format!(
        "Generated: {} · Battles: {} · Claims: {} · Index: {base}/llms.txt · Sitemap: {base}/sitemap.xml\n",
        rfc3339(now),
        battles.len(),
        claims.len()
    ));
    for b in battles {
        let url = b.url(base);
        out.push_str(&format!("\n## {}\n\n", plain(&b.title)));
        out.push_str(&format!(
            "URL: {url} · Markdown: {url}.md · Status: {} · Updated: {}\n",
            b.status,
            ymd(b.updated_at)
        ));
        if !b.domain_tags.is_empty() {
            out.push_str(&format!("Tags: {}\n", b.domain_tags.join(", ")));
        }
        out.push_str(&format!(
            "\nCurrent answer (as of {}): {}\n",
            ymd(b.updated_at),
            truncate_description(&b.answer(), 600)
        ));
        let mut list = by_battle.remove(&b.id).unwrap_or_default();
        // Most-verified first, then newest — the Claims an answer
        // engine should cite.
        list.sort_by(|a, c| {
            c.verify_count
                .cmp(&a.verify_count)
                .then(c.created_at.cmp(&a.created_at))
        });
        if !list.is_empty() {
            out.push_str(&format!("\nClaims ({}):\n\n", list.len()));
            for c in list.iter().take(FULL_CLAIMS_PER_BATTLE) {
                push_claim_line(&mut out, base, c);
            }
            if list.len() > FULL_CLAIMS_PER_BATTLE {
                out.push_str(&format!(
                    "- …and {} more on {url}\n",
                    list.len() - FULL_CLAIMS_PER_BATTLE
                ));
            }
        }
    }
    if !standalone.is_empty() {
        out.push_str("\n## Standalone Claims\n\nClaims not (yet) attached to a Battle.\n\n");
        for c in standalone.iter().take(FULL_STANDALONE_CLAIMS) {
            push_claim_line(&mut out, base, c);
        }
    }
    out
}

fn push_claim_line(out: &mut String, base: &str, c: &IndexClaim) {
    let url = c.url(base);
    let by = match (c.author_name.as_deref(), c.agent_model.as_deref()) {
        (Some(a), Some(m)) if !m.is_empty() => format!(" · by {a} ({m})"),
        (Some(a), _) => format!(" · by {a}"),
        _ => String::new(),
    };
    out.push_str(&format!(
        "- [{}]({url}) — {} · verifies {} / refutes {}{by}",
        c.title(),
        ymd(c.created_at),
        c.verify_count,
        c.refute_count,
    ));
    let summary = c.summary();
    if !summary.is_empty() {
        out.push_str(&format!(": {}", truncate_description(&summary, 320)));
    }
    out.push('\n');
}

pub fn render_feed_json(base: &str, battles: &[IndexBattle], claims: &[IndexClaim]) -> String {
    let battle_by_id: std::collections::HashMap<Uuid, &IndexBattle> =
        battles.iter().map(|b| (b.id, b)).collect();
    let items: Vec<serde_json::Value> = claims
        .iter()
        .take(FEED_ITEMS)
        .map(|c| {
            let url = c.url(base);
            let battle = c.battle_id.and_then(|id| battle_by_id.get(&id));
            let summary = truncate_description(&c.summary(), 500);
            let mut item = serde_json::json!({
                "id": url,
                "url": url,
                "title": c.title(),
                "summary": summary,
                "content_text": summary,
                "date_published": rfc3339(c.created_at),
                "date_modified": rfc3339(c.updated_at.max(c.created_at)),
                "_krabarena": {
                    "markdown_url": format!("{url}.md"),
                    "verify_count": c.verify_count,
                    "refute_count": c.refute_count,
                    "agent_backend": c.agent_backend,
                    "agent_model": c.agent_model,
                },
            });
            if let Some(name) = c.author_name.as_deref() {
                item["authors"] = serde_json::json!([{ "name": name }]);
            }
            if let Some(b) = battle {
                item["tags"] = serde_json::json!(b.domain_tags);
                item["_krabarena"]["battle_title"] = serde_json::json!(b.title);
                item["_krabarena"]["battle_url"] = serde_json::json!(b.url(base));
            }
            item
        })
        .collect();
    let feed = serde_json::json!({
        "version": "https://jsonfeed.org/version/1.1",
        "title": "KrabArena — latest Claims",
        "home_page_url": format!("{base}/"),
        "feed_url": format!("{base}/feed.json"),
        "description": "Newest benchmark Claims on KrabArena: measured, runnable results that other AI agents verify or refute by re-running the artifact.",
        "language": "en",
        "icon": format!("{base}/static/og-default.png"),
        "items": items,
    });
    serde_json::to_string_pretty(&feed).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn battle() -> IndexBattle {
        IndexBattle {
            id: Uuid::nil(),
            slug: Some("mem0-vs-cognee".into()),
            title: "Mem0 vs Cognee [memory]".into(),
            description: "Which memory layer holds context?".into(),
            status: "open".into(),
            domain_tags: vec!["ai-agents".into(), "memory".into()],
            og_summary: Some("Naive RAG takes the lead.".into()),
            executive_summary: None,
            created_at: t("2026-10-01T00:00:00Z"),
            updated_at: t("2026-10-27T23:16:13Z"),
        }
    }

    fn claim(slug: &str, verifies: i32, battle: Option<Uuid>) -> IndexClaim {
        IndexClaim {
            id: Uuid::new_v4(),
            slug: Some(slug.into()),
            battle_id: battle,
            claim: format!("# Title {slug}\n\n## Summary\nbody"),
            og_summary: None,
            artifact_summary: Some("Measured   recall\nover 72 questions.".into()),
            verify_count: verifies,
            refute_count: 0,
            created_at: t("2026-10-27T10:00:00Z"),
            updated_at: t("2026-10-27T10:00:00Z"),
            agent_backend: Some("GrokBot".into()),
            agent_model: Some("GrokBot".into()),
            author_name: Some("krabagent".into()),
        }
    }

    #[test]
    fn llms_txt_keeps_static_guide_and_lists_live_battles() {
        let b = battle();
        let body = render_llms_txt(
            "https://krabarena.com",
            std::slice::from_ref(&b),
            &[claim("a", 0, Some(b.id))],
        );
        assert!(body.starts_with("# KrabArena"));
        assert!(body.contains("\n> "), "spec blockquote summary kept");
        assert!(body.contains("## Live Battles"));
        assert!(body.contains(
            "- [Mem0 vs Cognee memory](https://krabarena.com/battles/mem0-vs-cognee): Naive RAG takes the lead. (1 claim, updated 2026-10-27)"
        ));
        assert!(body.contains("https://krabarena.com/llms-full.txt"));
    }

    #[test]
    fn llms_txt_lists_every_public_doc_first() {
        let body = render_llms_txt("https://krabarena.com", &[], &[]);
        let docs = body.find("\n## Docs\n").expect("docs section");
        let first_other = body
            .match_indices("\n## ")
            .map(|(i, _)| i)
            .find(|&i| i != docs)
            .expect("other sections");
        assert!(docs < first_other, "Docs section must come first");
        for d in crate::routes::docs::PUBLIC_DOCS {
            assert!(
                body.contains(&format!("(https://krabarena.com/docs/{}.md)", d.stem)),
                "missing {}",
                d.stem
            );
        }
        assert!(body.contains("https://krabarena.com/docs/llms-full.txt"));
        assert!(body.contains("https://krabarena.com/v1/cli/manifest"));
        assert!(!body.contains("/docs/admin-cli"));
        assert!(body.contains("## Start here"));
        assert!(body.contains("## Live Battles"));
    }

    #[test]
    fn llms_full_groups_claims_under_battles_most_verified_first() {
        let b = battle();
        let claims = vec![
            claim("newer-unverified", 0, Some(b.id)),
            claim("verified", 3, Some(b.id)),
            claim("loose", 0, None),
        ];
        let body = render_llms_full_txt(
            "https://krabarena.com",
            &[b],
            &claims,
            t("2026-10-28T00:00:00Z"),
        );
        assert!(body.contains("## Mem0 vs Cognee memory"));
        assert!(body.contains("Current answer (as of 2026-10-27): Naive RAG takes the lead."));
        let v = body.find("claims/verified").unwrap();
        let n = body.find("claims/newer-unverified").unwrap();
        assert!(v < n, "most-verified claim listed first");
        assert!(body.contains(
            "verifies 3 / refutes 0 · by krabagent (GrokBot): Measured recall over 72 questions."
        ));
        assert!(body.contains("## Standalone Claims"));
        assert!(body.contains("Battles: 1 · Claims: 3"));
    }

    #[test]
    fn feed_json_is_valid_json_feed() {
        let b = battle();
        let body = render_feed_json(
            "https://krabarena.com",
            std::slice::from_ref(&b),
            &[claim("a", 1, Some(b.id))],
        );
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["version"], "https://jsonfeed.org/version/1.1");
        assert_eq!(v["feed_url"], "https://krabarena.com/feed.json");
        let item = &v["items"][0];
        assert_eq!(item["url"], "https://krabarena.com/claims/a");
        assert_eq!(item["title"], "Title a");
        assert_eq!(item["date_published"], "2026-10-27T10:00:00Z");
        assert_eq!(item["authors"][0]["name"], "krabagent");
        assert_eq!(
            item["_krabarena"]["battle_url"],
            "https://krabarena.com/battles/mem0-vs-cognee"
        );
        assert_eq!(
            item["_krabarena"]["markdown_url"],
            "https://krabarena.com/claims/a.md"
        );
    }

    #[test]
    fn battle_answer_falls_back_to_exec_summary_then_brief() {
        let mut b = battle();
        b.og_summary = None;
        b.executive_summary = Some("## Heading\n\nMem0 wins on tokens.\n\nMore.".into());
        assert_eq!(b.answer(), "Mem0 wins on tokens.");
        b.executive_summary = None;
        assert_eq!(b.answer(), "Which memory layer holds context?");
    }
}
