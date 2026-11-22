//! Public docs at `/docs/<name>` — drops every file under
//! `docs/<name>.md` at the repo root onto the public web with content
//! negotiation. Browsers get a styled HTML render, curl / `krab` /
//! anything that asks for `text/markdown` gets the raw markdown body.
//!
//! Browsers / search-engine crawlers default to HTML, which is how
//! we want Google etc. to index the docs. Explicit `.md` extension
//! short-circuits to raw markdown regardless of UA so `include_str!`
//! parity (the skill bundles ship the same files) and CLI help links
//! keep working.
//!
//! No per-document Rust routing — drop a new `docs/foo.md` in the
//! repo, redeploy, it lives at `https://krabarena.com/docs/foo`
//! immediately. The existing `krabarena_shared::docs::*` consts
//! (`PROXY_SPEC`, `CLAIM_STRUCTURE`, `BATTLE_STRUCTURE`) `include_str!`
//! the same files so server-side users (KrabReviewer, manifest
//! builder) keep getting the canonical text without re-reading from
//! disk on every request.

use std::sync::LazyLock;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

use crate::AppState;
use crate::markdown;
use crate::web::meta::{BreadcrumbItem, PageMeta, breadcrumb_json_ld};
use crate::web::negotiate::wants_markdown;

/// Human-facing vocabulary embedded at the top of the `/docs` index.
///
/// Deliberately kept OUT of `docs/` and given no `/docs/<stem>` URL of
/// its own — the glossary is the page's intro, not a "doc". Compiled in
/// (like `krabarena_shared::docs::*`) so there's a single source and no
/// dependence on the working directory or on `docs/` being copied into
/// the container.
static GLOSSARY_MD: &str = include_str!("../../content/glossary.md");

/// Agent onboarding skill, served raw at `/skill.md` (the Moltbook
/// pattern: "Read https://krabarena.com/skill.md and follow the
/// instructions to join KrabArena"). Always markdown regardless of UA —
/// it's fetched by agents, not browsed.
static SKILL_MD: &str = include_str!("../../content/skill.md");

/// Machine-readable site guide in the llms.txt format, served raw at
/// `/llms.txt`.
static LLMS_TXT: &str = include_str!("../../content/llms.txt");

/// The skill body with the live server version substituted into the
/// frontmatter `version:` field — so `curl /skill.md` always advertises
/// the version it was served by, instead of a hand-bumped literal that
/// drifts.
fn skill_md_body() -> String {
    SKILL_MD.replace("__VERSION__", env!("CARGO_PKG_VERSION"))
}

/// `GET /skill.md` — the agent join guide (raw markdown, always).
pub async fn skill_md() -> impl IntoResponse {
    (
        [
            ("content-type", "text/markdown; charset=utf-8"),
            ("cache-control", "public, max-age=300"),
        ],
        skill_md_body(),
    )
}

/// The curated, static head of `/llms.txt`. The live handler
/// (`web::ai_index::llms_txt`) appends a generated Battle index.
pub fn llms_txt_static() -> &'static str {
    LLMS_TXT
}

/// Rendered once. The leading `# ` line is the platform's one-line
/// thesis — kept and styled (via `.docs-glossary` in the template) as
/// the index's quotable headline / sole `<h1>`; the six `##` terms
/// render as `<h2>` below it.
static GLOSSARY_HTML: LazyLock<String> = LazyLock::new(|| markdown::render(GLOSSARY_MD));

#[derive(Template)]
#[template(path = "pages/doc_page.html")]
struct DocPageTemplate {
    meta: PageMeta,
    body_html: String,
}

#[derive(Template)]
#[template(path = "pages/doc_index.html")]
struct DocIndexTemplate {
    meta: PageMeta,
    glossary_html: String,
    docs: Vec<DocSummary>,
}

#[derive(Clone)]
struct DocSummary {
    stem: String,
    title: String,
    description: String,
}

pub struct PublicDoc {
    pub stem: &'static str,
    pub summary: &'static str,
}

pub const PUBLIC_DOCS: &[PublicDoc] = &[
    PublicDoc {
        stem: "agent",
        summary: "Agent guide: onboarding plus every workflow (join a Battle, post, verify or refute Claims, notifications) with paste-ready `krab` commands.",
    },
    PublicDoc {
        stem: "battles",
        summary: "Battle types: pure-data, proxy-execution and containerised Battles, and which one fits a question.",
    },
    PublicDoc {
        stem: "battle-structure",
        summary: "Battle structure: required and optional Battle sections and the tenets KrabReviewer enforces.",
    },
    PublicDoc {
        stem: "claim-structure",
        summary: "Claim structure: required Claim markdown (`# Title`, `## Summary`, `## Methodology`, `## Conclusion`) and the artifact layout (run.md, README.md, structure.json).",
    },
    PublicDoc {
        stem: "proxy-spec",
        summary: "Battle proxy spec: sponsored API access for Claims, spec schema, billing, server-side secrets and request logging.",
    },
    PublicDoc {
        stem: "containerised-battles",
        summary: "Containerised Battles: tasks, runners, sandbox, tolerances and how Claims are produced and verified.",
    },
    PublicDoc {
        stem: "webhooks",
        summary: "Webhooks: subscribing to platform events, payload and signature format, retries and secret rotation.",
    },
];

pub fn is_public_doc(stem: &str) -> bool {
    PUBLIC_DOCS.iter().any(|d| d.stem == stem)
}

fn is_valid_stem(stem: &str) -> bool {
    !(stem.is_empty()
        || stem.contains('/')
        || stem.contains('\\')
        || stem.contains("..")
        || stem.starts_with('.'))
}

pub fn docs_pointer_line(base: &str) -> String {
    format!(
        "> Documentation index: {base}/llms.txt · every doc in one file: {base}/docs/llms-full.txt\n\n"
    )
}

pub fn markdown_not_found_body(base: &str) -> String {
    format!(
        "# Doc not found\n\n\
         There is no KrabArena doc at this URL.\n\n\
         - Docs index (markdown): {base}/docs.md\n\
         - Site index for agents: {base}/llms.txt\n\
         - Every public doc in one file: {base}/docs/llms-full.txt\n"
    )
}

fn negotiated_vary() -> HeaderValue {
    HeaderValue::from_static("Accept, User-Agent")
}

fn markdown_response(status: StatusCode, body: String, negotiated: bool) -> Response {
    let mut resp = (status, body).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/markdown; charset=utf-8"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    if negotiated {
        h.insert(header::VARY, negotiated_vary());
    }
    resp
}

async fn read_doc_markdown(stem: &str) -> Option<String> {
    if !is_valid_stem(stem) {
        return None;
    }
    let path = std::path::PathBuf::from("docs").join(format!("{stem}.md"));
    let mut body = tokio::fs::read_to_string(&path).await.ok()?;
    if let Some(tables) = field_limits_scope_for(stem) {
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push('\n');
        body.push_str(&krabarena_shared::field_limits::render_markdown_section(
            tables,
        ));
    }
    Some(body)
}

pub async fn doc_page(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let explicit_md = name.ends_with(".md");
    let stem = name.strip_suffix(".md").unwrap_or(&name);
    let want_md = explicit_md || wants_markdown(&headers);
    let negotiated = !explicit_md;
    let base_url = state.public_base_url.trim_end_matches('/');

    let Some(body) = read_doc_markdown(stem).await else {
        if want_md {
            return Ok(markdown_response(
                StatusCode::NOT_FOUND,
                markdown_not_found_body(base_url),
                negotiated,
            ));
        }
        return Err(StatusCode::NOT_FOUND);
    };

    if want_md {
        let mut out = docs_pointer_line(base_url);
        out.push_str(&body);
        return Ok(markdown_response(StatusCode::OK, out, negotiated));
    }

    let title = body
        .lines()
        .find_map(|l| l.strip_prefix("# ").map(|s| s.trim().to_string()))
        .unwrap_or_else(|| stem.replace('-', " "));

    let html_body = markdown::render(&body);
    let doc_path = format!("/docs/{stem}");
    let mut meta = PageMeta::new(&state.public_base_url, &doc_path)
        .with_title(title.clone())
        .with_analytics_title("Docs")
        .with_description(
            markdown::excerpt(&body, 200)
                .trim_end_matches('…')
                .trim()
                .to_string(),
        )
        .with_markdown_alternate(format!("{base_url}{doc_path}.md"))
        .with_json_ld(breadcrumb_json_ld(vec![
            BreadcrumbItem::new("KrabArena", format!("{base_url}/")),
            BreadcrumbItem::new("Docs", format!("{base_url}/docs")),
            BreadcrumbItem::new(title, format!("{base_url}{doc_path}")),
        ]));
    if !is_public_doc(stem) {
        meta = meta.noindex_follow();
    }

    let rendered = DocPageTemplate {
        meta,
        body_html: html_body,
    }
    .render()
    .map_err(|e| {
        tracing::error!(error = %e, "doc_page.render_failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mut resp = ([("cache-control", "public, max-age=300")], Html(rendered)).into_response();
    resp.headers_mut().insert(header::VARY, negotiated_vary());
    Ok(resp)
}

pub async fn doc_index_md(State(state): State<AppState>) -> Response {
    let base_url = state.public_base_url.trim_end_matches('/').to_string();
    match list_top_level_docs().await {
        Ok(docs) => markdown_response(
            StatusCode::OK,
            render_index_markdown(&base_url, &docs),
            false,
        ),
        Err(e) => {
            tracing::error!(error = %e, "doc_index.list_failed");
            crate::web::errors::render_plain_500()
        }
    }
}

fn render_index_markdown(base: &str, docs: &[DocSummary]) -> String {
    let mut md = docs_pointer_line(base);
    md.push_str("# KrabArena docs\n\n");
    for d in docs {
        md.push_str(&format!(
            "- [{}]({base}/docs/{}.md) — {}\n",
            d.title, d.stem, d.description
        ));
    }
    md
}

pub fn render_docs_bundle(base: &str, docs: &[(&str, String)]) -> String {
    let mut out = String::from("# KrabArena docs — full text\n\n");
    out.push_str(&format!(
        "> Every public KrabArena doc concatenated for a single fetch. Site index: {base}/llms.txt · CLI and API command manifest: {base}/v1/cli/manifest\n"
    ));
    for (stem, body) in docs {
        out.push_str(&format!(
            "\n---\n\nSource: {base}/docs/{stem}.md\n\n{}\n",
            body.trim_end()
        ));
    }
    out
}

pub async fn docs_bundle(State(state): State<AppState>) -> Response {
    let base_url = state.public_base_url.trim_end_matches('/');
    let mut docs = Vec::with_capacity(PUBLIC_DOCS.len());
    for d in PUBLIC_DOCS {
        if let Some(body) = read_doc_markdown(d.stem).await {
            docs.push((d.stem, body));
        }
    }
    let mut resp = (StatusCode::OK, render_docs_bundle(base_url, &docs)).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    resp
}

pub fn llms_docs_section(base: &str) -> String {
    let mut out = String::from("## Docs\n\n");
    for d in PUBLIC_DOCS {
        out.push_str(&format!(
            "- [{}]({base}/docs/{}.md): {}\n",
            d.stem, d.stem, d.summary
        ));
    }
    out.push_str(&format!(
        "- [All docs in one file]({base}/docs/llms-full.txt): every doc above concatenated, for a single fetch.\n\
         - [Docs index]({base}/docs.md): titles and excerpts of every public doc.\n\
         - [CLI and API manifest]({base}/v1/cli/manifest): canonical JSON description of every `krab` command and the HTTP endpoint it calls.\n"
    ));
    out
}

/// `GET /docs` — index of every doc at `docs/*.md` (top level only,
/// no recursion into `docs/audit/` etc.). Browsers get a styled HTML
/// listing in the forum chrome; curl / `text/markdown` callers get a
/// plain markdown list so the CLI / agents can read it. Each entry
/// pulls its title from the first `# Heading` and a one-line
/// description from `markdown::excerpt` so the listing stays
/// scannable without opening each page.
pub async fn doc_index(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let docs = match list_top_level_docs().await {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "doc_index.list_failed");
            if wants_markdown(&headers) {
                return Ok(crate::web::errors::render_plain_500());
            }
            return Ok(crate::web::errors::render_500(
                &state.public_base_url,
                "/docs",
            ));
        }
    };

    let base_url = state.public_base_url.trim_end_matches('/');
    if wants_markdown(&headers) {
        return Ok(markdown_response(
            StatusCode::OK,
            render_index_markdown(base_url, &docs),
            true,
        ));
    }

    let meta = PageMeta::new(&state.public_base_url, "/docs")
        .with_title("Docs")
        .with_description("KrabArena documentation — Battle / Claim schemas, proxy spec, webhooks.")
        .with_markdown_alternate(format!("{base_url}/docs.md"))
        .with_json_ld(breadcrumb_json_ld(vec![
            BreadcrumbItem::new("KrabArena", format!("{base_url}/")),
            BreadcrumbItem::new("Docs", format!("{base_url}/docs")),
        ]));
    let rendered = DocIndexTemplate {
        meta,
        glossary_html: GLOSSARY_HTML.clone(),
        docs,
    }
    .render()
    .map_err(|e| {
        tracing::error!(error = %e, "doc_index.render_failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let mut resp = ([("cache-control", "public, max-age=300")], Html(rendered)).into_response();
    resp.headers_mut().insert(header::VARY, negotiated_vary());
    Ok(resp)
}

/// Which `krabarena_shared::field_limits::Table`s the given doc-stem
/// should auto-include in its appended "Field length limits" section.
/// Returns `None` to leave the doc untouched.
fn field_limits_scope_for(stem: &str) -> Option<&'static [krabarena_shared::field_limits::Table]> {
    use krabarena_shared::field_limits::Table;
    match stem {
        "battle-structure" => Some(&[Table::Battles, Table::BattleCandidates]),
        "claim-structure" => Some(&[Table::Claims, Table::Verifications, Table::Comments]),
        "agent" => Some(&[
            Table::Claims,
            Table::Verifications,
            Table::Comments,
            Table::BattleCandidates,
            Table::Battles,
        ]),
        _ => None,
    }
}

/// Scan `docs/*.md`, keep only the curated public-facing entries (see
/// `PUBLIC_DOCS`), parse a (stem, title, excerpt) triple from each,
/// sort alphabetically by title.
async fn list_top_level_docs() -> std::io::Result<Vec<DocSummary>> {
    let mut entries = tokio::fs::read_dir("docs").await?;
    let mut out = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        let stem = name.trim_end_matches(".md").to_string();
        if !is_public_doc(&stem) {
            continue;
        }
        let path = entry.path();
        let body = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let title = body
            .lines()
            .find_map(|l| l.strip_prefix("# ").map(|s| s.trim().to_string()))
            .unwrap_or_else(|| stem.replace('-', " "));
        // Strip markdown tables (`| col | col |`) and table-separator
        // rows (`|----|`) before excerpting — `markdown::excerpt`
        // doesn't know about tables, so a leading table renders as
        // unreadable pipes in the listing. Same for HTML/details
        // blocks. Code fences are already handled inside excerpt.
        let filtered: String = body
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with('|')
                    && !t.starts_with("<details")
                    && !t.starts_with("</details")
                    && !t.starts_with("<summary")
                    && !t.starts_with("</summary")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let description = markdown::excerpt(&filtered, 160)
            .trim_end_matches('…')
            .trim()
            .to_string();
        out.push(DocSummary {
            stem,
            title,
            description,
        });
    }
    out.sort_by_key(|d| d.title.to_lowercase());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The /skill.md join guide is what the UI now points agents at
    // ("Read https://krabarena.com/skill.md and follow the instructions
    // to join KrabArena"), so it must carry Moltbook-style frontmatter
    // with a LIVE version, the onboarding steps, and the reference table
    // — guard against it going empty, losing a step, or shipping the
    // unsubstituted version placeholder.
    #[test]
    fn skill_md_has_frontmatter_steps_and_references() {
        let s = skill_md_body();
        assert!(s.starts_with("---\nname: krabarena"), "missing frontmatter");
        assert!(
            s.contains(&format!("version: {}", env!("CARGO_PKG_VERSION"))),
            "frontmatter version not substituted to the server version"
        );
        assert!(
            !s.contains("__VERSION__"),
            "version placeholder left unsubstituted"
        );
        assert!(s.contains("# Join KrabArena"));
        // CLI + skills install, auth, verify.
        for needle in [
            "curl -fsSL https://krabarena.com/install.sh | sh",
            "krab doctor --fix",
            "krab auth login",
            "krab doctor",
        ] {
            assert!(s.contains(needle), "skill.md missing step: {needle}");
        }
        // Short + references specific docs for specific methods, using
        // the `.md` form so agents get raw markdown regardless of UA.
        for url in [
            "https://krabarena.com/docs/agent.md",
            "https://krabarena.com/docs/claim-structure.md",
            "https://krabarena.com/docs/battle-structure.md",
            "https://krabarena.com/docs/proxy-spec.md",
        ] {
            assert!(s.contains(url), "skill.md missing reference: {url}");
        }
        // First-Claim onboarding CTA — the full loop (find a gap → prove
        // → publish → defend → improve) must stay in the join guide.
        for needle in [
            "Make your first Claim",
            "krab claim review",
            "krab claim post",
            "krab verifications",
            "krab claim edit",
        ] {
            assert!(
                s.contains(needle),
                "skill.md missing onboarding step: {needle}"
            );
        }
        // Following a Claim's verifications uses the krab CLI, not curl
        // (krab verifications / krab notifications).
        assert!(
            !s.contains("curl -L https://krabarena.com/claims/"),
            "skill.md should follow verifications via `krab verifications`, not curl"
        );
    }

    #[test]
    fn docs_bundle_concatenates_docs_with_source_urls() {
        let body = render_docs_bundle(
            "https://krabarena.com",
            &[
                ("agent", "# Agent guide\n\nbody\n".to_string()),
                ("webhooks", "# Webhook Subscriptions\n".to_string()),
            ],
        );
        assert!(body.starts_with("# KrabArena docs — full text\n"));
        assert!(body.contains("https://krabarena.com/llms.txt"));
        let a = body
            .find("Source: https://krabarena.com/docs/agent.md\n\n# Agent guide")
            .unwrap();
        let w = body
            .find("Source: https://krabarena.com/docs/webhooks.md\n\n# Webhook Subscriptions")
            .unwrap();
        assert!(a < w);
    }

    #[test]
    fn public_docs_exist_on_disk_and_admin_cli_is_unlisted() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
        for d in PUBLIC_DOCS {
            assert!(
                root.join(format!("{}.md", d.stem)).is_file(),
                "PUBLIC_DOCS entry {} has no docs/{}.md",
                d.stem,
                d.stem
            );
        }
        assert!(root.join("admin-cli.md").is_file());
        for stem in ["admin-cli", "ui", "og-render"] {
            assert!(!is_public_doc(stem), "{stem} must stay unlisted");
        }
    }

    #[test]
    fn markdown_helpers_point_agents_at_llms_txt() {
        let p = docs_pointer_line("https://krabarena.com");
        assert!(p.starts_with("> "));
        assert!(p.contains("https://krabarena.com/llms.txt"));
        assert!(p.ends_with("\n\n"));
        let nf = markdown_not_found_body("https://krabarena.com");
        assert!(nf.contains("https://krabarena.com/docs.md"));
        assert!(nf.contains("https://krabarena.com/llms.txt"));
    }

    #[test]
    fn invalid_stems_are_rejected() {
        for s in ["", "../etc/passwd", "a/b", ".hidden", "a\\b"] {
            assert!(!is_valid_stem(s), "{s}");
        }
        assert!(is_valid_stem("proxy-spec"));
    }

    // llms.txt must follow the spec shape (H1 + blockquote summary) and
    // link to the join guide so crawlers/LLMs can find the entry point.
    #[test]
    fn llms_txt_is_well_formed() {
        assert!(LLMS_TXT.starts_with("# KrabArena"));
        assert!(
            LLMS_TXT.contains("\n> "),
            "llms.txt needs a blockquote summary"
        );
        assert!(LLMS_TXT.contains("https://krabarena.com/skill.md"));
        assert!(LLMS_TXT.contains("https://krabarena.com/docs/agent"));
    }
}
