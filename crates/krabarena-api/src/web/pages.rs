use askama::Template;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::markdown::filters;
use crate::models::battle::{BATTLE_COLUMNS, BattleRow, parse_solutions};
use crate::models::claim::{CLAIM_COLUMNS, ClaimRow};
use crate::models::comment::CommentRow;
use crate::models::verification::VerificationRow;
use crate::services::format_metric::format_metric_compact;
use crate::web::meta::{
    BreadcrumbItem, CreativeWorkExtras, PageMeta, breadcrumb_json_ld, creative_work_json_ld_rich,
    website_json_ld,
};
use crate::web::negotiate::wants_markdown;
use krabarena_shared::presentation::PresentationConfig;

/// Render either the HTML template or the Markdown template based on
/// the caller's `Accept` header and User-Agent. Adds `Vary: Accept,
/// User-Agent` so caches don't mix the two representations.
fn render_negotiated<H: Template, M: Template>(
    public_base_url: &str,
    path: &str,
    headers: &HeaderMap,
    html: H,
    md: M,
) -> Response {
    let vary = HeaderValue::from_static("Accept, User-Agent");
    if wants_markdown(headers) {
        let body = match crate::web::errors::render_template_body(md, "web.markdown") {
            Some(body) => body,
            None => {
                let mut resp = crate::web::errors::render_plain_500();
                resp.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                resp.headers_mut().insert(header::VARY, vary);
                return resp;
            }
        };
        let mut resp = (StatusCode::OK, body).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/markdown; charset=utf-8"),
        );
        resp.headers_mut().insert(header::VARY, vary);
        resp
    } else {
        let mut resp =
            crate::web::errors::render_template_or_500(html, public_base_url, path, "web.html");
        resp.headers_mut().insert(header::VARY, vary);
        resp
    }
}

fn canonical_slug_location(prefix: &str, slug: &str, raw_query: Option<&str>) -> String {
    let path = format!("{prefix}/{slug}");
    match raw_query {
        Some(query) if !query.is_empty() => format!("{path}?{query}"),
        _ => path,
    }
}

fn canonical_slug_redirect(prefix: &str, slug: &str, raw_query: Option<&str>) -> Response {
    let has_query = raw_query.is_some_and(|query| !query.is_empty());
    let location = canonical_slug_location(prefix, slug, raw_query);
    let mut response = axum::response::Redirect::permanent(&location).into_response();
    if has_query {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

/// Strip a trailing `.md` from a Battle / Claim path segment. When
/// present, force the markdown representation by rewriting `Accept`
/// (slugs are `[a-z0-9-]`, so a literal `.md` can never be part of one).
fn split_markdown_suffix(raw: String, mut headers: HeaderMap) -> (String, HeaderMap, bool) {
    match raw.strip_suffix(".md") {
        Some(stem) if !stem.is_empty() => {
            headers.insert(header::ACCEPT, HeaderValue::from_static("text/markdown"));
            (stem.to_string(), headers, true)
        }
        _ => (raw, headers, false),
    }
}

fn markdown_suffixed(slug: &str, md: bool) -> String {
    if md {
        format!("{slug}.md")
    } else {
        slug.to_string()
    }
}

/// `Link: <canonical>; rel="canonical"` so the markdown twin (and any
/// query-string variant) consolidates onto the HTML page's URL instead
/// of being indexed as duplicate content.
fn with_canonical_link(mut resp: Response, canonical_url: &str) -> Response {
    if resp.status() == StatusCode::OK {
        if let Ok(v) = HeaderValue::from_str(&format!("<{canonical_url}>; rel=\"canonical\"")) {
            resp.headers_mut().insert(header::LINK, v);
        }
    }
    resp
}

// --- View models (Polymarket-style data dense) ---

#[derive(Debug, Clone)]
pub struct BattleCard {
    pub id: String,
    /// SEO slug if one was generated for this battle (every battle
    /// since 0.7 has one); used for the `b/<slug>` badge label and
    /// preferred over the raw UUID for any user-facing href. `None`
    /// for synthetic / preview cards built without a slug.
    pub slug: Option<String>,
    pub title: String,
    pub description: String,
    pub status: String,
    pub author_display_name: Option<String>,
    pub domain_tags: Vec<String>,
    pub claim_count: i64,
    pub total_verify: i64,
    pub total_refute: i64,
    pub verify_pct: u32,
    pub refute_pct: u32,
    pub sponsor_domain: Option<String>,
    pub executive_summary: Option<String>,
    /// `### Notes` + `### What's missing` sections split out of
    /// `executive_summary` at render time. Shown in a separate
    /// collapsed block on the battle page.
    pub executive_notes: Option<String>,
    /// Compact (~200 char + top-3 leaderboard) version of
    /// `executive_summary`, generated by the same battle_summarizer
    /// pass since 0.8.20. Rendered on the home page / battle-list
    /// cards where the full editorial would crowd the layout.
    /// `None` for battles that haven't run a summarizer yet.
    pub short_summary: Option<String>,
    pub proxy_services: Vec<String>,
    pub proxy_budget_cents: i32,
    pub proxy_remaining_cents: i32,
    pub proxy_total_requests: i32,
    /// Per-service usage rows for the Sponsored Budget breakdown
    /// table on the battle page. Empty when the battle has no proxy
    /// configured (template uses `is_empty()` to suppress the
    /// breakdown block).
    pub proxy_per_service: Vec<ProxyServiceRow>,
    /// Pretty-formatted total spend (e.g. `"$0.55"` / `"$1.2300"`)
    /// for the Sponsored Budget label. Computed from the same
    /// per-service rows so the headline matches the table.
    pub proxy_spent_label: String,
    /// Pretty-formatted total budget (e.g. `"$50.00"`). Same
    /// formatter as `proxy_spent_label` so the two read consistently.
    pub proxy_budget_label: String,
    /// Live temperature score and bucket. The score is what the
    /// service computed; the bucket is the human projection used
    /// for chip color / emoji.
    pub temperature: f32,
    pub temp_class: &'static str,
    pub temp_emoji: &'static str,
    pub temp_label: &'static str,
    /// Aggregated total tokens across every claim attached to this
    /// battle: `SUM(claims.tokens_consumed) + SUM(prompt_tokens +
    /// completion_tokens FROM claim_reviews)`. Combined into a single
    /// counter because the battle card has limited horizontal room.
    /// Populated by `apply_cost_agg`; defaults to 0 (chip suppressed).
    pub agg_tokens: i64,
    /// Combined dollar total for this battle: author compute
    /// (`price_cents/100`) + external API spend
    /// (`claim_structure.cost.external_usd`) + KrabReview spend
    /// (`claim_reviews.cost_micros / 1_000_000`).
    pub agg_cost_usd: f64,
    /// Parsed `short_summary` panels (Top 3 leaderboard + Top
    /// metrics + editor notes). The home-page card renders these as
    /// structured panels matching the forum redesign reference; falls
    /// back to the markdown-only summary when this is empty.
    pub short_summary_view: ShortSummaryView,
    /// Editor-uploaded hero image (migration 032). Rendered above
    /// the title on the Battle detail page; ignored on list cards.
    /// `None` for battles without a cover (the historical default).
    pub cover_image_url: Option<String>,
    /// Accessible alt text for `cover_image_url` (Gemini-generated
    /// or filename-derived).
    pub cover_image_alt: Option<String>,
    /// Up to 3 vendor logos drawn from `battles.solutions` (since
    /// 0.13.4). Rendered as a horizontal strip on the home-page card
    /// — purely decorative (the card itself is the link). Empty
    /// vec means no logo strip is shown. Order follows the
    /// solutions array order in the database; ties are broken by
    /// insertion order.
    pub top_logos: Vec<BattleCardLogo>,
    /// Resolved "produced by" brands for the interactive top-right label
    /// (home card + detail hero); empty when the battle has no producers.
    /// Resolved from the `battles.produced_by` slug array via
    /// [`crate::models::producer::resolve_all`]; the `'static` refs mean
    /// the template reads name/description/website/logo with no per-card
    /// allocation.
    pub produced_by: Vec<&'static crate::models::producer::Producer>,
    /// Home feed card timestamp ("last claim 2h ago" / "opened 3d ago").
    /// `None` outside the home feed (battle page, list previews).
    pub feed_time: Option<FeedTime>,
}

/// One vendor logo entry rendered on a home-page Battle card (since
/// 0.13.4). Sourced from `battles.solutions[*].logo_url` —
/// decorative, no link of its own (the wrapping card already routes
/// to the battle page). Carries both the URL and the matching
/// solution name so the `<img alt="…">` attribute reads as the
/// vendor / product name for accessibility.
#[derive(Debug, Clone)]
pub struct BattleCardLogo {
    pub url: String,
    pub name: String,
}

/// A Claim rendered inline in the unified home feed (0.20.0). Compact:
/// just enough to show a ranked Claim alongside a Battle card and link
/// through. Carries the parent Battle's title/slug for context.
#[derive(Debug, Clone)]
pub struct FeedClaimCard {
    /// Slug (preferred) or UUID — used in the `/claims/<id>` href.
    pub id: String,
    pub claim_title: String,
    pub battle_title: String,
    /// Slug or UUID of the parent battle for the `b/<…>` badge link.
    /// Empty string for standalone (battle-less) claims.
    pub battle_slug: String,
    pub author_display_name: Option<String>,
    pub agent_backend: String,
    pub agent_model: String,
    pub verify_count: i32,
    pub refute_count: i32,
    /// Pretty `$x.xx` author-compute dollars spent on this claim, for
    /// the chip. Empty string when zero (chip suppressed).
    pub cost_label: String,
    /// "posted 3h ago" timestamp (claim `created_at`).
    pub time: FeedTime,
    /// Standalone-claim topic (the question it answers). Empty for
    /// battle-attached claims, which take their context from the
    /// parent Battle instead. Drives the Claims-tab card subtitle.
    pub topic: String,
    /// Standalone-claim topic tags — clickable `#tag` chips on the
    /// Claims tab that deep-link to `/t/<tag>`. Empty for attached
    /// claims (they inherit context from the Battle badge).
    pub topic_tags: Vec<String>,
    /// Resolved "produced by" brands for the top-right label (empty when
    /// the claim has no producers). Resolved from `claims.produced_by` via
    /// [`crate::models::producer::resolve_all`]. Mirrors `BattleCard`.
    pub produced_by: Vec<&'static crate::models::producer::Producer>,
    /// Parsed benchmark label + top-3 leaderboard, so the home claims
    /// feed renders decision cards (verdict pill + mini-leaderboard +
    /// favicons) matching the battle-page claim cards instead of bare
    /// titles. Empty when the claim has no parsed structure.
    pub benchmark_label: String,
    pub benchmark_metric: String,
    pub leaderboard: Vec<LeaderboardRowView>,
}

/// A Verification rendered as a card in the home Verifications tab
/// (discovery redesign). A verification is the outcome of someone
/// running a Claim's artifact (`confirmed`/`disconfirmed`); the card
/// links straight to the parent Claim. Ranked by hotness — recency
/// with refutes weighted heavier (see the verifications query in
/// `index()`).
#[derive(Debug, Clone)]
pub struct VerificationCard {
    /// Slug (preferred) or UUID of the verified Claim — the card href.
    pub claim_id: String,
    pub claim_title: String,
    /// `true` for `confirmed`, `false` for `disconfirmed` — picks the
    /// ✓/✗ glyph and the verified/refuted verb in the template.
    pub is_confirmed: bool,
    pub author_display_name: Option<String>,
    pub agent_backend: String,
    pub agent_model: String,
    /// One-line evidence excerpt: `failure_reason` for a refute,
    /// `environment` for a verify. Empty string suppresses the line.
    pub detail: String,
    /// Parent Battle context (empty for verifications on a standalone
    /// claim). `battle_slug` is the `b/<…>` badge link target.
    pub battle_title: String,
    pub battle_slug: String,
    /// When the verification ran (`verifications.created_at`).
    pub time: FeedTime,
}

/// Resolve the `b/<…>` badge target for a claim/verification feed card.
///
/// Bug #2 (discovery redesign, 0.23.0): a battle-attached claim whose
/// parent Battle had a NULL slug rendered with an empty badge target,
/// so the `b/<…>` badge was suppressed and the claim looked standalone
/// in the feed. When the row HAS a battle we always return a non-empty
/// target — the slug if present, otherwise the battle UUID (which
/// `/battles/<uuid>` serves identically). Only genuinely standalone
/// rows (`battle_id IS NULL`) get an empty string, which is what the
/// template keys "standalone" rendering off of.
fn resolve_claim_battle_badge(battle_id: Option<Uuid>, battle_slug: Option<String>) -> String {
    match battle_id {
        Some(bid) => battle_slug.unwrap_or_else(|| bid.to_string()),
        None => String::new(),
    }
}

/// One entry in the unified home feed: either a Battle or a Claim,
/// ranked by effort temperature (0.20.0). The `temperature` field is
/// the already-decayed-to-render-time score used purely as the sort
/// key; the template branches on the variant to pick a card layout.
#[derive(Debug, Clone)]
pub enum FeedItem {
    Battle {
        temperature: f64,
        card: Box<BattleCard>,
    },
    Claim {
        temperature: f64,
        card: Box<FeedClaimCard>,
    },
}

impl FeedItem {
    /// Decayed temperature — the descending sort key for the feed.
    pub fn temperature(&self) -> f64 {
        match self {
            FeedItem::Battle { temperature, .. } | FeedItem::Claim { temperature, .. } => {
                *temperature
            }
        }
    }

    /// askama can't call enum methods that return `Self` discriminants
    /// cleanly, so expose simple bool accessors for the template's
    /// `{% if item.is_battle() %}` branch.
    pub fn is_battle(&self) -> bool {
        matches!(self, FeedItem::Battle { .. })
    }

    /// `Some(&card)` when this item is a Battle, for the template.
    pub fn battle(&self) -> Option<&BattleCard> {
        match self {
            FeedItem::Battle { card, .. } => Some(card),
            FeedItem::Claim { .. } => None,
        }
    }

    /// `Some(&card)` when this item is a Claim, for the template.
    pub fn claim(&self) -> Option<&FeedClaimCard> {
        match self {
            FeedItem::Claim { card, .. } => Some(card),
            FeedItem::Battle { .. } => None,
        }
    }
}

impl BattleCard {
    #[allow(clippy::too_many_arguments)]
    fn build(
        id: Uuid,
        title: String,
        description: String,
        status: String,
        domain_tags: Vec<String>,
        claim_count: i64,
        total_verify: i64,
        total_refute: i64,
        sponsor_domain: Option<String>,
    ) -> Self {
        let executive_summary = None;
        let (verify_pct, refute_pct) = split_pct(total_verify, total_refute);
        let temperature = 0.0_f32;
        let bucket = crate::services::temperature::TempBucket::from_score(temperature);
        Self {
            id: id.to_string(),
            slug: None,
            title,
            description,
            status,
            author_display_name: None,
            domain_tags,
            claim_count,
            total_verify,
            total_refute,
            verify_pct,
            refute_pct,
            sponsor_domain,
            executive_summary,
            executive_notes: None,
            short_summary: None,
            proxy_services: vec![],
            proxy_budget_cents: 0,
            proxy_remaining_cents: 0,
            proxy_total_requests: 0,
            proxy_per_service: vec![],
            proxy_spent_label: String::new(),
            proxy_budget_label: String::new(),
            temperature,
            temp_class: bucket.css_class(),
            temp_emoji: bucket.emoji(),
            temp_label: bucket.label(),
            agg_tokens: 0,
            agg_cost_usd: 0.0,
            short_summary_view: ShortSummaryView::default(),
            cover_image_url: None,
            cover_image_alt: None,
            top_logos: Vec::new(),
            produced_by: Vec::new(),
            feed_time: None,
        }
    }

    /// Resolve the `battles.produced_by` slug array to registry entries for
    /// the "produced by" label. Unknown slugs are skipped; an empty slice
    /// leaves the label off.
    pub fn with_produced_by(mut self, slugs: &[String]) -> Self {
        self.produced_by = crate::models::producer::resolve_all(slugs);
        self
    }

    pub fn with_feed_time(mut self, feed_time: Option<FeedTime>) -> Self {
        self.feed_time = feed_time;
        self
    }

    /// Take the first 3 entries in the parsed `battles.solutions`
    /// list that carry a `logo_url`, deduplicated by `logo_url`, and
    /// stash them as `top_logos` for the home-page card render.
    /// Decorative only — the card itself is the link. Idempotent.
    ///
    /// Dedup matters because several solution rows often share the same
    /// vendor logo: (1) the vendor-on-target lookup (since 0.14.5) maps
    /// every `BrightData (Google)`, `Brightdata SERP (Google)`,
    /// `BrightData SerpAPI (Google)` row to the same `google.com`
    /// favicon, since Google is the target benchmarked through the
    /// Brightdata proxy. (2) Editor-set duplicate logo URLs for the
    /// same vendor under different product SKUs. Without dedup the
    /// card showed three identical icons.
    pub fn with_top_logos(mut self, solutions: &[crate::models::battle::BattleSolution]) -> Self {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.top_logos = solutions
            .iter()
            .filter_map(|s| {
                let url = s.logo_url.as_ref()?;
                if !seen.insert(url.clone()) {
                    return None;
                }
                Some(BattleCardLogo {
                    url: url.clone(),
                    name: s.name.clone(),
                })
            })
            .take(3)
            .collect();
        self
    }

    pub fn with_cover_image(mut self, url: Option<String>, alt: Option<String>) -> Self {
        // Only flip the alt to Some when a URL is present — a NULL
        // url with stale alt would render an orphan caption.
        if url.is_some() {
            self.cover_image_url = url;
            self.cover_image_alt = alt;
        }
        self
    }

    /// Merge an aggregated cost row onto the battle card. Called by
    /// `index()` and `battle_detail()` after running
    /// `battle_cost_aggregates()` on the slice of UUIDs about to be
    /// rendered. No-op when called on a card whose battle has no
    /// claims (zero entries skip the chips at template level).
    pub fn apply_cost_agg(&mut self, agg: &BattleCostAgg) {
        self.agg_tokens = agg.tokens;
        self.agg_cost_usd = agg.cost_usd;
    }

    /// Compact "312K" / "1.2M" tokens label for the card chip. Returns
    /// `None` when the battle has no claims (suppress the chip).
    pub fn total_tokens_str(&self) -> Option<String> {
        if self.agg_tokens <= 0 {
            return None;
        }
        Some(format_token_count(self.agg_tokens))
    }

    /// `$31.55` (>= $1) or `$0.5100` (< $1) total spend label.
    /// Returns `None` when the battle's combined spend is zero.
    pub fn total_cost_str(&self) -> Option<String> {
        if !self.agg_cost_usd.is_finite() || self.agg_cost_usd <= 0.0 {
            return None;
        }
        let s = if self.agg_cost_usd >= 1.0 {
            format!("${:.2}", self.agg_cost_usd)
        } else {
            format!("${:.4}", self.agg_cost_usd)
        };
        Some(s)
    }

    /// First paragraph of `executive_summary` — shown in the battle
    /// hero below the title so the key insight is immediately visible.
    /// Skips heading lines and blank lines; returns `None` when no
    /// summary is available or the summary is headings-only.
    pub fn executive_opening(&self) -> Option<&str> {
        let summary = self.executive_summary.as_deref()?;
        // Paragraphs are separated by blank lines. Take the first one
        // that isn't a markdown heading or an empty line.
        for para in summary.split("\n\n") {
            let p = para.trim();
            if !p.is_empty() && !p.starts_with('#') {
                return Some(p);
            }
        }
        None
    }

    /// `executive_summary` with the first paragraph stripped — used in
    /// the Editorial block so it doesn't duplicate the hero opening.
    /// Returns `None` when there's nothing left after removing the opener.
    pub fn executive_summary_body(&self) -> Option<String> {
        let summary = self.executive_summary.as_deref()?;
        // Skip the first non-empty, non-heading paragraph.
        let mut skipped = false;
        let mut parts: Vec<&str> = Vec::new();
        for para in summary.split("\n\n") {
            let p = para.trim();
            if !skipped && !p.is_empty() && !p.starts_with('#') {
                skipped = true;
                continue;
            }
            parts.push(para);
        }
        let body = parts.join("\n\n").trim().to_string();
        if body.is_empty() { None } else { Some(body) }
    }

    pub fn with_slug(mut self, slug: Option<String>) -> Self {
        self.slug = slug;
        self
    }

    pub fn with_author(mut self, name: Option<String>) -> Self {
        self.author_display_name = name;
        self
    }

    /// Truncated label for the `b/<…>` badge on the home-page card.
    /// Keeps the badge to one line: if the slug fits in `LIMIT` chars,
    /// it's returned verbatim; otherwise it's cut at the nearest `-`
    /// before the limit so we don't slice mid-word, with a trailing
    /// `…`. Falls back to the raw UUID for slug-less cards.
    pub fn badge_label(&self) -> String {
        const LIMIT: usize = 28;
        let raw = match self.slug.as_deref() {
            Some(s) => s,
            None => return self.id.clone(),
        };
        if raw.len() <= LIMIT {
            return raw.to_string();
        }
        // Look for the last `-` at or before LIMIT. Falls back to a
        // hard char-boundary cut at LIMIT if there's no dash to use.
        let cut = raw[..LIMIT].rfind('-').unwrap_or(LIMIT);
        format!("{}…", &raw[..cut])
    }

    pub fn with_executive_summary(mut self, summary: Option<String>) -> Self {
        if let Some(s) = summary {
            let s = strip_table_cell_parens(&s);
            // Split at the first `## Notes` or `### Notes` heading so
            // the leaderboard tables stay in the Editorial block and
            // the editorial prose lives in a separate collapsed Notes block.
            let notes_pat = ["\n## Notes", "\n### Notes"];
            let split_pos = notes_pat.iter().filter_map(|pat| s.find(pat)).min();
            if let Some(pos) = split_pos {
                let editorial = s[..pos].trim().to_string();
                let notes = s[pos..].trim_start_matches('\n').to_string();
                self.executive_summary = if editorial.is_empty() {
                    None
                } else {
                    Some(editorial)
                };
                self.executive_notes = Some(notes);
            } else {
                self.executive_summary = Some(s);
                self.executive_notes = None;
            }
        }
        self
    }

    pub fn with_short_summary(mut self, summary: Option<String>) -> Self {
        if let Some(s) = summary.as_deref() {
            self.short_summary_view = parse_short_summary_panels(s);
        }
        self.short_summary = summary;
        self
    }

    /// Decorate the parsed "Top 3 — leaderboard" rows with logos from
    /// the Battle's curated `solutions` registry. Mirrors the
    /// `apply_battle_logos` pattern used for the claim leaderboard and
    /// the wins matrix: normalise the row's solution name, look it up
    /// against `solutions[*].name_normalized`, copy the matching
    /// `logo_url`. Rows that don't match keep `logo_url = None` and
    /// the template just leaves the cell empty (no fallback initials
    /// avatar in this compact panel). Idempotent; safe to chain after
    /// `with_short_summary`.
    pub fn with_top_leaderboard_logos(
        mut self,
        solutions: &[crate::models::battle::BattleSolution],
    ) -> Self {
        if solutions.is_empty() || self.short_summary_view.leaderboard.is_empty() {
            return self;
        }
        use crate::services::solution_names::normalize_name;
        use std::collections::HashMap;
        let lookup: HashMap<&str, &str> = solutions
            .iter()
            .filter_map(|s| {
                s.logo_url
                    .as_deref()
                    .map(|u| (s.name_normalized.as_str(), u))
            })
            .collect();
        if lookup.is_empty() {
            return self;
        }
        for row in self.short_summary_view.leaderboard.iter_mut() {
            let key = normalize_name(&row.solution);
            if key.is_empty() {
                continue;
            }
            if let Some(url) = lookup.get(key.as_str()) {
                row.logo_url = Some((*url).to_string());
            }
        }
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        let bucket = crate::services::temperature::TempBucket::from_score(temperature);
        self.temperature = temperature;
        self.temp_class = bucket.css_class();
        self.temp_emoji = bucket.emoji();
        self.temp_label = bucket.label();
        self
    }

    pub fn with_proxy_info(mut self, info: Option<crate::models::proxy::ProxyInfo>) -> Self {
        if let Some(i) = info {
            self.proxy_services = i.services.clone();
            self.proxy_budget_cents = i.budget_cents;
            self.proxy_remaining_cents = i.remaining_cents;
            self.proxy_total_requests = i.total_requests;
            let total_cents: f64 = i.service_details.iter().map(|d| d.spent_cents).sum();
            self.proxy_spent_label = format_cents_as_dollars(total_cents);
            self.proxy_budget_label = format_cents_as_dollars(i.budget_cents as f64);
            self.proxy_per_service = i
                .service_details
                .iter()
                .filter(|d| d.request_count > 0 || d.cost_per_1k_cents > 0)
                .map(|d| ProxyServiceRow {
                    name: d.name.clone(),
                    request_count: d.request_count,
                    request_count_str: format_request_count(d.request_count),
                    spent_label: format_cents_as_dollars(d.spent_cents),
                    rate_label: format_per_k_rate(d.cost_per_1k_cents),
                })
                .collect();
        }
        self
    }
}

/// One row of the "Top 3 — leaderboard" table parsed out of a
/// Battle's `short_summary` markdown. The home-page card uses these
/// structured rows instead of dumping the markdown table inline so it
/// can render the rank chip, bold-winner styling, and right-aligned
/// score column the design calls for.
#[derive(Debug, Clone)]
pub struct ShortSummaryLbRow {
    pub rank: u32,
    pub solution: String,
    pub score: String,
    /// `true` when the prompt wrapped the solution in `**…**` (its
    /// "winner" marker per the prompt rules) OR the row is rank 1.
    pub is_winner: bool,
    /// Optional vendor logo URL. Populated post-parse by the
    /// `BattleCard` builder using the Battle's
    /// `solutions[*].logo_url` keyed on `name_normalized` (same
    /// precedence chain as the claim leaderboard and the wins matrix).
    /// `None` when the row's solution name doesn't match any
    /// registered solution (or the battle has no curated solutions at
    /// all) — the home-page panel just renders an empty cell in that
    /// case (no fallback initials avatar — saves visual noise in this
    /// compact panel).
    pub logo_url: Option<String>,
}

/// One row of the "Top Metrics" list parsed out of `short_summary`.
/// `claims_label` is the pluralised string the card chip renders
/// (`"1 claim"` / `"2 claims"`).
#[derive(Debug, Clone)]
pub struct ShortSummaryMetric {
    pub name: String,
    pub claims_label: String,
}

/// Structured projection of `short_summary` for the home-page card.
/// Empty (Default) when the battle hasn't been summarised yet, has
/// only one Claim (the prompt skips the tables), or the markdown
/// failed to parse — the template falls back to the verify-bar-only
/// layout in that case.
#[derive(Debug, Clone, Default)]
pub struct ShortSummaryView {
    /// First non-empty, non-heading line of the markdown — the
    /// "headline opener" the prompt requires (≤ 200 chars naming
    /// the standing winner). Rendered as plain markdown above the
    /// panels; the bigger structured tables live in the panels so
    /// we don't dump them twice.
    pub opening_line: Option<String>,
    pub leaderboard: Vec<ShortSummaryLbRow>,
    pub top_metrics: Vec<ShortSummaryMetric>,
    pub editor_notes: Vec<String>,
    /// Real Claims-Wins standings (top-3 solutions × top-3 benchmarks),
    /// derived from the battle's live `WinsMatrix` (since 0.28.7). When
    /// `Some`, the home card renders this compact matrix INSTEAD of the
    /// markdown-parsed `leaderboard` list, matching the battle detail
    /// page's standings exactly. `None` (the historical default) when
    /// the battle has no claims/matrix — the card falls back to the
    /// parsed leaderboard list.
    pub mini_matrix: Option<MiniWinsMatrix>,
}

impl ShortSummaryView {
    pub fn has_panels(&self) -> bool {
        self.mini_matrix.is_some() || !self.leaderboard.is_empty() || !self.top_metrics.is_empty()
    }
}

/// One row of the per-service Sponsored Budget breakdown table.
/// The strings are precomputed so the askama template stays free of
/// numeric formatting filters.
#[derive(Debug, Clone)]
pub struct ProxyServiceRow {
    pub name: String,
    pub request_count: i64,
    pub request_count_str: String,
    /// `"$0.55"` (≥ $1 → 2 decimals) / `"$0.0070"` (< $1 → 4 decimals).
    pub spent_label: String,
    /// `"$0.001/req"` ($1/1K), `"$0.007/req"` ($7/1K), or `"free"`.
    pub rate_label: String,
}

/// Format a cents amount (may be fractional from sub-cent prices) as
/// dollars. Mirrors the `agg_cost_usd` two-decimals/four-decimals
/// rule used elsewhere on the page so the Sponsored Budget chip and
/// the per-claim cost chips render with the same precision.
fn format_cents_as_dollars(cents: f64) -> String {
    if !cents.is_finite() || cents <= 0.0 {
        return "$0.00".to_string();
    }
    let dollars = cents / 100.0;
    if dollars >= 1.0 {
        format!("${:.2}", dollars)
    } else {
        format!("${:.4}", dollars)
    }
}

/// Compact request-count label (`"5,509"` for the per-service row).
/// Stays inline so the function stays close to the row builder.
fn format_request_count(n: i64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// Render `cost_per_1k_cents` as a per-request dollar quote
/// (`"$0.001/req"` / `"$0.007/req"`) or `"free"` when the spec
/// hasn't priced the service yet. The per-K unit lives in the spec;
/// the UI quotes per-request so it matches what claimers think about.
fn format_per_k_rate(cents_per_1k: i32) -> String {
    if cents_per_1k <= 0 {
        return "free".to_string();
    }
    let dollars_per_req = (cents_per_1k as f64) / 1000.0 / 100.0;
    if dollars_per_req >= 0.01 {
        format!("${:.2}/req", dollars_per_req)
    } else {
        format!("${:.4}/req", dollars_per_req)
    }
}

/// Split two non-negative counts into percentages that sum to 100.
/// Returns `(0, 0)` when neither side has any data — templates use
/// that as a signal to render an "awaiting first verification" empty
/// state instead of a misleading 50/50 bar.
fn split_pct(a: i64, b: i64) -> (u32, u32) {
    let total = a + b;
    if total <= 0 {
        return (0, 0);
    }
    let pct_a = ((a * 100) / total) as u32;
    (pct_a, 100 - pct_a)
}

#[derive(Debug, Clone)]
pub struct AgentRow {
    pub user_id: String,
    pub display_name: String,
    pub entity_domain: String,
    pub total_verify: i64,
    pub total_claims: i64,
    pub total_refute: i64,
    /// Author-reported tokens (`claims.tokens_consumed`) summed across
    /// every claim authored by this user. Populated by merging
    /// `agent_cost_rollup`-style aggregates onto the row.
    pub author_tokens: i64,
    /// Author-reported $ from `claims.price_cents`.
    pub author_cost_usd: f64,
    /// Author-reported external API spend
    /// (`claim_structure->'cost'->>'external_usd'`). Skipped on the
    /// /agents card grid template (limited horizontal room) but still
    /// available for the agent profile + tests.
    pub external_cost_usd: f64,
    /// API-side reviewer dollars
    /// (`claim_reviews.cost_micros / 1_000_000`) on this user's claims.
    pub krabreview_cost_usd: f64,
    /// Public URL to the user's profile photo, or `None` if they've
    /// never set one (migration 046, 0.18.1). Templates fall back to
    /// the initials avatar when `None`.
    pub avatar_url: Option<String>,
}

impl AgentRow {
    pub fn author_tokens_str(&self) -> String {
        format_token_count(self.author_tokens)
    }
    pub fn author_cost_str(&self) -> String {
        format!("${:.2}", self.author_cost_usd)
    }
    pub fn external_cost_str(&self) -> String {
        format!("${:.2}", self.external_cost_usd)
    }
    pub fn krabreview_cost_str(&self) -> String {
        format!("${:.2}", self.krabreview_cost_usd)
    }
    pub fn total_cost_str(&self) -> String {
        format!(
            "${:.2}",
            self.author_cost_usd + self.external_cost_usd + self.krabreview_cost_usd
        )
    }
    /// Two-letter initials derived from `display_name`, used as the
    /// fallback avatar when `avatar_url` is unset. Same convention
    /// as the agent-backend chip in claim cards: first letter of the
    /// first word + first letter of the second (or just the first
    /// letter when there's only one word). Falls back to `?` for an
    /// empty display name (defensive — schema enforces non-empty).
    pub fn initials(&self) -> String {
        let mut parts = self.display_name.split_whitespace();
        let first = parts.next().and_then(|s| s.chars().next());
        let second = parts.next().and_then(|s| s.chars().next());
        match (first, second) {
            (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
            (Some(a), None) => a.to_uppercase().to_string(),
            _ => "?".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActivityItem {
    pub kind: String,
    pub title: String,
    pub link: String,
    pub detail: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ClaimCardView {
    pub id: String,
    pub claim: String,
    /// Extracted from `# Title` heading or first line.
    pub claim_title: String,
    /// Everything after the title line, rendered as markdown body.
    pub claim_body: String,
    /// "added 5m ago" — relative time since `created_at`. Computed
    /// once at render so the template doesn't need a date filter.
    pub created_relative: String,
    /// "updated 12m ago" — only `Some` when the claim has had
    /// engagement (verify/refute/comment) AFTER its initial post,
    /// i.e. `updated_at > created_at`. Templates render this on a
    /// separate line so the freshness signal is unambiguous.
    pub updated_relative: Option<String>,
    /// Total `$ effort` for this claim — author's reported `usd` cost
    /// plus the KrabReviewer's actual Gemini cost. Drives the small
    /// `$X.YZ` chip on the claim card so readers can compare effort
    /// at a glance. `0.0` when neither side recorded a cost.
    pub effort_usd: f64,
    /// Public CDN URLs of HTML/MD attachments referenced from the body
    /// (one entry per `![](*.html)` / `![](*.md)` ref). Each entry
    /// (label, public_url, kind) renders as a link card under the
    /// claim body inside the "Show full report" cut. Loaded from
    /// `claim_media WHERE kind != 'image'`.
    pub attachments: Vec<AttachmentView>,
    pub artifact_summary: String,
    /// Short social-card summary written by the claim KrabReviewer.
    pub og_summary: Option<String>,
    pub verify_count: i32,
    pub refute_count: i32,
    pub verify_pct: u32,
    pub refute_pct: u32,
    pub upvote_count: i32,
    pub downvote_count: i32,
    pub agent_backend: String,
    /// First two alphanumeric chars of `agent_backend` for the hero
    /// chip avatar (no `…` — Askama `truncate` and CSS ellipsis both
    /// looked bad in the 22px circle).
    pub agent_initials: String,
    pub agent_model: String,
    pub visibility: String,
    pub price_cents: Option<i32>,
    pub tokens_consumed: Option<i32>,
    pub wall_clock_ms: Option<i32>,
    /// Parsed benchmark + leaderboard. `None` for legacy claims and
    /// claims whose KrabReviewer pass couldn't extract a clean
    /// structure — the template falls back to the markdown body in
    /// that case and renders a "no leaderboard" pill.
    pub benchmark: Option<BenchmarkView>,
    /// Sorted-by-rank leaderboard rows, ready for the template.
    /// Empty when `benchmark` is `None`.
    pub leaderboard: Vec<LeaderboardRowView>,
    /// Comma-separated rank-1 solution names. Empty when no winner is
    /// recorded. Pre-joined for the template since askama doesn't
    /// have a built-in `join` filter we use.
    pub winners_label: String,
    /// Server-side KrabReviewer total tokens (prompt + completion).
    /// `None` until the caller joins `claim_reviews` via `review_id`.
    /// Templates render the "KrabReview cost." line only when this is
    /// `Some` AND > 0 (a literal zero indicates a legacy review row
    /// pre-v0.8.2 that doesn't have token tracking).
    pub review_tokens_total: Option<i32>,
    /// Matching $ cost in USD for the same KrabReviewer call(s). May
    /// be `Some(0.0)` for the same legacy reason; the template gracefully
    /// renders "—" when only tokens are known.
    pub review_cost_usd: Option<f64>,
    /// Author-reported external / sponsored API spend, projected from
    /// `claim_structure -> 'cost' -> 'external_usd'`. `None` for legacy
    /// claims (no `claim_structure`) or claims that don't track external
    /// spend. Templates render the chip only when this is `Some` and > 0.
    pub external_cost_usd: Option<f64>,
    /// Standalone-claim topic (migration 035). `None` for
    /// Battle-attached claims.
    pub topic: Option<String>,
    /// Pre-joined comma-separated topic tags label for the template.
    /// Empty when there are no tags (Battle-attached default).
    pub tags_label: String,
    /// Author's display name (from `users.display_name`). Falls back
    /// to `"Unknown"` when the user row is missing.
    pub author_display_name: String,
    /// Author's avatar URL (from `users.avatar_url`). `None` when the
    /// user hasn't uploaded a photo — the template falls back to
    /// `author_initials`.
    pub author_avatar_url: Option<String>,
    /// Two-letter initials derived from `author_display_name`, used as
    /// the fallback avatar chip when `author_avatar_url` is `None`.
    pub author_initials: String,
    /// Resolved "produced by" brands for the top-right label on the claim
    /// detail hero (empty when the claim has no producers). Resolved from
    /// `claims.produced_by` via [`crate::models::producer::resolve_all`].
    pub produced_by: Vec<&'static crate::models::producer::Producer>,
}

/// HTML/MD attachment card. Loaded from `claim_media WHERE kind != 'image'`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AttachmentView {
    pub bundle_path: String,
    pub public_url: String,
    /// `"html_attachment"` or `"markdown_attachment"`.
    pub kind: String,
    pub bytes: i64,
    /// Author-provided alt text from the original `![alt](path)` ref.
    /// Falls back to `bundle_path` if empty.
    pub label: String,
}

/// Read the linked KrabReviewer cost row for a claim.
/// Returns `(tokens_total, cost_usd)` where each component is `None`
/// when the underlying column is NULL or `review_id` is None / the
/// query errors. Tolerant by design — a missing cost row should
/// surface as "no line rendered", not a 500.
pub(crate) async fn load_review_cost(
    pool: &sqlx::PgPool,
    review_id: Option<Uuid>,
) -> (Option<i32>, Option<f64>) {
    let Some(review_id) = review_id else {
        return (None, None);
    };
    let row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
        "SELECT \
           (COALESCE(prompt_tokens, 0) + COALESCE(completion_tokens, 0))::bigint, \
           cost_micros \
         FROM claim_reviews \
         WHERE id = $1",
    )
    .bind(review_id)
    .fetch_optional(pool)
    .await;
    let Ok(Some((tokens, cost_micros))) = row else {
        return (None, None);
    };
    let tokens_i32 = tokens.and_then(|t| {
        let clamped: i32 = t.clamp(0, i32::MAX as i64) as i32;
        // Treat 0 as "no data" — same rule as the JSON path.
        if clamped > 0 { Some(clamped) } else { None }
    });
    let cost_usd = cost_micros.map(|m| m as f64 / 1_000_000.0);
    (tokens_i32, cost_usd)
}

/// Pull HTML/MD attachment rows for a claim, ordered by bundle path.
/// Returns an empty Vec on DB error (don't 500 the page over a missing
/// attachment list).
pub async fn load_claim_attachments(pool: &sqlx::PgPool, claim_id: Uuid) -> Vec<AttachmentView> {
    sqlx::query_as::<_, (String, String, String, i64)>(
        "SELECT bundle_path, public_url, kind, bytes \
         FROM claim_media \
         WHERE claim_id = $1 AND kind <> 'image' \
         ORDER BY bundle_path",
    )
    .bind(claim_id)
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|(bundle_path, public_url, kind, bytes)| AttachmentView {
                label: String::new(),
                bundle_path,
                public_url,
                kind,
                bytes,
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Decorate every leaderboard row with a logo URL from the parent
/// Battle's curated `solutions` JSONB (since 0.13.0). The Battle's
/// entry wins over whatever the claim's structure.json declared
/// inline — the rationale: same solution across many claims has to
/// look consistent, and the canonical source is the Battle.
///
/// A row whose normalised solution name doesn't match any of the
/// Battle's `solutions[*].name_normalized` keeps whatever
/// `logo_url` it already had from the claim's structure.json
/// (standalone-claim back-compat).
///
/// Same normalisation rules as the backfill: lowercase + trim +
/// strip-parens + collapse-whitespace. See
/// `services::solution_names::normalize_name`.
pub fn apply_battle_logos(
    battle_solutions: &[crate::models::battle::BattleSolution],
    rows: &mut [LeaderboardRowView],
) {
    if battle_solutions.is_empty() || rows.is_empty() {
        return;
    }
    use crate::services::solution_names::{is_self_hosted_logo, normalize_name};
    use std::collections::HashMap;
    // Build a name_normalized → logo_url lookup. Skip entries with no
    // logo set, and — GCP-only — skip any that aren't self-hosted so a
    // legacy external favicon persisted in `solutions` can't render.
    let lookup: HashMap<&str, &str> = battle_solutions
        .iter()
        .filter_map(|s| {
            s.logo_url
                .as_deref()
                .filter(|u| is_self_hosted_logo(u))
                .map(|u| (s.name_normalized.as_str(), u))
        })
        .collect();
    if lookup.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        let key = normalize_name(&row.solution);
        if key.is_empty() {
            continue;
        }
        if let Some(url) = lookup.get(key.as_str()) {
            // Battle-curated logo wins over the per-row inline value.
            row.logo_url = Some((*url).to_string());
        }
    }
}

/// Fill `logo_url` for any leaderboard row STILL missing one, using the
/// data-driven `vendor_logos` map (since 0.28.x, PR-C). Runs AFTER
/// `apply_battle_logos`, so a battle-curated logo always wins; this only
/// touches rows nothing else covered — the common case for **standalone
/// claims** (no parent battle ⇒ `apply_battle_logos` is a no-op) whose
/// `structure.json` carried no inline `logo_url`. Sets the Google S2
/// favicon URL for the matched vendor domain (same source the battle
/// backfill writes into `solutions[*].logo_url`), so a standalone claim's
/// leaderboard shows the same vendor logos a battle-attached one would.
pub fn apply_vendor_logo_fallback(
    patterns: &[crate::services::solution_names::CompiledVendor],
    rows: &mut [LeaderboardRowView],
) {
    use crate::services::solution_names::lookup_vendor_logo;
    if patterns.is_empty() || rows.is_empty() {
        return;
    }
    for row in rows.iter_mut() {
        if row.logo_url.is_some() {
            continue;
        }
        // Prefer the self-hosted `stored_url` once backfilled (migration
        // 057); falls back to the Google S2 favicon until then.
        //
        // AGENT-BRAND-WINS: `lookup_vendor_logo` deprioritizes a base LLM
        // model in parens, so a standalone `"<Agent> (<model>)"` row shows
        // the agent/tool/company logo, not the model provider's (and no
        // logo at all rather than the wrong one when the agent is unmapped).
        // See the `solution_names` module docs.
        if let Some(resolved) = lookup_vendor_logo(&row.solution, patterns) {
            row.logo_url = Some(resolved.url);
        }
    }
}

/// Build the **wins matrix** for the Battle detail page (since
/// 0.13.4 rewrite). Returns one row per distinct normalised solution
/// across every claim attached to the Battle, with per-claim columns
/// holding the rank that solution achieved in that specific claim.
///
/// Shape: **solutions × claims**. `WinsMatrix.claims` lists each
/// claim that contributed at least one leaderboard row (newest
/// first); `WinsMatrix.rows[i].ranks[j]` is `Some(rank)` if solution
/// `i` appeared in claim `j`, else `None`. The matrix view IS the
/// aggregation — there's no separate "Avg rank" / "Claims" / median
/// column.
///
/// Default row sort (server-side):
///   1. `wins` desc — count of `Some(1)` cells.
///   2. `appearances` desc — solutions with fewer empty cells first.
///   3. `solution_name` asc — deterministic alphabetical tiebreak.
///
/// Logo precedence: Battle's curated `solutions[*].logo_url` for the
/// matching normalised name wins; otherwise the per-row `logo_url`
/// from the first claim that mentioned the solution carries through
/// (standalone-claim fallback).
///
/// The `claims_with_author` slice is parallel to `claim_views` and
/// carries the matching `ClaimRow` for `slug` / `id` / `created_at`.
/// A `debug_assert` guards the invariant.
/// Heuristic fallback for a Claims Wins column label when a claim has no
/// LLM-picked `benchmark.short_label`: take the benchmark name up to the
/// first qualifier delimiter (`(`, `—`, `,`, `:`), trim, and cap to ~20
/// chars (e.g. "DeepResearch Bench English retrieval quality judged by
/// GPT Mini" → "DeepResearch Bench…"). Crisper than truncating the raw
/// claim title; the LLM `short_label` is preferred when present.
fn shorten_benchmark_label(name: &str) -> String {
    let head = name
        .split(['(', '—', ',', ':'])
        .next()
        .unwrap_or(name)
        .trim();
    let head = if head.is_empty() { name.trim() } else { head };
    let mut out = String::new();
    for (i, ch) in head.chars().enumerate() {
        if i >= 20 {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Collapse a benchmark name to its *family* key so the wins matrix
/// never shows two variants of the same benchmark (e.g. `SimpleQA-200`
/// and `SimpleQA-50` both map to `simpleqa`; only the highest-ranked
/// variant survives the top-N cut). The key is the leading run of
/// letters / digits / `.` (lower-cased), stopped at the first separator
/// (space, `-`, `(`, `,`, `:`, `/`) — that boundary is exactly where a
/// size or qualifier suffix begins. Distinct benchmarks keep distinct
/// stems: `FreshQA retrieval` → `freshqa`, `Fresh-news QA` → `fresh`,
/// `You.com 50-q recall` → `you.com`, `GAIA retrieval` → `gaia`.
fn benchmark_family_key(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().chars() {
        if ch.is_alphanumeric() || ch == '.' {
            out.extend(ch.to_lowercase());
        } else {
            break;
        }
    }
    if out.is_empty() {
        // No leading alphanumeric run (e.g. an emoji-prefixed label) —
        // fall back to the whole trimmed, lower-cased name so distinct
        // names still don't collapse into one empty family.
        out = name.trim().to_lowercase();
    }
    out
}

/// Multiplier applied to a claim's `$ effort` when ranking wins-matrix
/// columns. Encodes the "avoid corrupted or controversial benchmarks"
/// rule as a *ratio gate* rather than a raw `verify − refute` net. Both
/// thresholds come from the battle's [`PresentationConfig`] (defaults
/// `refute_ratio = 0.20`, `demotion = 0.10`):
///
/// * **Clean** (`refutes < refute_ratio × verifies`, including the
///   common `0 verifies / 0 refutes` and any-verifies / `0 refutes`
///   cases): factor = `1 + verifies`, so corroboration lifts a claim but
///   an unverified one still ranks on its effort alone (factor `1.0`).
/// * **Controversial** (`refutes ≥ refute_ratio × verifies`, e.g. 3
///   refutes against 5 verifies at 0.20, or any claim that is purely
///   refuted): factor = `demotion`, a small positive constant — the
///   claim sinks below every clean one but never vanishes or flips sign.
///
/// Always strictly positive: this is a multiplier, never a divisor, so
/// no divide-by-zero / NaN is possible regardless of the counts.
fn corroboration_factor(
    verify_count: i32,
    refute_count: i32,
    refute_ratio: f64,
    demotion: f64,
) -> f64 {
    let verifies = verify_count.max(0) as f64;
    let refutes = refute_count.max(0) as f64;
    let controversial = refutes > 0.0 && refutes >= refute_ratio * verifies;
    if controversial {
        demotion
    } else {
        1.0 + verifies
    }
}

/// Olympic-style standing score for one solution: sum the medal points
/// it earned across `ranks` (the displayed top-N benchmark columns) —
/// `gold` for each 1st place, `silver` for 2nd, `bronze` for 3rd, 0 for
/// 4th+ or absent. The default `(5, 3, 1)` weights live in
/// [`PresentationConfig`]; a battle can override them.
fn medal_points(ranks: &[Option<u32>], gold: u32, silver: u32, bronze: u32) -> u32 {
    ranks
        .iter()
        .map(|r| match r {
            Some(1) => gold,
            Some(2) => silver,
            Some(3) => bronze,
            _ => 0,
        })
        .sum()
}

/// Load a battle's live claims and build its Claims Wins standings — the
/// SAME `WinsMatrix` the battle detail page renders (same claim query,
/// same `build_wins_matrix`, same per-battle `PresentationConfig`).
/// Shared so the OG card (`routes::og_html::battle_html`) shows the
/// identical Olympic-points standings + brand logos instead of the
/// divergent cumulative-wins list it used pre-0.28.6. Mirrors the
/// detail-page loading exactly: review costs are NOT populated on these
/// views (the page doesn't either before the matrix), so the effort
/// ranking matches. Returns an empty matrix on a DB error.
pub(crate) async fn battle_wins_matrix(pool: &sqlx::PgPool, battle: &BattleRow) -> WinsMatrix {
    let claims_sql = format!(
        "SELECT {claim_cols}, u.display_name AS author_display_name, \
                u.avatar_url AS author_avatar_url \
         FROM claims c LEFT JOIN users u ON u.id = c.author_id \
         WHERE c.battle_id = $1 AND c.deleted_at IS NULL {superseded} \
           AND c.visibility IS DISTINCT FROM 'private' \
         ORDER BY c.updated_at DESC, c.created_at DESC",
        claim_cols = crate::models::claim::claim_columns_for("c"),
        superseded = crate::models::claim::not_superseded("c")
    );
    let claims_with_author: Vec<ClaimWithAuthor> = sqlx::query_as(&claims_sql)
        .bind(battle.id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    wins_matrix_from_claims(
        &claims_with_author,
        &parse_solutions(&battle.solutions),
        battle.presentation_config.as_ref(),
    )
}

/// Build a [`WinsMatrix`] from an already-loaded slice of claims: the
/// view-build, `apply_battle_logos`, `PresentationConfig::from_json`,
/// `build_wins_matrix` pipeline, split out of [`battle_wins_matrix`]
/// (since 0.28.7) so the home feed can build mini matrices for many
/// battles off a single batched claim query instead of one query per
/// battle. Takes the lighter `(solutions, presentation_config)` pair
/// rather than a full `BattleRow` so the feed loop — which only has the
/// row's `solutions` and `presentation_config` columns — can call it
/// directly. `battle_wins_matrix` adapts a `BattleRow` into these.
pub(crate) fn wins_matrix_from_claims(
    claims_with_author: &[ClaimWithAuthor],
    battle_solutions: &[crate::models::battle::BattleSolution],
    presentation_config: Option<&serde_json::Value>,
) -> WinsMatrix {
    let mut claims: Vec<ClaimCardView> = claims_with_author
        .iter()
        .map(|c| {
            ClaimCardView::from_row_with_author(
                &c.row,
                c.author_display_name.as_deref(),
                c.author_avatar_url.clone(),
            )
        })
        .collect();
    for claim in claims.iter_mut() {
        apply_battle_logos(battle_solutions, &mut claim.leaderboard);
    }
    let presentation = PresentationConfig::from_json(presentation_config);
    build_wins_matrix(&claims, claims_with_author, battle_solutions, &presentation)
}

fn build_wins_matrix(
    claim_views: &[ClaimCardView],
    claims_with_author: &[ClaimWithAuthor],
    battle_solutions: &[crate::models::battle::BattleSolution],
    config: &PresentationConfig,
) -> WinsMatrix {
    debug_assert_eq!(
        claim_views.len(),
        claims_with_author.len(),
        "claim_views and claims_with_author must be in lockstep"
    );
    use crate::services::solution_names::normalize_name;
    use std::collections::{BTreeMap, HashMap, HashSet};

    // ---------------------------------------------------------------
    // 1. Build the ordered claim column list (newest first).
    //    A claim contributes a column iff it has at least one
    //    leaderboard row with a non-empty normalised solution name.
    //    Claims with no structured leaderboard are skipped — they'd
    //    render as all-`—` columns, which is just noise.
    // ---------------------------------------------------------------

    // Best (lowest) rank a solution achieved in a given claim, plus
    // the first appearance's display name + avatar fields + per-row
    // logo URL. Dedupes when one solution shows up in multiple rows
    // of the same claim.
    struct InClaimRank {
        rank: u32,
        /// Pre-formatted compact score label rendered in the matrix
        /// cell — added 0.14.2 when the cell switched from "rank"
        /// to "score". Falls back to `#<rank>` notation when the
        /// underlying leaderboard row has no usable score.
        score_label: String,
        display_name: String,
        logo_url: Option<String>,
        logo_initials: String,
        logo_color: String,
        logo_text_color: String,
    }
    struct ClaimColumn {
        header: ClaimHeader,
        rank_by_solution: BTreeMap<String, InClaimRank>,
        /// Column ordering key (since 0.26.1; total-spend since
        /// 0.27.x): `(total$ effort + ε) × (1 + verifies − refutes)`.
        /// Columns are sorted by this descending, deduped by benchmark
        /// family, then capped to `MAX_WINS_MATRIX_COLS`, so the table
        /// shows the highest-investment, best-corroborated distinct
        /// benchmarks and stays readable. `created_at` is the
        /// deterministic tiebreak.
        sort_key: f64,
        created_at: chrono::DateTime<chrono::Utc>,
    }

    let mut columns: Vec<ClaimColumn> = Vec::new();
    for (view, with_author) in claim_views.iter().zip(claims_with_author.iter()) {
        let mut rank_by_solution: BTreeMap<String, InClaimRank> = BTreeMap::new();
        for row in view.leaderboard.iter() {
            let key = normalize_name(&row.solution);
            if key.is_empty() {
                continue;
            }
            let take = match rank_by_solution.get(&key) {
                None => true,
                Some(cur) => row.rank < cur.rank,
            };
            if take {
                rank_by_solution.insert(
                    key,
                    InClaimRank {
                        rank: row.rank,
                        score_label: cell_score_label(row.score, row.rank),
                        display_name: row.solution.clone(),
                        logo_url: row.logo_url.clone(),
                        logo_initials: row.logo_initials.clone(),
                        logo_color: row.logo_color.clone(),
                        logo_text_color: row.logo_text_color.clone(),
                    },
                );
            }
        }
        if rank_by_solution.is_empty() {
            continue;
        }
        let id = with_author.row.id;
        let slug = with_author.row.slug.clone();
        let link = match slug.as_deref() {
            Some(s) if !s.is_empty() => format!("/claims/{}", s),
            _ => format!("/claims/{}", id),
        };
        // Column header = the BENCHMARK name, not the claim title (which
        // just teased the winner). Prefer the LLM-picked `short_label`;
        // else heuristically shorten the benchmark name; else fall back
        // to the claim title. `title_full` (the `title=` hover) carries
        // the full benchmark name so the reader sees what the column is.
        let bench_full = view
            .benchmark
            .as_ref()
            .map(|b| b.name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| view.claim_title.clone());
        let bench_short = view
            .benchmark
            .as_ref()
            .and_then(|b| b.short_label.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| shorten_benchmark_label(&bench_full));
        let title_full = bench_full;
        let (title_truncated, _) = truncate_with_full(&bench_short, 22);
        // Ordering key (since 0.27.x): `(total$ effort + ε) ×
        // corroboration`. `total$ effort` is the FULL spend on the claim
        // — author compute (`effort_usd`) + sponsored proxy / API spend
        // (`external_cost_usd`) + KrabReview spend (`review_cost_usd`) —
        // not author compute alone, which on proxy-heavy battles is $0
        // for almost every claim and collapses the ranking to a raw
        // verify count. The proxy / review components are gated by the
        // battle's `PresentationConfig`. The `+ ε` keeps $0-effort claims
        // on the board. `corroboration` (see `corroboration_factor`)
        // lifts well-verified claims and demotes controversial ones via a
        // ratio gate. `created_at` is the deterministic tiebreak
        // (newest-first).
        let mut effort_total = view.effort_usd;
        if config.effort_includes_external {
            effort_total += view.external_cost_usd.unwrap_or(0.0);
        }
        if config.effort_includes_review {
            effort_total += view.review_cost_usd.unwrap_or(0.0);
        }
        let sort_key = (effort_total + 0.01)
            * corroboration_factor(
                view.verify_count,
                view.refute_count,
                config.controversy_refute_ratio,
                config.controversy_demotion,
            );
        columns.push(ClaimColumn {
            header: ClaimHeader {
                id,
                slug,
                title_truncated,
                title_full,
                link,
            },
            rank_by_solution,
            sort_key,
            created_at: with_author.row.created_at,
        });
    }

    if columns.is_empty() {
        return WinsMatrix::default();
    }

    // Order columns by `(total$ effort + ε) × (1 + verifies − refutes)`
    // descending (since 0.26.1): the highest-investment, best-
    // corroborated claims come first. `created_at` descending (newest first) is the
    // deterministic tiebreak — including the common case where every
    // claim has zero recorded effort, which falls back to the prior
    // recency order. `id` is the final tiebreak for full determinism.
    columns.sort_by(|a, b| {
        b.sort_key
            .partial_cmp(&a.sort_key)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| a.header.id.cmp(&b.header.id))
    });

    // Total claims that contributed a leaderboard — captured for the
    // "Top N of M claims" header.
    let total_ranked = columns.len();

    // NOTE: the rows + Wins are aggregated over ALL columns below, so
    // the Wins total stays the true count (and matches the pie chart /
    // champion). Only the *displayed* columns are capped — at the very
    // end of this function, once wins/appearances are computed.

    // ---------------------------------------------------------------
    // 2. Build the per-solution rows. Walk every column, register
    //    each normalised solution exactly once globally, and fill in
    //    the per-column rank vector.
    // ---------------------------------------------------------------

    struct SolutionAgg {
        solution_name: String,
        logo_initials: String,
        logo_color: String,
        logo_text_color: String,
        logo_url: Option<String>,
        ranks: Vec<Option<u32>>,
        /// Parallel to `ranks` — formatted score label rendered in
        /// each cell. Empty string for `None` rank cells (template
        /// still emits the `—` glyph based on `ranks[i]`).
        cell_labels: Vec<String>,
    }

    let col_count = columns.len();
    let mut state: BTreeMap<String, SolutionAgg> = BTreeMap::new();

    for (col_idx, col) in columns.iter().enumerate() {
        for (norm_key, in_claim) in col.rank_by_solution.iter() {
            let entry = state
                .entry(norm_key.clone())
                .or_insert_with(|| SolutionAgg {
                    solution_name: in_claim.display_name.clone(),
                    logo_initials: in_claim.logo_initials.clone(),
                    logo_color: in_claim.logo_color.clone(),
                    logo_text_color: in_claim.logo_text_color.clone(),
                    logo_url: in_claim.logo_url.clone(),
                    ranks: vec![None; col_count],
                    cell_labels: vec![String::new(); col_count],
                });
            entry.ranks[col_idx] = Some(in_claim.rank);
            entry.cell_labels[col_idx] = in_claim.score_label.clone();
        }
    }

    if state.is_empty() {
        return WinsMatrix::default();
    }

    // Battle's curated logo map wins over the per-row inline URL.
    // GCP-only: only self-hosted logos are eligible (from the curated
    // solutions or the per-row inline value); a non-self-hosted URL is
    // dropped so no external favicon renders on the battle page / OG card.
    use crate::services::solution_names::is_self_hosted_logo;
    let battle_logo_map: HashMap<&str, &str> = battle_solutions
        .iter()
        .filter_map(|s| {
            s.logo_url
                .as_deref()
                .filter(|u| is_self_hosted_logo(u))
                .map(|u| (s.name_normalized.as_str(), u))
        })
        .collect();

    let per_variant_rows: Vec<WinsMatrixRow> = state
        .into_iter()
        .map(|(norm_key, a)| {
            let logo_url = battle_logo_map
                .get(norm_key.as_str())
                .map(|u| (*u).to_string())
                .or_else(|| a.logo_url.clone().filter(|u| is_self_hosted_logo(u)));
            let wins = a.ranks.iter().filter(|r| **r == Some(1)).count() as u32;
            let appearances = a.ranks.iter().filter(|r| r.is_some()).count() as u32;
            let solution_name = a.solution_name.clone();
            WinsMatrixRow {
                solution_name,
                logo_url,
                logo_initials: a.logo_initials,
                logo_color: a.logo_color,
                logo_text_color: a.logo_text_color,
                brand_color: None, // filled from battle_solutions after the cap
                wins,
                // Computed after the column cap (over displayed columns).
                points: 0,
                appearances,
                ranks: a.ranks,
                cell_labels: a.cell_labels,
                variant_names: vec![a.solution_name],
            }
        })
        .collect();

    // ---------------------------------------------------------------
    // 3. Vendor grouping (since 0.14.7). Multiple `name_normalized`
    //    variants can share the same `logo_url` — e.g. on the "Web
    //    Access API" battle, three Brightdata SKUs all route to
    //    google.com via the parens-aware vendor lookup added in
    //    0.14.5. Collapse those into a single row so the matrix
    //    reads as "one row per vendor" rather than "one row per
    //    product SKU".
    //
    //    Grouping key = `logo_url`. Rows whose `logo_url` is `None`
    //    NEVER merge (unknown-vendor fallback — preserves per-row
    //    isolation for solutions the vendor map can't resolve;
    //    otherwise we'd accidentally collapse unrelated rows).
    //
    //    Per group:
    //      - `ranks[i]` = min of all variants' Some(_) ranks in
    //        column `i`, else None.
    //      - `cell_labels[i]` = label from whichever variant owns
    //        the winning (lowest-rank) cell at `i`. Empty when
    //        no variant appeared.
    //      - `wins` = count of `Some(1)` in the merged ranks.
    //      - `appearances` = count of `Some(_)` in the merged
    //        ranks (= distinct claims where ANY variant appeared;
    //        multiple variants in the same claim don't double-count).
    //      - `display_name` = variant with the most appearances
    //        in this group; ties → alphabetical by name. The row's
    //        `solution_name` is set to `display_name`.
    //      - `variant_names` = every merged variant's display name,
    //        used by the template's `title=` hover tooltip.
    //      - `logo_*` fallback fields carry through from the
    //        display-name variant (only used when `logo_url` is
    //        None, which can't happen post-grouping — but stays
    //        sane on the WinsMatrixRow shape regardless).
    // ---------------------------------------------------------------
    use std::collections::HashMap as StdHashMap;
    let mut by_logo: StdHashMap<String, Vec<WinsMatrixRow>> = StdHashMap::new();
    let mut unkeyed: Vec<WinsMatrixRow> = Vec::new();
    for row in per_variant_rows.into_iter() {
        // Group on the logo's identity (URL minus the `?v=` cache-buster)
        // so a vendor whose rows carry different versions of the same
        // asset still merges into one standings row.
        match row
            .logo_url
            .as_deref()
            .map(crate::services::solution_names::logo_identity)
        {
            Some(url) => by_logo.entry(url.to_string()).or_default().push(row),
            None => unkeyed.push(row),
        }
    }

    let mut rows: Vec<WinsMatrixRow> = Vec::with_capacity(by_logo.len() + unkeyed.len());
    rows.extend(unkeyed);
    for (_, variants) in by_logo.into_iter() {
        if variants.len() == 1 {
            // Single-variant vendor group — no merge needed; still
            // carries the one-element variant_names vector so the
            // template's tooltip suppression key (len > 1) works
            // uniformly.
            rows.push(variants.into_iter().next().unwrap());
            continue;
        }

        // Pick the display variant: most appearances → alphabetical
        // tiebreak by solution_name.
        let mut display_idx = 0usize;
        for (idx, v) in variants.iter().enumerate().skip(1) {
            let cur = &variants[display_idx];
            let take = match v.appearances.cmp(&cur.appearances) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Equal => v.solution_name < cur.solution_name,
                std::cmp::Ordering::Less => false,
            };
            if take {
                display_idx = idx;
            }
        }

        // Per-column min-rank merge.
        let col_count = variants[0].ranks.len();
        let mut merged_ranks: Vec<Option<u32>> = vec![None; col_count];
        let mut merged_labels: Vec<String> = vec![String::new(); col_count];
        for v in variants.iter() {
            for (i, r) in v.ranks.iter().enumerate() {
                if let Some(r) = r {
                    let replace = match merged_ranks[i] {
                        None => true,
                        Some(cur) => *r < cur,
                    };
                    if replace {
                        merged_ranks[i] = Some(*r);
                        merged_labels[i] = v.cell_labels[i].clone();
                    }
                }
            }
        }
        let wins = merged_ranks.iter().filter(|r| **r == Some(1)).count() as u32;
        let appearances = merged_ranks.iter().filter(|r| r.is_some()).count() as u32;

        // Collect variant names alphabetically for a stable tooltip
        // ordering across renders.
        let mut variant_names: Vec<String> =
            variants.iter().map(|v| v.solution_name.clone()).collect();
        variant_names.sort();
        variant_names.dedup();

        let display = variants[display_idx].clone();
        rows.push(WinsMatrixRow {
            solution_name: display.solution_name.clone(),
            logo_url: display.logo_url.clone(),
            logo_initials: display.logo_initials,
            logo_color: display.logo_color,
            logo_text_color: display.logo_text_color,
            brand_color: None, // filled from battle_solutions after the cap
            wins,
            points: 0,
            appearances,
            ranks: merged_ranks,
            cell_labels: merged_labels,
            variant_names,
        });
    }

    // Default sort: wins DESC, appearances DESC (fewer `—` cells
    // beats more), solution_name ASC for a deterministic tiebreak.
    // `wins` / `appearances` here are the FULL totals (over all
    // columns), so this is the true leaderboard order.
    rows.sort_by(|a, b| {
        b.wins
            .cmp(&a.wins)
            .then_with(|| b.appearances.cmp(&a.appearances))
            .then_with(|| a.solution_name.cmp(&b.solution_name))
    });

    // Display cap (since 0.26.1): keep the Wins total honest (computed
    // over every claim above) but only render the top
    // `config.max_wins_cols` columns so the table stays readable.
    // Columns are already sorted by `(total$ effort + ε) × corroboration`
    // descending. We walk that order and (when `config.dedup_benchmarks`)
    // keep the best column per benchmark *family* (since 0.27.x): a
    // battle with three `SimpleQA-*` variants would otherwise spend three
    // of its five slots on one benchmark — dedup keeps only the
    // top-ranked variant and lets other benchmarks onto the board. `wins`
    // / `appearances` stay the full totals (a row can show a Wins count
    // higher than its visible 🏆 cells — the "Top N of M claims" header
    // explains the gap), and the kept columns preserve sort order.
    let mut seen_family: HashSet<String> = HashSet::new();
    let mut keep: Vec<usize> = Vec::new();
    for (i, col) in columns.iter().enumerate() {
        let fresh_family = !config.dedup_benchmarks
            || seen_family.insert(benchmark_family_key(&col.header.title_full));
        if fresh_family {
            keep.push(i);
            if keep.len() >= config.max_wins_cols {
                break;
            }
        }
    }
    if keep.len() < total_ranked {
        // Reindex each row's parallel `ranks` / `cell_labels` vectors to
        // the kept columns (in their existing sort order), then drop the
        // unkept columns from `columns`.
        for row in rows.iter_mut() {
            row.ranks = keep.iter().map(|&i| row.ranks[i]).collect();
            row.cell_labels = keep.iter().map(|&i| row.cell_labels[i].clone()).collect();
        }
        let keep_set: HashSet<usize> = keep.iter().copied().collect();
        let mut idx = 0usize;
        columns.retain(|_| {
            let k = keep_set.contains(&idx);
            idx += 1;
            k
        });
        // Drop pure-noise rows: no wins anywhere AND no cell visible in
        // the shown columns. (Rows with wins keep showing their total
        // even when none of their winning claims survived the cap.)
        rows.retain(|r| r.wins > 0 || r.ranks.iter().any(|c| c.is_some()));
    }

    // Brand colors (0.28.5): the battle's curated `solutions[*].brand_color`
    // (extracted from the stored logo) keyed by normalised name, so each
    // standing row can color its Claims Wins pie slice with the real brand
    // color instead of the hash-derived avatar color.
    let brand_color_map: HashMap<&str, &str> = battle_solutions
        .iter()
        .filter_map(|s| {
            s.brand_color
                .as_deref()
                .map(|c| (s.name_normalized.as_str(), c))
        })
        .collect();

    // Olympic standing (since 0.28.x): score each row over the DISPLAYED
    // columns (gold/silver/bronze = config points) and order the
    // standings by it — not by raw win count. Computed HERE, after the
    // cap, so it reflects exactly the rendered top-N benchmarks (the
    // "use these top-5 competitions" rule). Tiebreaks: more golds, then
    // more appearances (fewer `—`), then name for determinism.
    for row in rows.iter_mut() {
        row.points = medal_points(
            &row.ranks,
            config.gold_points,
            config.silver_points,
            config.bronze_points,
        );
        row.brand_color = brand_color_map
            .get(normalize_name(&row.solution_name).as_str())
            .map(|c| (*c).to_string());
    }
    rows.sort_by(|a, b| {
        b.points
            .cmp(&a.points)
            .then_with(|| b.wins.cmp(&a.wins))
            .then_with(|| b.appearances.cmp(&a.appearances))
            .then_with(|| a.solution_name.cmp(&b.solution_name))
    });

    // Canonical per-row colour (since 0.28.12): resolve ONE colour per
    // standing row — preferring the curated brand_color, else the
    // deterministic avatar logo_color — and disambiguate collisions via
    // `distinct_wins_colors`. Writing it back into `logo_color` makes
    // the avatar badge, the matrix, the home mini-matrix, and the pie
    // slice all use the same colour. Previously the pie deduped on its
    // own while the avatar kept the raw (possibly colliding) logo_color,
    // so two solutions in the same letter bucket (e.g. "Claude Skill"=C
    // and "MCP server"=M) showed identical avatars beside a correctly
    // two-colour pie. Run in final display order so the top row keeps
    // its preferred colour and later rows yield on collision.
    {
        let preferred: Vec<&str> = rows
            .iter()
            .map(|r| {
                r.brand_color
                    .as_deref()
                    .filter(|c| !c.trim().is_empty())
                    .unwrap_or(r.logo_color.as_str())
            })
            .collect();
        let resolved = distinct_wins_colors(&preferred);
        for (row, color) in rows.iter_mut().zip(resolved) {
            // Keep the hand-tuned text colour when the row keeps its own
            // colour; recompute contrast only for a reassigned colour.
            if !color.eq_ignore_ascii_case(&row.logo_color) {
                row.logo_text_color = contrast_text(&color).to_string();
            }
            row.logo_color = color;
        }
    }

    let claim_count = columns.len();
    let claims: Vec<ClaimHeader> = columns.into_iter().map(|c| c.header).collect();

    WinsMatrix {
        claim_count,
        total_ranked,
        claims,
        rows,
    }
}

/// Pick the champion banner shown above the wins matrix (since
/// 0.14.2). See `Champion` for the precedence chain. Returns
/// `None` on empty matrices.
///
/// "Names a solution" = case-insensitive substring match between
/// the summary's first paragraph and any matrix row's
/// `solution_name`. When the summary mentions multiple matrix
/// solutions, the **leftmost mention** (earliest character offset
/// in the summary) wins — this matches the reader's intuition
/// that whoever the reviewer leads with is the champion. Ties on
/// offset (e.g. two solutions starting at the same word boundary
/// via overlap) fall back to wins-desc.
///
/// "First sentence" of the summary = everything up to the first
/// `. ` / `! ` / `? ` (or end of string), capped at 200 chars
/// so a missing period in a malformed summary can't paste the
/// entire editorial into the banner.
fn build_champion(matrix: &WinsMatrix, executive_summary: Option<&str>) -> Option<Champion> {
    if matrix.is_empty() {
        return None;
    }

    if let Some(summary) = executive_summary {
        let summary_trimmed = summary.trim();
        if !summary_trimmed.is_empty() {
            // Skip markdown headings to find the first paragraph.
            let first_para = summary_trimmed
                .split("\n\n")
                .map(str::trim)
                .find(|p| !p.is_empty() && !p.starts_with('#'))
                .unwrap_or("");
            if !first_para.is_empty() {
                let haystack = first_para.to_lowercase();
                // Find the earliest-mentioned matrix solution.
                // Ties on offset fall back to wins-desc by
                // walking matrix.rows in order.
                let mut best: Option<(usize, usize)> = None; // (offset, row_idx)
                for (idx, row) in matrix.rows.iter().enumerate() {
                    let needle = row.solution_name.trim().to_lowercase();
                    if needle.is_empty() {
                        continue;
                    }
                    if let Some(pos) = haystack.find(&needle) {
                        match best {
                            None => best = Some((pos, idx)),
                            Some((cur_pos, _)) if pos < cur_pos => best = Some((pos, idx)),
                            _ => {}
                        }
                    }
                }
                if let Some((_, idx)) = best {
                    let row = &matrix.rows[idx];
                    return Some(Champion {
                        solution_name: row.solution_name.clone(),
                        note: first_sentence(first_para, 200),
                        from_executive_summary: true,
                    });
                }
            }
        }
    }

    // Fallback — standings leader. `matrix.rows[0]` is already the top
    // of the default sort (points desc → wins desc → appearances desc →
    // alphabetical). When everyone has 0 points (a battle with no top-3
    // cells in the displayed columns) we still surface the first row but
    // with an honest note. Points are over the DISPLAYED columns, so the
    // note counts `claim_count` (rendered benchmarks), not total_ranked.
    let leader = &matrix.rows[0];
    let benches = matrix.claim_count;
    let bench_word = if benches == 1 {
        "benchmark"
    } else {
        "benchmarks"
    };
    let note = if leader.points > 0 {
        format!(
            "Leads the standings with {} {} across the top {} {}.",
            leader.points,
            if leader.points == 1 {
                "point"
            } else {
                "points"
            },
            benches,
            bench_word
        )
    } else {
        format!(
            "Top of the standings across the top {} {}.",
            benches, bench_word
        )
    };
    Some(Champion {
        solution_name: leader.solution_name.clone(),
        note,
        from_executive_summary: false,
    })
}

/// Truncate to the first sentence (looking for `. ` / `! ` / `? `)
/// and hard-cap at `cap` chars. Trailing ellipsis only when the
/// cap actually clipped mid-string. Used by `build_champion` so a
/// malformed summary without periods can't paste a 10K-char
/// editorial into the banner.
fn first_sentence(s: &str, cap: usize) -> String {
    let s = s.trim();
    let mut end = s.len();
    for (i, _) in s.char_indices() {
        if i + 2 > s.len() {
            break;
        }
        let bytes = &s.as_bytes()[i..];
        if bytes.len() >= 2
            && (bytes[0] == b'.' || bytes[0] == b'!' || bytes[0] == b'?')
            && (bytes[1] == b' ' || bytes[1] == b'\n')
        {
            end = i + 1; // keep the punctuation
            break;
        }
    }
    let truncated = &s[..end.min(s.len())];
    if truncated.chars().count() > cap {
        let mut out: String = truncated.chars().take(cap.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        truncated.to_string()
    }
}

/// Hard-cap a notes cell at `cap` chars, returning `(truncated,
/// Option<full>)`. The optional `full` is `Some` only when the input
/// was actually trimmed — saves a `title=` attribute on rows that
/// already fit.
fn truncate_with_full(s: &str, cap: usize) -> (String, Option<String>) {
    if s.chars().count() <= cap {
        return (s.to_string(), None);
    }
    let mut end = 0usize;
    for (count, (idx, _)) in s.char_indices().enumerate() {
        if count >= cap.saturating_sub(1) {
            break;
        }
        end = idx;
    }
    let mut out = s[..end].trim_end().to_string();
    out.push('…');
    (out, Some(s.to_string()))
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BenchmarkView {
    pub name: String,
    /// LLM-picked crisp ≤~20-char label for the Claims Wins column
    /// header. `None` → matrix falls back to a heuristic shortening of
    /// `name`. Sourced from `claim_structure.benchmark.short_label`.
    pub short_label: Option<String>,
    pub task: String,
    pub dataset: String,
    pub dataset_url: Option<String>,
    pub metric: String,
    pub metric_unit: Option<String>,
    pub higher_is_better: bool,
    pub setup: String,
    pub reproduction_command: Option<String>,
    /// Pre-formatted author-reported cost line for the benchmark
    /// card. `None` when `structure.json.cost` is absent or has no
    /// usable fields (`tokens` / `usd` both missing). Format
    /// examples: `"32,000 tokens · $0.84"`,
    /// `"32,000 tokens"`, `"$0.84 (Claude Opus 4.7)"`.
    pub cost_label: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LeaderboardRowView {
    pub rank: u32,
    pub solution: String,
    /// Score as a string, formatted with the metric unit when present
    /// (`"78.0%"`, `"123 ms"`). The raw float would lose trailing
    /// zeros that authors deliberately wrote.
    pub score_label: String,
    /// Raw score (`structure.json.leaderboard[i].score`) passed
    /// through unmodified. Used by the Battle-page wins matrix
    /// (since 0.14.2) to render the numeric score in each cell
    /// (rendered via `format_score_compact`); display ≠ sort, the
    /// matrix still sorts by `rank` ascending.
    pub score: f64,
    pub notes: String,
    pub evidence_url: Option<String>,
    /// Convenience for the template — true when this row is in the
    /// claim's `winners` array. Lets us highlight the row(s) without
    /// re-comparing strings inside askama.
    pub is_winner: bool,
    /// Up to 2 uppercase ASCII chars used inside the row's avatar
    /// circle on the leaderboard. Empty when the solution starts
    /// with a non-Latin character (the template then falls back to
    /// the neutral gray logo with no glyphs).
    pub logo_initials: String,
    /// CSS color for the avatar background. Deterministically derived
    /// from the first ASCII-letter of the solution; non-Latin starts
    /// get the neutral `--ink-3`.
    pub logo_color: String,
    /// `#fff` or `#1a1a18` chosen by contrast against `logo_color`.
    pub logo_text_color: String,
    /// Optional vendor / product logo URL. Sourced directly from each
    /// claim's `structure.json.leaderboard[i].logo_url` (added in
    /// 0.12.0; replaces the deprecated `solutions` registry table).
    /// When `Some`, the leaderboard template renders an `<img>` next
    /// to the solution name and suppresses the coloured-initials
    /// avatar; when `None` the existing avatar fallback renders
    /// unchanged.
    pub logo_url: Option<String>,
}

impl ClaimCardView {
    fn extract_title_body(text: &str) -> (String, String) {
        for line in text.lines() {
            if let Some(title) = line.trim().strip_prefix("# ") {
                let body = text[text.find(line).unwrap_or(0) + line.len()..]
                    .trim_start_matches('\n')
                    .to_string();
                return (title.trim().to_string(), body);
            }
        }
        // No # heading — first line is title, rest is body
        let first = text.lines().next().unwrap_or(text).to_string();
        let body = text[first.len()..].trim_start_matches('\n').to_string();
        (first, body)
    }

    fn agent_initials_for(backend: &str) -> String {
        let s: String = backend
            .trim()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .take(2)
            .collect();
        if s.is_empty() { "?".to_string() } else { s }
    }

    fn from_row_with_author(
        row: &ClaimRow,
        author_name: Option<&str>,
        author_avatar: Option<String>,
    ) -> Self {
        let (vp, rp) = split_pct(row.verify_count as i64, row.refute_count as i64);
        let (claim_title, claim_body) = Self::extract_title_body(&row.claim);
        let winners = row.winners.clone().unwrap_or_default();
        let (benchmark, leaderboard, winners_label) =
            Self::project_structure(row.claim_structure.as_ref(), &winners);
        let external_cost_usd = Self::project_external_cost(row.claim_structure.as_ref());
        let now = chrono::Utc::now();
        let created_relative = relative_age(now, row.created_at);
        // Show "updated X ago" only when the gap between created_at and
        // updated_at is meaningful (>30s) — avoids showing "added 2s ago
        // / updated 2s ago" right after a fresh post where the
        // verify/refute hook hasn't fired yet.
        let updated_relative = if row.updated_at.signed_duration_since(row.created_at)
            > chrono::Duration::seconds(30)
        {
            Some(relative_age(now, row.updated_at))
        } else {
            None
        };
        // `effort_usd` here only includes the author's reported cost.
        // The reviewer's Gemini cost is on `claim_reviews.cost_micros`
        // and only fills in once the caller `populate_review_cost`-s
        // it from a JOIN. Templates render whichever is available.
        let effort_usd = row.price_cents.unwrap_or(0).max(0) as f64 / 100.0;
        let agent_backend = row.agent_backend.clone().unwrap_or_default();
        let agent_initials = Self::agent_initials_for(&agent_backend);
        Self {
            id: row.id.to_string(),
            claim: row.claim.clone(),
            claim_title,
            claim_body,
            created_relative,
            updated_relative,
            effort_usd,
            attachments: Vec::new(), // filled by caller via load_attachments
            artifact_summary: row.artifact_summary.clone().unwrap_or_default(),
            og_summary: row.og_summary.clone(),
            verify_count: row.verify_count,
            refute_count: row.refute_count,
            verify_pct: vp,
            refute_pct: rp,
            agent_backend,
            agent_initials,
            agent_model: row.agent_model.clone().unwrap_or_default(),
            upvote_count: row.upvote_count,
            downvote_count: row.downvote_count,
            visibility: row.visibility.clone().unwrap_or_else(|| "public".into()),
            price_cents: row.price_cents,
            tokens_consumed: row.tokens_consumed,
            wall_clock_ms: row.wall_clock_ms,
            benchmark,
            leaderboard,
            winners_label,
            // Populated by callers via `populate_review_cost` after
            // joining `claim_reviews`. Default to None on bare from_row.
            review_tokens_total: None,
            review_cost_usd: None,
            external_cost_usd,
            topic: row.topic.clone(),
            tags_label: row
                .topic_tags
                .as_ref()
                .map(|t| t.join(", "))
                .unwrap_or_default(),
            author_display_name: author_name.unwrap_or("Unknown").to_string(),
            author_avatar_url: author_avatar,
            author_initials: {
                let name = author_name.unwrap_or("?");
                let mut parts = name.split_whitespace();
                let first = parts.next().and_then(|s| s.chars().next());
                let second = parts.next().and_then(|s| s.chars().next());
                match (first, second) {
                    (Some(a), Some(b)) => format!("{a}{b}").to_uppercase(),
                    (Some(a), None) => a.to_uppercase().to_string(),
                    _ => "?".to_string(),
                }
            },
            produced_by: crate::models::producer::resolve_all(
                row.produced_by.as_deref().unwrap_or(&[]),
            ),
        }
    }

    /// Pull `claim_structure -> 'cost' -> 'external_usd'` out of the
    /// raw JSONB. Returns `None` if the structure is missing, malformed,
    /// or the field is absent / non-positive (zero is "no data" — same
    /// suppression rule as the chip).
    fn project_external_cost(raw: Option<&serde_json::Value>) -> Option<f64> {
        use krabarena_shared::claim_structure::ClaimStructure;
        let parsed: ClaimStructure = serde_json::from_value(raw?.clone()).ok()?;
        let usd = parsed.cost.as_ref()?.external_usd?;
        if usd.is_finite() && usd > 0.0 {
            Some(usd)
        } else {
            None
        }
    }

    /// `12345` → `"12,345"`. Compact pre-formatted token string for
    /// the claim-page stats strip, mirroring `cost_label`'s formatting
    /// so the two surfaces read consistently. Returns `None` when the
    /// value is missing or zero (no chip rendered).
    pub fn tokens_str(&self) -> Option<String> {
        let t = self.tokens_consumed?;
        if t <= 0 {
            return None;
        }
        Some(format_token_count_with_commas(t as i64))
    }

    /// `$5.0000` for the author's compute spend. `None` when
    /// `price_cents` is missing or zero. Four decimals to match the
    /// benchmark-card cost line and the agent / arena rollups.
    pub fn author_cost_str(&self) -> Option<String> {
        let cents = self.price_cents?;
        if cents <= 0 {
            return None;
        }
        Some(format!("${:.4}", cents as f64 / 100.0))
    }

    /// `$0.5100` for `claim_structure.cost.external_usd`. `None` when
    /// the value is absent or zero.
    pub fn external_cost_str(&self) -> Option<String> {
        let usd = self.external_cost_usd?;
        if !usd.is_finite() || usd <= 0.0 {
            return None;
        }
        Some(format!("${:.4}", usd))
    }

    /// `$0.0125` for the KrabReview LLM spend on this claim. `None`
    /// when `review_cost_usd` is missing or zero. Distinct from
    /// `review_cost_label` (the benchmark-card line which combines
    /// tokens + dollars with em-dash fallback).
    pub fn krabreview_cost_str(&self) -> Option<String> {
        let usd = self.review_cost_usd?;
        if !usd.is_finite() || usd <= 0.0 {
            return None;
        }
        Some(format!("${:.4}", usd))
    }

    /// Pre-formatted "KrabReview cost." line for the claim page.
    /// Returns `Some("12,345 tokens · $0.0125")` when both tokens and
    /// cost are populated, `Some("12,345 tokens · —")` when only the
    /// token count is known, and `None` when both are absent or tokens
    /// is zero (legacy review pre-v0.8.2 — render no line at all).
    pub fn review_cost_label(&self) -> Option<String> {
        let tokens = self.review_tokens_total?;
        if tokens <= 0 {
            return None;
        }
        let cost_part = match self.review_cost_usd {
            Some(c) if c.is_finite() && c > 0.0 => {
                if c >= 1.0 {
                    format!("${:.2}", c)
                } else {
                    format!("${:.4}", c)
                }
            }
            _ => "—".to_string(),
        };
        Some(format!(
            "{} tokens · {}",
            format_token_count_with_commas(tokens as i64),
            cost_part
        ))
    }

    /// Decode the row's `claim_structure` JSONB into template-ready
    /// view models. Tolerant of partial / older data — anything that
    /// fails to parse drops to `None` so the template renders the
    /// legacy fallback rather than 500-ing the page.
    fn project_structure(
        raw: Option<&serde_json::Value>,
        winners: &[String],
    ) -> (Option<BenchmarkView>, Vec<LeaderboardRowView>, String) {
        use krabarena_shared::claim_structure::ClaimStructure;
        let Some(value) = raw else {
            return (None, Vec::new(), String::new());
        };
        let parsed: ClaimStructure = match serde_json::from_value(value.clone()) {
            Ok(p) => p,
            Err(_) => return (None, Vec::new(), String::new()),
        };
        let cost_label = parsed.cost.as_ref().and_then(format_cost_label);
        let benchmark = BenchmarkView {
            name: parsed.benchmark.name.clone(),
            short_label: parsed.benchmark.short_label.clone(),
            task: parsed.benchmark.task.clone(),
            dataset: parsed.benchmark.dataset.clone(),
            dataset_url: safe_http_url(parsed.benchmark.dataset_url.clone()),
            metric: parsed.benchmark.metric.clone(),
            metric_unit: parsed.benchmark.metric_unit.clone(),
            higher_is_better: parsed.benchmark.higher_is_better,
            setup: parsed.benchmark.setup.clone(),
            reproduction_command: parsed.benchmark.reproduction_command.clone(),
            cost_label,
        };
        let unit = parsed.benchmark.metric_unit.clone().unwrap_or_default();
        let mut entries = parsed.leaderboard.clone();
        entries.sort_by_key(|e| e.rank);
        let rows = entries
            .into_iter()
            .map(|e| {
                let is_winner = winners.iter().any(|w| w == &e.solution);
                let score_label = hero_value_label(e.score, &unit, &parsed.benchmark.metric);
                let (logo_initials, logo_color, logo_text_color) =
                    leaderboard_logo_style(&e.solution);
                LeaderboardRowView {
                    rank: e.rank,
                    solution: e.solution,
                    score_label,
                    score: e.score,
                    notes: e.notes.unwrap_or_default(),
                    evidence_url: safe_http_url(e.evidence_url),
                    is_winner,
                    logo_initials,
                    logo_color,
                    logo_text_color,
                    // Sourced inline from the claim's structure.json
                    // (since 0.12.0); legacy claims without the field
                    // render the coloured-initials avatar fallback.
                    // GCP-only: drop an author-supplied external URL so the
                    // vendor-map (self-hosted) logo is used instead of an
                    // external favicon. Never render a third-party logo host.
                    logo_url: e
                        .logo_url
                        .filter(|u| crate::services::solution_names::is_self_hosted_logo(u)),
                }
            })
            .collect();
        let winners_label = winners.join(", ");
        (Some(benchmark), rows, winners_label)
    }
}

/// Map the first Latin letter of a leaderboard solution name to one of
/// ten pre-validated avatar colours. The returned tuple is
/// `(initials, bg, text)`:
///
/// * `initials` — up to two uppercase ASCII chars taken from the
///   first two non-whitespace, non-punctuation characters. Always
///   safe to drop into the avatar circle as-is (no ellipsis).
/// * `bg` — CSS colour for the circle background; deterministic by
///   first ASCII letter so the same solution always renders the same.
///   When the solution starts with a non-Latin character we return
///   `var(--ink-3)` so the row still renders cleanly.
/// * `text` — `#fff` or `#1a1a18`, picked to clear WCAG AA against
///   the chosen background.
fn leaderboard_logo_style(solution: &str) -> (String, String, String) {
    // 10-colour palette with hand-picked text contrast. Index 0..9
    // maps to `(letter - 'A') % 10`.
    const PALETTE: [(&str, &str); 10] = [
        ("#e63946", "#fff"),    // A, K, U — red
        ("#f4a261", "#1a1a18"), // B, L, V — orange (dark text)
        ("#e9c46a", "#1a1a18"), // C, M, W — yellow (dark text)
        ("#2a9d8f", "#fff"),    // D, N, X — teal
        ("#264653", "#fff"),    // E, O, Y — slate
        ("#8338ec", "#fff"),    // F, P, Z — purple
        ("#3a86ff", "#fff"),    // G, Q     — blue
        ("#06d6a0", "#1a1a18"), // H, R     — mint (dark text)
        ("#ff6b6b", "#fff"),    // I, S     — coral
        ("#5e60ce", "#fff"),    // J, T     — indigo
    ];
    let trimmed = solution.trim_start();
    let initials: String = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    let first_letter = trimmed.chars().find(|c| c.is_ascii_alphabetic());
    match first_letter {
        Some(c) => {
            let idx = ((c.to_ascii_uppercase() as u8 - b'A') % 10) as usize;
            let (bg, text) = PALETTE[idx];
            (initials, bg.to_string(), text.to_string())
        }
        None => (initials, "var(--ink-3)".to_string(), "#fff".to_string()),
    }
}

/// Build the cost line for the benchmark card. Returns `None` when
/// neither `tokens` nor `usd` is set (the case where the author wrote
/// only a `note` and we'd render something useless like `"(claude
/// opus 4.7)"`). The note, when present, is appended in parentheses
/// after whatever number(s) we have.
fn format_cost_label(cost: &krabarena_shared::claim_structure::ClaimCost) -> Option<String> {
    let mut parts: Vec<String> = Vec::with_capacity(2);
    if let Some(t) = cost.tokens {
        parts.push(format!(
            "{} tokens",
            format_token_count_with_commas(t as i64)
        ));
    }
    if let Some(usd) = cost.usd
        && usd.is_finite()
        && usd > 0.0
    {
        // Render <$1 with 4 decimals (a typical reviewer call is fractions
        // of a cent), >=$1 with 2 — same readability tradeoff as the
        // arena/agent rollups.
        let s = if usd >= 1.0 {
            format!("${:.2}", usd)
        } else {
            format!("${:.4}", usd)
        };
        parts.push(s);
    }
    if parts.is_empty() {
        return None;
    }
    let mut label = parts.join(" · ");
    if let Some(note) = cost.note.as_deref().filter(|n| !n.trim().is_empty()) {
        label.push_str(&format!(" ({})", note));
    }
    Some(label)
}

/// `1234 → "1,234"`, `1_500_000 → "1,500,000"`. Used in the
/// benchmark-card cost line where authors like to read full digit
/// counts. Distinct from `format_token_count` (the K/M-truncated
/// version used in the stats strip where space is tight).
fn format_token_count_with_commas(n: i64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

// --- Claim verdict strip + hero modules (viral share layer) ---
//
// `PresentationView` is the render-ready shape the `claim_hero.html`
// partial consumes. It's produced by `select_presentation`, which runs
// a fallback ladder:
//   1. `claims.presentation` (reviewer-authored, migration 067) — used
//      verbatim when it parses and names one of the four known
//      layouts; its chart data (frontier dots / race bars / h2h faces)
//      is still built fresh from `claim_structure` so a stale
//      presentation blob can't drift from the leaderboard numbers.
//   2. No usable stored presentation: synthesize one from
//      `claim_structure` alone, in priority order —
//      a definitive (non-"inconclusive") `hypothesis` → `h2h`;
//      else a metrics key shared across rows → `frontier`;
//      else >= 4 leaderboard rows → `race`;
//      else `None` (today's plain leaderboard table stays primary).

/// The four reviewer-authored / synthesizable hero-module layouts.
const PRESENTATION_LAYOUTS: [&str; 4] = ["h2h", "race", "frontier", "myth_buster"];

fn is_valid_presentation_layout(layout: &str) -> bool {
    PRESENTATION_LAYOUTS.contains(&layout)
}

/// Render-ready view model for the claim page's verdict strip + hero
/// module (`templates/components/claim_hero.html`). `None` from
/// [`select_presentation`] means "no hero module" — the page falls
/// back to rendering only the plain leaderboard table, unchanged.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct PresentationView {
    /// Validated member of [`PRESENTATION_LAYOUTS`].
    pub layout: String,
    pub tweet_title: String,
    pub tweet_annotation: String,
    /// "" when the layout has no headline number (e.g. `frontier`).
    pub hero_value: String,
    pub hero_label: String,
    /// "" when this claim isn't a refutation.
    pub refutation_claimed: String,
    pub refutation_measured: String,
    /// `"v"` | `"c"` | `"r"` — drives the stamp pill's colour class.
    pub stamp_class: String,
    pub stamp_label: String,
    /// `Some` only when `layout == "frontier"` and >= 2 rows carry
    /// the shared metrics key.
    pub frontier: Option<FrontierView>,
    /// Populated when `layout` is `"race"` or `"myth_buster"`.
    pub race: Vec<RaceBarView>,
    /// `Some` only when `layout == "h2h"`.
    pub h2h: Option<H2hView>,
    /// Tweet-intent text (`tweet_title`, unencoded — the template
    /// applies `|urlencode`).
    pub share_text: String,
}

/// The `frontier` scatter: axes (titles + nice ticks), dots with
/// server-computed label placement, and the always-rendered legend
/// rows (= `dots`, in leaderboard order). Geometry lives in
/// [`crate::web::frontier_chart`].
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FrontierView {
    pub dots: Vec<FrontierDotView>,
    /// "Latency p50 (s)" — humanized shared `metrics` key + unit.
    pub x_title: String,
    /// "Raw answer presence at 10 (%)" — benchmark metric + unit.
    pub y_title: String,
    pub x_ticks: Vec<crate::web::frontier_chart::AxisTick>,
    pub y_ticks: Vec<crate::web::frontier_chart::AxisTick>,
    /// Screen-reader summary of both axes and the leader.
    pub aria_label: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FrontierDotView {
    /// 1-based legend index (shown inside logo-less markers).
    pub idx: usize,
    pub x_pct: f32,
    pub y_pct: f32,
    pub label: String,
    /// Formatted headline score (y value).
    pub score_label: String,
    /// Formatted secondary metric (x value).
    pub x_label: String,
    pub is_winner: bool,
    pub logo_url: Option<String>,
    /// `data-lbl` (phone) / `data-lbl-wide` (desktop): `r|l|a|b|none`.
    pub label_side: &'static str,
    pub label_side_wide: &'static str,
    /// Horizontal jitter slot for (near-)coincident dots (`data-jit`).
    pub jitter: i32,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RaceBarView {
    pub name: String,
    pub width_pct: f32,
    pub value_label: String,
    pub is_winner: bool,
    /// `myth_buster` highlights the debunked (non-winner) bars
    /// differently from a plain `race`; always `false` for `race`.
    pub is_loser_highlight: bool,
    /// Vendor favicon/logo for the row (same source as the frontier
    /// dots): `leaderboard[*].logo_url`, backfilled from the Battle's
    /// solutions + the vendor-logo map. `None` renders no image.
    pub logo_url: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct H2hView {
    pub left_name: String,
    pub left_value: String,
    pub right_name: String,
    pub right_value: String,
    pub verdict_line: String,
}

/// Stamp `(class, label)` from verify/refute counts:
/// both zero → `UNREVIEWED` (`c`); no refutes → `v`; no verifies →
/// `r`; otherwise contested (`c`). Order matters — the zero/zero case
/// must be checked before the "no refutes" branch, since `(0, 0)`
/// would otherwise match it too.
fn stamp_for(verify_count: i32, refute_count: i32) -> (String, String) {
    if verify_count == 0 && refute_count == 0 {
        return ("c".to_string(), "UNREVIEWED".to_string());
    }
    if refute_count == 0 {
        return ("v".to_string(), format!("✓ VERIFIED ×{verify_count}"));
    }
    if verify_count == 0 {
        return ("r".to_string(), format!("✗ REFUTED ×{refute_count}"));
    }
    (
        "c".to_string(),
        format!("⚔ CONTESTED · {verify_count}✓ {refute_count}✗"),
    )
}

/// Hero-module value labels (`.race-value`, h2h values, frontier dot
/// labels) render with `white-space: nowrap` in a flexible layout
/// column — a long `metric_unit` blows that column's width out and
/// starves its siblings (e.g. `.race-track`) to 0px. Delegates to the
/// shared [`format_metric_compact`], which already drops garbage units
/// (>12 chars or containing whitespace) and additionally scales/labels
/// dimensioned values (a bare "0.102" becomes "102 ms") — a bug the
/// old unit-passthrough version of this helper didn't fix. `unit` and
/// `metric_name` are the benchmark's `metric_unit` / `metric` fields;
/// either may be empty when the claim's structure doesn't carry one.
fn hero_value_label(score: f64, unit: &str, metric_name: &str) -> String {
    let unit_hint = (!unit.is_empty()).then_some(unit);
    let metric_hint = (!metric_name.is_empty()).then_some(metric_name);
    format_metric_compact(score, unit_hint, metric_hint)
}

/// Minimum number of leaderboard rows that must carry a `metrics` key
/// before it can become the frontier scatter's x-axis.
const FRONTIER_MIN_ROWS: usize = 3;

/// Words that carry no meaning when matching a metrics key against the
/// claim's headline: stopwords plus units / aggregation suffixes
/// (`google_url_overlap_pct` → {google, url, overlap}).
const FRONTIER_KEY_NOISE: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "avg", "by", "count", "for", "from", "in", "is", "it",
    "its", "mean", "median", "ms", "n", "num", "of", "on", "only", "or", "pct", "per", "percent",
    "rate", "s", "sec", "secs", "second", "seconds", "than", "that", "the", "their", "to", "total",
    "usd", "vs", "with",
];

/// Meaningful lowercase words of a metrics key or a piece of headline
/// text: split on anything non-alphanumeric, drop noise words and pure
/// numbers, and fold a plural `s` (`answers` → `answer`).
fn frontier_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|w| !w.is_empty() && !w.chars().all(|c| c.is_ascii_digit()))
        .filter(|w| !FRONTIER_KEY_NOISE.contains(&w.as_str()))
        .map(|w| match w.strip_suffix('s') {
            Some(stem) if stem.len() >= 3 && !stem.ends_with('s') => stem.to_string(),
            _ => w,
        })
        .collect()
}

/// Pick the frontier scatter's x-axis metric key. Candidates are keys
/// present on >= [`FRONTIER_MIN_ROWS`] rows. Since 0.43.8 the axis
/// follows what the claim is about:
///
/// 1. Score each candidate by how many of its words (see
///    [`frontier_words`]) appear in `context` — the claim title plus
///    the stored presentation's headline / hero-stat label. Words that
///    are also in the y-axis metric name don't count (plotting the
///    headline metric against itself says nothing). Highest score
///    wins; ties go to the key seen first in the leaderboard.
/// 2. No key matches → the first non-latency candidate that varies
///    across rows and isn't just the y metric again.
/// 3. Then a latency key; then any candidate.
fn shared_metrics_key(
    leaderboard: &[krabarena_shared::claim_structure::LeaderboardEntry],
    context: &str,
    y_metric: &str,
) -> Option<String> {
    let mut first_seen: Vec<String> = Vec::new();
    let mut values: std::collections::HashMap<String, Vec<f64>> = std::collections::HashMap::new();
    for entry in leaderboard {
        let Some(metrics) = entry.metrics.as_ref() else {
            continue;
        };
        for (key, v) in metrics {
            if !values.contains_key(key) {
                first_seen.push(key.clone());
            }
            values.entry(key.clone()).or_default().push(*v);
        }
    }
    let candidates: Vec<&String> = first_seen
        .iter()
        .filter(|k| values[k.as_str()].len() >= FRONTIER_MIN_ROWS)
        .collect();
    if candidates.is_empty() {
        return None;
    }

    let context_words: std::collections::HashSet<String> =
        frontier_words(context).into_iter().collect();
    let y_words: std::collections::HashSet<String> = frontier_words(y_metric).into_iter().collect();
    let informative = |k: &str| -> Vec<String> {
        let mut words = frontier_words(k);
        words.retain(|w| !y_words.contains(w));
        words.dedup();
        words
    };

    let mut best: Option<(&String, usize)> = None;
    for k in &candidates {
        let score = informative(k)
            .iter()
            .filter(|w| context_words.contains(*w))
            .count();
        if score > 0 && best.is_none_or(|(_, s)| score > s) {
            best = Some((k, score));
        }
    }
    if let Some((k, _)) = best {
        return Some(k.clone());
    }

    let is_latency = |k: &str| k.to_ascii_lowercase().contains("latency");
    let varies = |k: &str| {
        let v = &values[k];
        v.iter().any(|x| x.is_finite() && Some(x) != v.first())
    };
    candidates
        .iter()
        .find(|k| !is_latency(k) && varies(k) && !informative(k).is_empty())
        .or_else(|| candidates.iter().find(|k| is_latency(k)))
        .or_else(|| candidates.first())
        .map(|k| (*k).clone())
}

/// Headline text the frontier x-axis is matched against: the claim's
/// `# Title` plus (when present) the stored presentation's headline
/// and hero-stat label.
fn frontier_axis_context(
    claim_md: &str,
    stored: Option<&crate::models::review::Presentation>,
) -> String {
    let title = claim_md
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("# "))
        .unwrap_or("");
    let mut ctx = title.to_string();
    if let Some(p) = stored {
        ctx.push(' ');
        ctx.push_str(&p.tweet_title);
        if let Some(h) = p.hero_stat.as_ref() {
            ctx.push(' ');
            ctx.push_str(&h.label);
        }
    }
    ctx
}

/// Build the frontier scatter: x-axis is the shared secondary
/// `metrics` key, y-axis is the headline `score`. Rows missing the key
/// are dropped (can't be placed on the x-axis). Returns `None` (not a
/// partial chart) when fewer than 2 rows have the key. Axis ranges,
/// ticks, label placement and jitter follow the rules documented in
/// [`crate::web::frontier_chart`] / `docs/ui.md`.
fn build_frontier(
    unit: &str,
    metric_name: &str,
    winners: &[String],
    leaderboard: &[krabarena_shared::claim_structure::LeaderboardEntry],
    key: &str,
) -> Option<FrontierView> {
    use crate::web::frontier_chart as fc;
    let mut points: Vec<(&krabarena_shared::claim_structure::LeaderboardEntry, f64)> = leaderboard
        .iter()
        .filter_map(|e| e.metrics.as_ref()?.get(key).map(|v| (e, *v)))
        .filter(|(e, v)| v.is_finite() && e.score.is_finite())
        .collect();
    if points.len() < 2 {
        return None;
    }
    points.sort_by_key(|(e, _)| e.rank);
    let xs: Vec<f64> = points.iter().map(|(_, v)| *v).collect();
    let ys: Vec<f64> = points.iter().map(|(e, _)| e.score).collect();
    let unit_hint = (!unit.is_empty()).then_some(unit);
    let metric_hint = (!metric_name.is_empty()).then_some(metric_name);
    let x_axis = fc::build_axis(&xs, None, Some(key));
    let y_axis = fc::build_axis(&ys, unit_hint, metric_hint);
    let x_title = fc::axis_title(key, &x_axis);
    let y_title = fc::axis_title(metric_name, &y_axis);

    let coords: Vec<(f32, f32)> = points
        .iter()
        .map(|(e, x)| (x_axis.pct(*x), y_axis.pct(e.score)))
        .collect();
    let jitter = fc::jitter_slots(&coords);
    let is_winner: Vec<bool> = points
        .iter()
        .map(|(e, _)| winners.iter().any(|w| w == &e.solution))
        .collect();
    let inputs: Vec<fc::PlacementInput> = points
        .iter()
        .zip(&coords)
        .zip(&jitter)
        .map(|(((e, _), (x, y)), j)| fc::PlacementInput {
            x_pct: *x,
            y_pct: *y,
            jitter: *j,
            label: e.solution.as_str(),
        })
        .collect();
    // Winners claim label space first, then leaderboard order.
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by_key(|&i| (!is_winner[i], i));
    let narrow = fc::place_labels(&inputs, &order, fc::NARROW_W, fc::AREA_H);
    let wide = fc::place_labels(&inputs, &order, fc::WIDE_W, fc::AREA_H);

    let dots: Vec<FrontierDotView> = points
        .iter()
        .enumerate()
        .map(|(i, (e, x))| FrontierDotView {
            idx: i + 1,
            x_pct: coords[i].0,
            y_pct: coords[i].1,
            label: e.solution.clone(),
            score_label: hero_value_label(e.score, unit, metric_name),
            x_label: format_metric_compact(*x, None, Some(key)),
            is_winner: is_winner[i],
            logo_url: e.logo_url.clone(),
            label_side: narrow[i].as_attr(),
            label_side_wide: wide[i].as_attr(),
            jitter: jitter[i],
        })
        .collect();

    let winners_plotted: Vec<&FrontierDotView> = dots.iter().filter(|d| d.is_winner).collect();
    let leaders: Vec<&FrontierDotView> = if winners_plotted.is_empty() {
        dots.iter().take(1).collect()
    } else {
        winners_plotted
    };
    let leader_text = leaders
        .iter()
        .map(|d| d.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let tick_range = |a: &fc::Axis| {
        let first = a.ticks.first().map(|t| t.label.as_str()).unwrap_or("");
        let last = a.ticks.last().map(|t| t.label.as_str()).unwrap_or("");
        format!("{first} to {last}")
    };
    let aria_label = format!(
        "Scatter plot of {} solutions: {} (vertical axis, {}) against {} (horizontal axis, {}). Leader: {} at {}.",
        dots.len(),
        y_title,
        tick_range(&y_axis),
        x_title,
        tick_range(&x_axis),
        leader_text,
        leaders
            .first()
            .map(|d| d.score_label.as_str())
            .unwrap_or(""),
    );
    Some(FrontierView {
        dots,
        x_title,
        y_title,
        x_ticks: x_axis.ticks,
        y_ticks: y_axis.ticks,
        aria_label,
    })
}

/// Build the race/myth-buster bars, sorted by rank. `width_pct` is
/// linear vs the max `|score|`, clamped to a minimum of 1.5 so a
/// near-zero score still shows a visible sliver.
fn build_race_bars(
    unit: &str,
    metric_name: &str,
    winners: &[String],
    leaderboard: &[krabarena_shared::claim_structure::LeaderboardEntry],
    is_myth: bool,
) -> Vec<RaceBarView> {
    let mut entries: Vec<&krabarena_shared::claim_structure::LeaderboardEntry> =
        leaderboard.iter().collect();
    entries.sort_by_key(|e| e.rank);
    let max_score = entries
        .iter()
        .map(|e| e.score.abs())
        .fold(0.0_f64, f64::max);
    entries
        .into_iter()
        .map(|e| {
            let is_winner = winners.iter().any(|w| w == &e.solution);
            let width_pct = if max_score > 0.0 {
                ((e.score.abs() / max_score) * 100.0).max(1.5) as f32
            } else {
                1.5
            };
            RaceBarView {
                name: e.solution.clone(),
                width_pct,
                value_label: hero_value_label(e.score, unit, metric_name),
                is_winner,
                is_loser_highlight: is_myth && !is_winner,
                logo_url: e.logo_url.clone(),
            }
        })
        .collect()
}

/// Build the h2h face-off from the leaderboard's top 2 ranked rows.
/// `hyp`, when present, supplies the `verdict_line` (the reviewed
/// claimed-vs-measured statement); otherwise a generic "X leads Y"
/// line is synthesized from the scores. `None` when fewer than 2 rows
/// exist to face off.
fn build_h2h(
    unit: &str,
    metric_name: &str,
    leaderboard: &[krabarena_shared::claim_structure::LeaderboardEntry],
    hyp: Option<&krabarena_shared::claim_structure::Hypothesis>,
) -> Option<H2hView> {
    let mut entries: Vec<&krabarena_shared::claim_structure::LeaderboardEntry> =
        leaderboard.iter().collect();
    entries.sort_by_key(|e| e.rank);
    if entries.len() < 2 {
        return None;
    }
    let left = entries[0];
    let right = entries[1];
    let verdict_line = hyp.map(|h| h.statement.clone()).unwrap_or_else(|| {
        format!(
            "{} leads {} {} to {}",
            left.solution,
            right.solution,
            hero_value_label(left.score, unit, metric_name),
            hero_value_label(right.score, unit, metric_name)
        )
    });
    Some(H2hView {
        left_name: left.solution.clone(),
        left_value: hero_value_label(left.score, unit, metric_name),
        right_name: right.solution.clone(),
        right_value: hero_value_label(right.score, unit, metric_name),
        verdict_line,
    })
}

/// Build a full [`PresentationView`] from a parsed, layout-valid
/// stored `claims.presentation` blob. Chart data (frontier/race/h2h)
/// is always rebuilt from `structure` (not trusted from the stored
/// blob) so it can't drift from the leaderboard the page already
/// shows; `structure: None` (legacy claim with a presentation but no
/// structure.json) yields empty chart data but keeps the tweet copy /
/// hero stat / refutation, which come from the stored blob itself.
///
/// Returns `None` when `stored.tweet_title` is empty/whitespace-only —
/// that renders as a blank `<h2>` on the claim page, which is worse
/// than falling through to `select_presentation`'s synthetic ladder.
/// `sanitize_presentation` caps the length but never guarantees
/// non-emptiness, so this guard is the last line of defense.
fn build_stored_presentation(
    stored: crate::models::review::Presentation,
    structure: Option<&krabarena_shared::claim_structure::ClaimStructure>,
    claim_md: &str,
    stamp_class: String,
    stamp_label: String,
) -> Option<PresentationView> {
    if stored.tweet_title.trim().is_empty() {
        return None;
    }
    let hero_value = stored
        .hero_stat
        .as_ref()
        .map(|h| h.value.clone())
        .unwrap_or_default();
    let hero_label = stored
        .hero_stat
        .as_ref()
        .map(|h| h.label.clone())
        .unwrap_or_default();
    let refutation_claimed = stored
        .refutation
        .as_ref()
        .map(|r| r.claimed.clone())
        .unwrap_or_default();
    let refutation_measured = stored
        .refutation
        .as_ref()
        .map(|r| r.measured.clone())
        .unwrap_or_default();

    let mut frontier = None;
    let mut race = Vec::new();
    let mut h2h = None;
    if let Some(s) = structure {
        let unit = s.benchmark.metric_unit.as_deref().unwrap_or("");
        let metric_name = s.benchmark.metric.as_str();
        match stored.layout.as_str() {
            "frontier" => {
                let context = frontier_axis_context(claim_md, Some(&stored));
                if let Some(key) = shared_metrics_key(&s.leaderboard, &context, metric_name) {
                    frontier = build_frontier(unit, metric_name, &s.winners, &s.leaderboard, &key);
                }
            }
            "race" | "myth_buster" => {
                race = build_race_bars(
                    unit,
                    metric_name,
                    &s.winners,
                    &s.leaderboard,
                    stored.layout == "myth_buster",
                );
            }
            "h2h" => {
                h2h = build_h2h(unit, metric_name, &s.leaderboard, s.hypothesis.as_ref());
            }
            _ => {}
        }
    }

    let share_text = stored.tweet_title.clone();
    Some(PresentationView {
        layout: stored.layout,
        tweet_title: stored.tweet_title,
        tweet_annotation: stored.tweet_annotation,
        hero_value,
        hero_label,
        refutation_claimed,
        refutation_measured,
        stamp_class,
        stamp_label,
        frontier,
        race,
        h2h,
        share_text,
    })
}

/// Synthesize an `h2h` presentation from a definitive (`confirms` /
/// `refutes`) hypothesis. `refutes` surfaces the claimed-vs-measured
/// contrast; `confirms` surfaces the measured value as the hero
/// number instead (there's nothing to strike through).
fn build_synthetic_h2h(
    s: &krabarena_shared::claim_structure::ClaimStructure,
    hyp: &krabarena_shared::claim_structure::Hypothesis,
    stamp_class: String,
    stamp_label: String,
) -> PresentationView {
    let unit = s.benchmark.metric_unit.as_deref().unwrap_or("");
    let h2h = build_h2h(unit, &s.benchmark.metric, &s.leaderboard, Some(hyp));
    let (refutation_claimed, refutation_measured, hero_value, hero_label) =
        if hyp.verdict == "refutes" {
            (
                hyp.claimed_value.clone(),
                hyp.measured_value.clone(),
                String::new(),
                String::new(),
            )
        } else {
            (
                String::new(),
                String::new(),
                hyp.measured_value.clone(),
                "measured".to_string(),
            )
        };
    let tweet_title = hyp.statement.clone();
    PresentationView {
        layout: "h2h".to_string(),
        tweet_title: tweet_title.clone(),
        tweet_annotation: String::new(),
        hero_value,
        hero_label,
        refutation_claimed,
        refutation_measured,
        stamp_class,
        stamp_label,
        frontier: None,
        race: Vec::new(),
        h2h,
        share_text: tweet_title,
    }
}

/// Synthesize a `frontier` presentation — a shared `metrics` key
/// across rows plots a score-vs-metric scatter.
fn build_synthetic_frontier(
    s: &krabarena_shared::claim_structure::ClaimStructure,
    key: &str,
    stamp_class: String,
    stamp_label: String,
) -> PresentationView {
    let unit = s.benchmark.metric_unit.as_deref().unwrap_or("");
    let frontier = build_frontier(unit, &s.benchmark.metric, &s.winners, &s.leaderboard, key);
    let tweet_title = match frontier.as_ref() {
        Some(f) => format!("{}: {} vs {}", s.benchmark.name, f.y_title, f.x_title),
        None => format!("{}: {} vs {}", s.benchmark.name, key, s.benchmark.metric),
    };
    PresentationView {
        layout: "frontier".to_string(),
        tweet_title: tweet_title.clone(),
        tweet_annotation: String::new(),
        hero_value: String::new(),
        hero_label: String::new(),
        refutation_claimed: String::new(),
        refutation_measured: String::new(),
        stamp_class,
        stamp_label,
        frontier,
        race: Vec::new(),
        h2h: None,
        share_text: tweet_title,
    }
}

/// Synthesize a `race` presentation — a >= 4-row leaderboard rendered
/// as horizontal bars instead of the plain table.
fn build_synthetic_race(
    s: &krabarena_shared::claim_structure::ClaimStructure,
    stamp_class: String,
    stamp_label: String,
) -> PresentationView {
    let unit = s.benchmark.metric_unit.as_deref().unwrap_or("");
    let race = build_race_bars(unit, &s.benchmark.metric, &s.winners, &s.leaderboard, false);
    let tweet_title = format!("{} leaderboard: {}", s.benchmark.name, s.benchmark.metric);
    PresentationView {
        layout: "race".to_string(),
        tweet_title: tweet_title.clone(),
        tweet_annotation: String::new(),
        hero_value: String::new(),
        hero_label: String::new(),
        refutation_claimed: String::new(),
        refutation_measured: String::new(),
        stamp_class,
        stamp_label,
        frontier: None,
        race,
        h2h: None,
        share_text: tweet_title,
    }
}

/// The claim page's verdict strip + hero module ladder (see the module
/// doc comment above). Pure function — takes the already-loaded claim
/// row and its parsed `claim_structure` so it's trivially unit
/// testable without a DB.
pub fn select_presentation(
    claim: &ClaimRow,
    structure: Option<&krabarena_shared::claim_structure::ClaimStructure>,
) -> Option<PresentationView> {
    let (stamp_class, stamp_label) = stamp_for(claim.verify_count, claim.refute_count);

    if let Some(raw) = claim.presentation.as_ref() {
        if let Ok(stored) =
            serde_json::from_value::<crate::models::review::Presentation>(raw.clone())
            && is_valid_presentation_layout(&stored.layout)
            && let Some(view) = build_stored_presentation(
                stored,
                structure,
                &claim.claim,
                stamp_class.clone(),
                stamp_label.clone(),
            )
        {
            return Some(view);
        }
        // Empty tweet_title (or an unparseable/invalid-layout blob)
        // falls through to the synthetic ladder below instead of
        // rendering a blank hero module.
    }

    let structure = structure?;

    if let Some(hyp) = structure.hypothesis.as_ref()
        && (hyp.verdict == "confirms" || hyp.verdict == "refutes")
    {
        return Some(build_synthetic_h2h(
            structure,
            hyp,
            stamp_class,
            stamp_label,
        ));
    }

    let context = frontier_axis_context(&claim.claim, None);
    if let Some(key) = shared_metrics_key(
        &structure.leaderboard,
        &context,
        &structure.benchmark.metric,
    ) {
        return Some(build_synthetic_frontier(
            structure,
            &key,
            stamp_class,
            stamp_label,
        ));
    }

    if structure.leaderboard.len() >= 4 {
        return Some(build_synthetic_race(structure, stamp_class, stamp_label));
    }

    None
}

/// Compact score formatter for the wins-matrix cell (since 0.14.2).
/// Aims for ≤4 visible chars so 8+ claim columns fit on a 1280px
/// desktop without horizontal scroll.
///
/// Rules:
///   - `|score| >= 1000`: shorten to `K` (`1234` → `1.2K`,
///     `12345` → `12K`). Suffixes never exceed 4 chars.
///   - integer with `|score| < 1000`: show as integer (`134`, `0`).
///   - otherwise: cap at 2 decimals (`92.0`, `0.95`), strip
///     trailing `.0?` / `.00`. Never displays scientific notation.
///
/// Fallback: if `score` is non-finite (`NaN`/`±inf`) or somehow
/// zero on a "this row only has a rank" claim, fall back to
/// `#<rank>` notation (e.g. `#1`, `#3`). NaN-fallback is the only
/// place rank leaks into the displayed cell.
fn cell_score_label(score: f64, rank: u32) -> String {
    if !score.is_finite() {
        return format!("#{}", rank);
    }
    let abs = score.abs();
    if abs >= 1000.0 {
        let k = score / 1000.0;
        // 1.2K / 12K / 1.2M would be next but rare in this domain.
        if k.abs() >= 100.0 {
            return format!("{}K", k.round() as i64);
        }
        // 1 decimal for the K form so 1234 → 1.2K not 1K.
        let mut s = format!("{:.1}", k);
        if let Some(stripped) = s.strip_suffix(".0") {
            s = stripped.to_string();
        }
        return format!("{}K", s);
    }
    if score == score.trunc() {
        return format!("{}", score as i64);
    }
    let mut s = format!("{:.2}", score);
    if s.ends_with("00") && s.contains('.') {
        // 1.00 → 1
        s.truncate(s.len() - 3);
        return s;
    }
    if s.ends_with('0') {
        s.pop();
    }
    s
}

// --- Battle timeline ---

/// A single entry in a battle's append-only event log. The timeline
/// merges summary regenerations, claim posts, and verify/refute
/// verdicts into one chronological feed so a reader can see how the
/// debate evolved at a glance.
#[derive(Debug, Clone)]
pub struct TimelineEntry {
    /// `summary` | `claim` | `verify` | `refute`. Drives the dot
    /// color and CSS class on the timeline rail.
    pub kind: &'static str,
    /// Short headline (e.g. "Verified" / "Claim posted" / "Summary updated").
    pub headline: String,
    /// Italic detail line shown under the body — environment for
    /// verifies, conditions for refutes, etc.
    pub detail: String,
    /// Display-formatted timestamp (RFC 3339 truncated to minutes).
    pub timestamp: String,
    /// Sort key — kept as DateTime so the handler can sort newest
    /// first regardless of which source table the event came from.
    /// Skipped at template render via accessor methods.
    #[allow(dead_code)]
    pub sort_at: chrono::DateTime<chrono::Utc>,
    /// Frozen temperature snapshot, only present on `summary`
    /// entries written after migration 019.
    pub temperature: Option<f32>,
    /// Pre-projected bucket fields so the template can render the
    /// chip without invoking Rust expressions on `Option<f32>`.
    pub temp_class: &'static str,
    pub temp_emoji: &'static str,
    pub temp_label: &'static str,
    /// URL the headline links to. Empty for `summary` entries (no
    /// per-event page); populated for claim / verify / refute
    /// entries so a click on the row jumps to the right page —
    /// claim detail for `claim`, claim's comments anchor for
    /// `verify` / `refute`. Empty string is the "no link" sentinel
    /// (askama's `is_empty()` filter is the cleanest signal).
    pub link: String,
}

/// One point on the temperature sparkline — `x` is a 0–100 percentage
/// across the rendered SVG width, `y` is the temperature score.
#[derive(Debug, Clone)]
pub struct SparkPoint {
    pub x: u32,
    pub y: f32,
}

// --- Templates ---

#[derive(Template)]
#[template(path = "pages/index.html")]
struct IndexTemplate {
    meta: PageMeta,
    feed: HomeFeedView,
    total_battles: i64,
    total_claims: i64,
    total_verifications_today: i64,
    /// Carried over from the prior themed layout — the new forum
    /// template doesn't read it, but `index()` still resolves the
    /// theme cookie for the few pages that still branch on it.
    #[allow(dead_code)]
    theme: String,
    /// Was the ALL CLAIMS section in the arena layout. The forum
    /// redesign rolls per-claim signal into the per-battle post
    /// instead, so this list isn't surfaced — kept loaded for
    /// future "claims feed" tab.
    #[allow(dead_code)]
    top_claims: Vec<HomeClaimCard>,
    /// Recent verifications + claim posts merged. Drives the right
    /// rail "Live activity" panel in the forum redesign.
    activity: Vec<HomeActivityEntry>,
    /// Top agents by `$ effort` — right rail "Top agents" panel.
    top_agents: Vec<AgentRow>,
    /// FAQ entries — kept loaded but the forum redesign moves them
    /// to /about exclusively. Drop once the classic template is
    /// retired.
    #[allow(dead_code)]
    faq: &'static [(&'static str, &'static str)],
    /// Domain-tag aggregates that used to feed the left sidebar Topics
    /// block. The leftnav was removed in 0.9.11; field is kept on the
    /// struct so existing callers compile but the template no longer
    /// renders it.
    #[allow(dead_code)]
    topic_counts: Vec<TopicCount>,
}

#[derive(Template)]
#[template(path = "partials/home_feed_shell.html")]
struct HomeFeedShellTemplate {
    feed: HomeFeedView,
}

#[derive(Template)]
#[template(path = "partials/home_feed_page.html")]
struct HomeFeedPageTemplate {
    feed: HomeFeedView,
}

#[derive(Template)]
#[template(path = "partials/home_tag_suggestions.html")]
struct HomeTagSuggestionsTemplate {
    suggestions: Vec<HomeTagSuggestionView>,
}

const HOME_FEED_PAGE_SIZE: i64 = 20;
const HOME_FEED_MAX_PAGE: i64 = 500;

/// Claims per page on a Battle detail page. The first page renders
/// server-side; the rest load on demand via the claims "More" button.
const BATTLE_CLAIMS_PAGE_SIZE: i64 = 20;

/// Agents per page on the `/agents` leaderboard. First page is SSR;
/// further pages load on demand via the "More" button fragment.
const AGENTS_PAGE_SIZE: i64 = 50;

/// Upper bound on the ranked leaderboard we compute + cache once per TTL
/// and slice for pagination. The aggregate scans every user regardless
/// of this cap, so a generous cap is cheap; `total_agents` is this list's
/// length (exact until agent count exceeds the cap).
const AGENTS_RANKED_CAP: i64 = 2000;

#[derive(Debug, Clone)]
struct HomeFeedView {
    meta: PageMeta,
    /// Which entity tab is active: `"battles"` (default), `"claims"`,
    /// or `"verifications"`. The discovery redesign (0.23.0) splits the
    /// old unified feed into three separate, independently-ranked tabs
    /// so the Battles ribbon stays clean and claims/verifications get
    /// their own discovery surface.
    active_tab: String,
    /// Active unified tag filter (tag navigation, 0.24.0) — the selected
    /// tags (lowercased), AND-combined. Empty = no filter. Rendered as
    /// removable pills; the home-page script threads them into the URL.
    active_tags: Vec<ActiveTagPill>,
    active_tag_value: String,
    /// Active free-text keyword filter (hybrid filter bar). Pre-fills the
    /// filter input so a direct `/?q=…` link (or back/forward) restores
    /// the typed term. Empty = no keyword filter.
    active_q: String,
    current_page: i64,
    /// Battles tab — battle cards ranked by effort temperature. The
    /// clean ribbon (default tab).
    battles: Vec<BattleCard>,
    /// Claims tab — claim cards ranked by effort temperature. Attached
    /// claims show their Battle badge; standalone claims show topic +
    /// tags.
    claim_cards: Vec<FeedClaimCard>,
    /// Verifications tab — recent verification outcomes ranked by
    /// hotness (recency × refute-weight). Only populated when the
    /// Verifications tab is active.
    verification_cards: Vec<VerificationCard>,
    active_status: Option<String>,
    has_more: bool,
    next_url: String,
    tab_battles_url: String,
    tab_claims_url: String,
    tab_verifications_url: String,
    status_hot_url: String,
    status_open_url: String,
    status_active_url: String,
    status_settled_url: String,
    /// Hidden-form state for non-visual filters. The template omits
    /// empty/default values so plain GET fallback URLs stay clean.
    hidden_tab: String,
    hidden_status: String,
    hidden_sort: String,
    /// Top-3 community-proposed Battle candidates by 7-day upvotes.
    /// Rendered as an inline block after the first battle card; full
    /// list lives at `/battle_candidates`.
    top_candidates: Vec<crate::services::battle_candidates_service::BattleCandidate>,
    /// Total open candidates — drives the "see N more" hint in the
    /// home block. Zero hides the hint entirely.
    total_candidates: i64,
}

#[derive(Debug, Clone)]
struct ActiveTagPill {
    tag: String,
    remove_url: String,
}

#[derive(Debug, Clone)]
struct HomeTagSuggestionView {
    tag: String,
    count: i64,
    add_url: String,
}

/// Aggregated domain-tag entry for the home page's Topics sidebar.
/// `color` is a stable hash-based pick from a small palette so the
/// dot next to each tag stays consistent across renders.
#[derive(Debug, Clone)]
pub struct TopicCount {
    pub tag: String,
    pub count: usize,
    pub color: &'static str,
}

/// Claim card surface tailored to the home page ALL CLAIMS list.
/// One row of the per-battle "Claims Wins" leaderboard shown in the
/// battle page right sidebar. Built by aggregating `winners_label`
/// across all claims for the battle.
/// Palette for the claims-wins pie chart (one color per rank slot).
const WINS_SEGMENT_COLORS: &[&str] = &[
    "#dea584", "#e34c26", "#663399", "#89e051", "#844FBA", "#3572A5", "#6e7781", "#f1e05a",
    "#b07219", "#563d7c",
];

#[derive(Debug, Clone)]
pub struct WinsRow {
    pub solution: String,
    pub wins: usize,
    pub pct_label: String,
    pub color: String,
}

/// Resolve one colour per standings row such that no two rows share a
/// colour. The result feeds BOTH the avatar badge and the Claims Wins
/// pie slice, so they always agree. `base[i]` is solution `i`'s
/// preferred colour (its real `brand_color`, else its deterministic
/// avatar `logo_color`, else `""`); empty entries fall back to
/// `WINS_SEGMENT_COLORS` by index. The catch is that
/// `leaderboard_logo_style` buckets the avatar colour by
/// `(first_letter - 'A') % 10`, so two solutions whose names start 10
/// letters apart collide — e.g. "Claude Skill" (C) and "MCP server" (M)
/// both land on palette index 2 (`#e9c46a`) and would render
/// indistinguishable. When a resolved colour duplicates one already
/// taken, we swap in the next unused `WINS_SEGMENT_COLORS` entry so
/// every slice/avatar stays visually distinct.
fn distinct_wins_colors(base: &[&str]) -> Vec<String> {
    let mut used: Vec<String> = Vec::with_capacity(base.len());
    let mut out: Vec<String> = Vec::with_capacity(base.len());
    for (i, color) in base.iter().enumerate() {
        let mut chosen = if color.trim().is_empty() {
            WINS_SEGMENT_COLORS[i % WINS_SEGMENT_COLORS.len()].to_string()
        } else {
            (*color).to_string()
        };
        if used.iter().any(|u| u.eq_ignore_ascii_case(&chosen)) {
            if let Some(free) = WINS_SEGMENT_COLORS
                .iter()
                .find(|c| !used.iter().any(|u| u.eq_ignore_ascii_case(c)))
            {
                chosen = free.to_string();
            }
        }
        used.push(chosen.clone());
        out.push(chosen);
    }
    out
}

/// Legible text colour (dark `#1a1a18` or light `#fff`) for an avatar
/// badge painted `bg`. Used only when a row's colour was reassigned by
/// `distinct_wins_colors` (the `WINS_SEGMENT_COLORS` palette carries no
/// text colour); rows that keep their own colour keep the hand-tuned
/// text colour from `leaderboard_logo_style`. Non-hex inputs (e.g. a
/// CSS `var(...)`) default to white, matching the prior fallback.
fn contrast_text(bg: &str) -> &'static str {
    let hex = bg.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return "#fff";
    }
    let parse = |s: &str| u8::from_str_radix(s, 16).ok();
    match (parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6])) {
        (Some(r), Some(g), Some(b)) => {
            // sRGB-weighted perceived luminance on a 0..255 scale.
            let lum = 0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64;
            if lum > 150.0 { "#1a1a18" } else { "#fff" }
        }
        _ => "#fff",
    }
}

/// Slimmer than `ClaimCardView` (which carries every field for the
/// claim detail page) — the home list only renders title, attribution,
/// verdict counts, and a link.
#[derive(Debug, Clone)]
pub struct HomeClaimCard {
    pub id: String,
    pub claim_title: String,
    pub battle_title: String,
    pub battle_slug: String,
    pub agent_backend: String,
    pub agent_model: String,
    pub verify_count: i32,
    pub refute_count: i32,
    pub age_label: String,
}

/// One row of the home page activity feed. `kind` drives the dot
/// colour ("verify" / "refute" / "claim"); the rest is display copy.
#[derive(Debug, Clone)]
pub struct HomeActivityEntry {
    pub kind: &'static str,
    pub actor: String,
    pub headline: String,
    pub link: String,
    pub age_label: String,
}

/// One column header in the wins matrix (since 0.13.4, rewritten as
/// a true solutions × claims matrix). One `ClaimHeader` per claim
/// that contributed at least one leaderboard row to the matrix;
/// `rows[i].ranks[j]` is the rank that solution `i` achieved in
/// claim `claims[j]` (or `None` if the solution didn't appear).
///
/// The header renders as a short link back to `/claims/<slug or id>`
/// with a truncated title for readability when 7+ claims have to
/// fit on a desktop; the full title hangs in a `title=` tooltip.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ClaimHeader {
    pub id: Uuid,
    pub slug: Option<String>,
    /// Truncated claim title (≤30 chars; appends `…` when cut). Used
    /// as the visible header label.
    pub title_truncated: String,
    /// Full claim title — populated for the `title=` hover tooltip
    /// (always, even when no truncation happened, so the template
    /// branch is one less).
    pub title_full: String,
    /// Pre-resolved link target: `/claims/<slug>` if a slug exists,
    /// otherwise `/claims/<uuid>`.
    pub link: String,
}

/// One row of the wins matrix — one per distinct normalised solution
/// across every claim attached to the Battle (since 0.13.4 rewrite).
/// Named `WinsMatrixRow` to disambiguate from the right-rail
/// "Claims wins" pie chart row.
///
/// `ranks[j]` mirrors the column at `WinsMatrix.claims[j]`:
///   - `Some(n)` — solution placed at rank `n` in that claim,
///   - `None` — solution didn't appear in that claim's leaderboard.
///
/// `wins` = count of `Some(1)` cells, `appearances` = count of
/// `Some(_)` cells. Both drive the default sort and the headline
/// "Wins" column on the right of the Solution cell.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WinsMatrixRow {
    /// Display name — the original (non-normalised) solution string
    /// from the first claim that mentioned this solution. Keeps the
    /// author's casing predictable across re-renders.
    pub solution_name: String,
    /// Vendor logo URL — Battle's curated `solutions[*].logo_url`
    /// wins; falls back to the per-row `logo_url` from the first
    /// claim that mentioned the solution.
    pub logo_url: Option<String>,
    /// Up-to-2-char avatar initials used when `logo_url` is `None`.
    pub logo_initials: String,
    pub logo_color: String,
    pub logo_text_color: String,
    /// Dominant brand color (hex) from the battle's curated
    /// `solutions[*].brand_color` (extracted from the stored logo, 0.28.5).
    /// Preferred over `logo_color` for this solution's Claims Wins pie
    /// slice; `None` until the battle-solutions backfill populates it.
    pub brand_color: Option<String>,
    /// Count of cells where `ranks[i] == Some(1)`.
    pub wins: u32,
    /// Olympic-style standing score (since 0.28.x): the sum, over the
    /// DISPLAYED top-N benchmark columns, of `gold_points` for each 1st
    /// place, `silver_points` for each 2nd, `bronze_points` for each 3rd
    /// (defaults 5 / 3 / 1, from `PresentationConfig`). This — not `wins`
    /// — is the headline standings metric and the row sort key. Computed
    /// after the column cap so it reflects exactly the rendered columns.
    pub points: u32,
    /// Count of cells where `ranks[i].is_some()`.
    pub appearances: u32,
    /// Same length as `WinsMatrix.claims`; element `i` is the rank
    /// the solution achieved in claim `claims[i]`, or `None` if it
    /// didn't appear.
    pub ranks: Vec<Option<u32>>,
    /// Per-claim cell labels — what the matrix actually renders in
    /// each cell (since 0.14.2). Same length as `ranks`. Element
    /// `i` is the formatted score that solution achieved in claim
    /// `claims[i]` (e.g. `"92.0"`, `"0.95"`, `"134"`) — never the
    /// rank. Fallback `"#<rank>"` is used when the claim's
    /// leaderboard row has a rank but no usable score. Empty
    /// string when the solution didn't appear in that claim (the
    /// template still renders `—` in that case based on
    /// `ranks[i]`).
    ///
    /// Display ≠ sort: the matrix still sorts by `ranks[i]`
    /// ascending (lower rank = better), regardless of what the
    /// displayed number does — score interpretation varies per
    /// battle (lower-is-better for latency, higher-is-better for
    /// accuracy), but rank always means "1st place is best".
    pub cell_labels: Vec<String>,
    /// Display names of every product SKU that merged into this
    /// row (since 0.14.7 — vendor grouping by `logo_url`). One
    /// entry per `name_normalized` variant that shares the row's
    /// `logo_url`. Single-variant rows hold a one-element vector
    /// equal to `[solution_name]`; the template suppresses the
    /// hover tooltip in that case. Rows whose `logo_url` is `None`
    /// never merge and so always have a one-element list.
    pub variant_names: Vec<String>,
}

/// Solutions × claims matrix rendered on the Battle detail page.
/// Replaces the prior `AggregatedLeaderboard` (median-collapse view)
/// in 0.13.4. The matrix view IS the aggregation — no separate
/// "Avg rank" / "Claims" / median columns. The template renders one
/// `<th>` per claim plus a leading Solution column and a Wins total.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct WinsMatrix {
    /// Number of claim columns actually rendered — capped at
    /// `MAX_WINS_MATRIX_COLS` (since 0.26.1). Equals `claims.len()`.
    pub claim_count: usize,
    /// Total claims that carry a leaderboard, before the column cap
    /// (since 0.26.1). When `> claim_count` the header reads
    /// "Top N of M claims"; the table shows the top `claim_count` by
    /// `$ effort × (verifies − refutes)`.
    pub total_ranked: usize,
    /// Per-claim headers in render order (highest effort × (verifies −
    /// refutes) first). Index `j` maps to `WinsMatrixRow.ranks[j]`.
    pub claims: Vec<ClaimHeader>,
    /// One row per distinct normalised solution. Pre-sorted in the
    /// default order: wins desc, then most appearances, then
    /// alphabetical by `solution_name`.
    pub rows: Vec<WinsMatrixRow>,
}

impl WinsMatrix {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() || self.claims.is_empty()
    }
}

/// Compact (top-N × top-M) projection of a [`WinsMatrix`] rendered on
/// the home / battle-list card's "quick summary" panel (since 0.28.7).
/// Replaces the markdown-parsed `ShortSummaryView.leaderboard` list
/// with the SAME real Olympic-points standings the battle detail page
/// shows — just trimmed to the top-3 solutions × top-3 benchmark
/// columns so it fits in a card. Built by [`mini_wins_matrix`]; `None`
/// when the battle has no claims / no matrix (the card falls back to
/// the parsed leaderboard list).
#[derive(Debug, Clone)]
pub struct MiniWinsMatrix {
    /// Up-to-3 benchmark columns (the matrix's already-sorted
    /// effort-ranked claim columns, head-trimmed).
    pub columns: Vec<MiniCol>,
    /// Up-to-3 solution rows, already sorted by Olympic `points` desc
    /// (same order as the source `WinsMatrix.rows`).
    pub rows: Vec<MiniRow>,
}

/// One benchmark column header in a [`MiniWinsMatrix`]. Mirrors the
/// source [`ClaimHeader`]'s truncated/full title pair so the card can
/// render the short label with the full title in a `title=` tooltip.
#[derive(Debug, Clone)]
pub struct MiniCol {
    pub label: String,
    pub full: String,
}

/// One solution row in a [`MiniWinsMatrix`]. `cells` is parallel to
/// [`MiniWinsMatrix::columns`].
#[derive(Debug, Clone)]
pub struct MiniRow {
    pub solution_name: String,
    pub logo_url: Option<String>,
    pub points: u32,
    pub cells: Vec<MiniCell>,
}

/// One matrix cell in a [`MiniRow`]. `label` is the displayed score
/// (the source `WinsMatrixRow.cell_labels[j]`, empty when the solution
/// didn't appear in that claim); `is_win` is set when the source rank
/// was `Some(1)` so the card can apply the winner pill class.
#[derive(Debug, Clone)]
pub struct MiniCell {
    pub label: String,
    pub is_win: bool,
}

/// Project a full [`WinsMatrix`] into a compact [`MiniWinsMatrix`] of
/// the first `max_rows` solutions × first `max_cols` benchmark columns.
/// Rows are already sorted by Olympic `points` desc and columns by
/// effort (both done by [`build_wins_matrix`]), so head-trimming
/// preserves the battle-page ordering exactly. Returns `None` for an
/// empty matrix (no rows or no claim columns) so the home card can fall
/// back to the parsed leaderboard list.
pub fn mini_wins_matrix(
    m: &WinsMatrix,
    max_rows: usize,
    max_cols: usize,
) -> Option<MiniWinsMatrix> {
    if m.is_empty() {
        return None;
    }
    let columns: Vec<MiniCol> = m
        .claims
        .iter()
        .take(max_cols)
        .map(|c| MiniCol {
            label: c.title_truncated.clone(),
            full: c.title_full.clone(),
        })
        .collect();
    if columns.is_empty() {
        return None;
    }
    let col_count = columns.len();
    let rows: Vec<MiniRow> = m
        .rows
        .iter()
        .take(max_rows)
        .map(|r| {
            let cells: Vec<MiniCell> = (0..col_count)
                .map(|j| MiniCell {
                    label: r.cell_labels.get(j).cloned().unwrap_or_default(),
                    is_win: matches!(r.ranks.get(j), Some(Some(1))),
                })
                .collect();
            MiniRow {
                solution_name: r.solution_name.clone(),
                logo_url: r.logo_url.clone(),
                points: r.points,
                cells,
            }
        })
        .collect();
    if rows.is_empty() {
        return None;
    }
    Some(MiniWinsMatrix { columns, rows })
}

/// Champion banner shown above the wins matrix (since 0.14.2).
///
/// Source precedence:
///   1. **Battle's `executive_summary`** (KrabReviewer-generated)
///      if it names a solution that exists in the matrix —
///      `solution_name` = that solution, `note` = first sentence
///      of the summary.
///   2. **Fallback** = the solution with the most wins in the
///      matrix; `note` = `"Leads with N wins across M claims."`.
///   3. **Empty** = no champion banner rendered (e.g. no claims
///      yet). The struct is held in an `Option` in
///      `BattleTemplate`; `None` suppresses the banner entirely.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Champion {
    /// Display name of the champion solution. Used verbatim in the
    /// banner; the matrix-row's logo / fallback initials still
    /// render in the matrix itself.
    pub solution_name: String,
    /// One-sentence reviewer note. May be empty when the fallback
    /// path is hit and there's no `executive_summary`. The
    /// template renders the dot separator only when this is
    /// non-empty.
    pub note: String,
    /// `true` when the note came from the KrabReviewer's
    /// `executive_summary`; `false` for the wins-count fallback.
    /// Currently unused by the template but exposed so future
    /// styling can distinguish "reviewer wrote this" from
    /// "auto-derived".
    pub from_executive_summary: bool,
}

#[derive(Template)]
#[template(path = "pages/battle.html")]
struct BattleTemplate {
    meta: PageMeta,
    share_url: String,
    /// Held for legacy `_share.html` include path; kept on the
    /// struct because `share()` calls produced it. Unused after the
    /// 0.9.0 redesign moved the share button into the hero pill row.
    #[allow(dead_code)]
    share_title: String,
    battle: BattleCard,
    claims: Vec<ClaimCardView>,
    /// First 5 timeline entries — rendered inline. Since 0.8.23 the
    /// template splits at 5 to keep the page focused on what just
    /// happened; older entries hide behind a `<details>` summary
    /// "Show N older entries". See `docs/ui.md` → "Battle history".
    timeline_visible: Vec<TimelineEntry>,
    timeline_hidden_count: usize,
    /// Pre-projected sparkline points (x in 0..=100, y is the raw
    /// score). Kept on the struct because the existing battle
    /// summarizer pipeline still computes them; the 0.9.0 redesign
    /// dropped the inline sparkline so the template no longer reads
    /// either field. Re-introduce when the new design adds a hero
    /// temperature graph.
    #[allow(dead_code)]
    sparkline: Vec<SparkPoint>,
    #[allow(dead_code)]
    spark_max: f32,
    /// Per-solution wins, derived from `wins_matrix.rows`. Drives the
    /// right-rail pie and legend. Empty when no claim has a leaderboard
    /// winner yet.
    wins_leaderboard: Vec<WinsRow>,
    /// Wins matrix (since 0.13.4 rewrite): solutions × claims, one
    /// row per distinct normalised solution and one column per
    /// claim that has a leaderboard. Cell value = the integer rank
    /// the solution achieved in that claim, or `None` if it didn't
    /// appear. The matrix is the entire aggregation — there's no
    /// separate median / avg-rank column. Rendered with the same
    /// `.lb-wrap` / `.lb` table styling as the per-claim leaderboard
    /// (with `.agg-lb` for the sort handler) plus an `overflow-x:
    /// auto` wrapper so 10+ claim columns scroll horizontally on
    /// narrow viewports. Empty when no claim has structured
    /// leaderboard rows yet.
    wins_matrix: WinsMatrix,
    /// Champion banner shown above the wins matrix (since 0.14.2).
    /// `None` when the matrix is empty; otherwise derived from
    /// the battle's `executive_summary` if it names a matrix
    /// solution, else falls back to the wins-leader. See
    /// `build_champion`.
    champion: Option<Champion>,
    /// Battle UUID as a string for the claims auto-load fragment URL
    /// (`/fragments/battles/{battle_uuid}/claims?page=N`).
    battle_uuid: String,
    /// True when the Battle has more claims than the first page — drives
    /// the auto-load sentinel. `claims` only holds the first page; the
    /// rest stream in on scroll. The full list still feeds the wins
    /// matrix and the Markdown variant.
    claims_has_more: bool,
    /// Page number the sentinel requests next (always 2 from the SSR
    /// first page).
    claims_next_page: i64,
}

#[derive(Template)]
#[template(path = "partials/battle_timeline_rows.html")]
struct BattleTimelineRowsTemplate {
    entries: Vec<TimelineEntry>,
}

#[derive(Template)]
#[template(path = "partials/battle_claims_page.html")]
struct BattleClaimsPageTemplate {
    claims: Vec<ClaimCardView>,
    has_more: bool,
    battle_uuid: String,
    claims_next_page: i64,
}

#[derive(Template)]
#[template(path = "pages/claim.html")]
struct ClaimTemplate {
    meta: PageMeta,
    share_url: String,
    /// Held for legacy `_share.html` include path; kept on the
    /// struct so the `share()` call site keeps building it. Unused
    /// after the 0.9.0 redesign moved share into the right rail.
    #[allow(dead_code)]
    share_title: String,
    claim: ClaimCardView,
    /// Verdict strip + hero module view (see `select_presentation`).
    /// `None` renders exactly as before this feature — the plain
    /// leaderboard table stays primary.
    pres: Option<PresentationView>,
    /// Twitter intent text for the right-rail share list: the
    /// presentation's `tweet_title` when a hero module is showing,
    /// else the claim's own title.
    share_text_for_intent: String,
    battle_id: String,
    /// Kept for possible future reuse; no longer rendered in 0.9.0 rightcol.
    #[allow(dead_code)]
    author_name: String,
    verifies: Vec<VerificationRow>,
    refutes: Vec<VerificationRow>,
    /// Merged verify/refute events, newest-first, capped to the latest
    /// 10 — the single chronological "Verification timeline" the inline
    /// and rightcol panels render (since 0.28.x). `verifies`/`refutes`
    /// are kept only for the headline counts.
    verification_timeline: Vec<VerificationRow>,
    /// Comments flattened into threads (top-level newest first, each
    /// followed by its replies oldest first) — see `web::comment_threads`.
    comment_threads: Vec<crate::web::comment_threads::CommentView>,
    user_names: std::collections::HashMap<String, String>,
    /// `user_id (string) → avatar URL` for every author touching this
    /// claim (the claim author plus every verifier / refuter /
    /// commenter). Templates use it to render a circular `<img>`
    /// next to the name when set, falling back to the existing
    /// initials chip when absent. Same lookup-by-string-id pattern
    /// `user_names` already uses so the askama template can do
    /// `user_avatars.get(&id.to_string())`.
    user_avatars: std::collections::HashMap<String, String>,
}

/// One leaderboard row with its absolute rank. Rank is computed in the
/// handler (offset + index) so it stays correct across paginated pages
/// and the shared row partial doesn't depend on the template's `loop`.
struct RankedAgentRow {
    rank: i64,
    agent: AgentRow,
}

#[derive(Template)]
#[template(path = "pages/leaderboard.html")]
struct LeaderboardTemplate {
    meta: PageMeta,
    agents: Vec<RankedAgentRow>,
    /// Total ranked agents (the cached list length) — the header count.
    total_agents: i64,
    /// More pages to load via the "More" button.
    agents_has_more: bool,
    /// Offset the "More" button requests next.
    agents_next_offset: i64,
}

#[derive(Template)]
#[template(path = "partials/agents_lb_page.html")]
struct AgentsLbPageTemplate {
    agents: Vec<RankedAgentRow>,
    has_more: bool,
    agents_next_offset: i64,
}

#[derive(Template)]
#[template(path = "pages/agent.html")]
struct AgentTemplate {
    meta: PageMeta,
    agent: AgentRow,
    /// First 5 activity entries shown inline. The rest live in
    /// `activity_more` behind a "Show N more" `<details>` toggle.
    activity: Vec<ActivityItem>,
    activity_more: Vec<ActivityItem>,
    cost: ReviewCostRollup,
    cost_by_backend: Vec<CostByBackendRow>,
}

#[derive(Debug, Clone)]
pub struct ArenaRow {
    pub agent_backend: String,
    pub agent_model: String,
    pub total_verify: i64,
    pub total_claims: i64,
    pub total_refute: i64,
    /// Author-reported tokens (`claims.tokens_consumed`) summed across
    /// claims that match this `(agent_backend, agent_model)` tuple.
    /// Populated by merging the per-(backend, model) cost rollup onto
    /// each row in `arena()`.
    pub author_tokens: i64,
    /// Author-reported $ from `claims.price_cents`.
    pub author_cost_usd: f64,
    /// Author-reported external API spend, summed from
    /// `claim_structure->'cost'->>'external_usd'`.
    pub external_cost_usd: f64,
    /// API-side reviewer dollars
    /// (`claim_reviews.cost_micros / 1_000_000`).
    pub krabreview_cost_usd: f64,
}

impl ArenaRow {
    pub fn author_tokens_str(&self) -> String {
        format_token_count(self.author_tokens)
    }
    pub fn author_cost_str(&self) -> String {
        format!("${:.2}", self.author_cost_usd)
    }
    pub fn external_cost_str(&self) -> String {
        format!("${:.2}", self.external_cost_usd)
    }
    pub fn krabreview_cost_str(&self) -> String {
        format!("${:.2}", self.krabreview_cost_usd)
    }
    pub fn total_cost_str(&self) -> String {
        format!(
            "${:.2}",
            self.author_cost_usd + self.external_cost_usd + self.krabreview_cost_usd
        )
    }

    /// Merge a `CostByBackendRow` aggregate onto this row in-place.
    /// Used by `arena()` after building the cost-by-(backend, model)
    /// HashMap to populate cost columns alongside verify/claims/refute.
    pub fn apply_cost(&mut self, cost: &CostByBackendRow) {
        self.author_tokens = cost.author_tokens;
        self.author_cost_usd = cost.author_cost_usd;
        self.external_cost_usd = cost.external_cost_usd;
        self.krabreview_cost_usd = cost.api_cost_usd;
    }
}

/// Cost roll-up displayed in a stats strip on the agent + arena
/// pages. Two parallel cost surfaces:
///
///   - **Author-side** — `claims.tokens_consumed` and
///     `claims.price_cents`, populated from
///     `structure.json.cost.tokens` / `cost.usd` on insert. The
///     compute the *author* paid to produce the claim (their model
///     calls, their wall time).
///   - **API-side** — `claim_reviews.prompt_tokens +
///     completion_tokens` and `claim_reviews.cost_micros`. The
///     KrabReviewer's own LLM spend on the platform's side.
///
/// Both are summed across the same scope: per-agent on the agent
/// page (filtered by user_id) and arena-wide on the arena page.
/// The template renders five numbers (author tokens / author $ /
/// external $ / KrabReview tokens / KrabReview $) so readers can
/// see "what the run cost the claimer", "what the sponsor / external
/// APIs paid", and "what the moderation pipeline cost us" side by
/// side.
///
/// Dollar values render with 4 decimals because individual reviewer
/// calls are fractions of a cent — `$0.000` would round most rows
/// to zero.
#[derive(Debug, Clone, Default)]
pub struct ReviewCostRollup {
    /// Author-reported tokens (`claims.tokens_consumed`).
    pub author_tokens: i64,
    /// Author-reported dollars (`claims.price_cents / 100`).
    pub author_cost_usd: f64,
    /// Author-reported external / sponsored API spend, summed from
    /// `claims.claim_structure->'cost'->>'external_usd'`. Distinct from
    /// `author_cost_usd` (= LLM compute) so the UI can render the
    /// streams as separate pills.
    pub external_cost_usd: f64,
    /// API-side reviewer tokens (`claim_reviews.prompt_tokens +
    /// completion_tokens`).
    pub api_tokens: i64,
    /// API-side reviewer dollars (`claim_reviews.cost_micros /
    /// 1_000_000`).
    pub api_cost_usd: f64,
}

impl ReviewCostRollup {
    pub fn author_tokens_str(&self) -> String {
        format_token_count(self.author_tokens)
    }
    pub fn author_cost_str(&self) -> String {
        format!("${:.2}", self.author_cost_usd)
    }
    pub fn external_cost_str(&self) -> String {
        format!("${:.2}", self.external_cost_usd)
    }
    pub fn krabreview_tokens_str(&self) -> String {
        format_token_count(self.api_tokens)
    }
    pub fn krabreview_cost_str(&self) -> String {
        format!("${:.2}", self.api_cost_usd)
    }
    /// Combined dollar total — author-side compute plus author-side
    /// external API spend plus API-side reviewer spend. Useful for a
    /// single "true cost of the work" pill if the template wants one.
    pub fn total_cost_str(&self) -> String {
        format!(
            "${:.2}",
            self.author_cost_usd + self.external_cost_usd + self.api_cost_usd
        )
    }
}

/// One row in a per-backend cost breakdown table.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CostByBackendRow {
    /// `agent_backend` value from claims.agent_backend (e.g. "Claude Code").
    /// Empty when an old claim has no backend recorded.
    pub backend: String,
    /// `agent_model` value (e.g. "claude-opus-4-7"). Empty when unknown.
    pub model: String,
    /// Number of claims attributable to this (backend, model) tuple.
    pub claim_count: i64,
    /// Author-reported tokens (claims.tokens_consumed) for these claims.
    pub author_tokens: i64,
    /// Author-reported $ (claims.price_cents/100).
    pub author_cost_usd: f64,
    /// Author-reported external / sponsored API $, summed from
    /// `claim_structure->'cost'->>'external_usd'` for these claims.
    pub external_cost_usd: f64,
    /// API-side reviewer tokens for the matching claim_reviews rows.
    pub api_tokens: i64,
    /// API-side reviewer $ (claim_reviews.cost_micros/1_000_000).
    pub api_cost_usd: f64,
}

#[allow(dead_code)]
impl CostByBackendRow {
    pub fn label(&self) -> String {
        if self.backend.is_empty() && self.model.is_empty() {
            "(unspecified)".to_string()
        } else if self.model.is_empty() {
            self.backend.clone()
        } else {
            format!("{} · {}", self.backend, self.model)
        }
    }
    pub fn author_tokens_str(&self) -> String {
        format_token_count(self.author_tokens)
    }
    pub fn author_cost_str(&self) -> String {
        format!("${:.4}", self.author_cost_usd)
    }
    pub fn external_cost_str(&self) -> String {
        format!("${:.4}", self.external_cost_usd)
    }
    pub fn krabreview_tokens_str(&self) -> String {
        format_token_count(self.api_tokens)
    }
    pub fn krabreview_cost_str(&self) -> String {
        format!("${:.4}", self.api_cost_usd)
    }
    pub fn total_cost_str(&self) -> String {
        format!(
            "${:.4}",
            self.author_cost_usd + self.external_cost_usd + self.api_cost_usd
        )
    }
}

/// Compact human-readable token count: `1234 → "1,234"`,
/// `1_500_000 → "1.5M"`. Used in the stats strip where space is
/// tight.
fn format_token_count(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Number of changelog versions rendered per page/scroll-load. Entries are
/// taller multi-line cards (admin bodies are long), so a smaller page than
/// the home feed (20) fills a viewport-plus without large payloads.
const CHANGELOG_PAGE_SIZE: i64 = 15;

/// Parse-once, serve-from-RAM caches — one per stream. `parse_changelog_entries`
/// (split + per-entry markdown render) would otherwise run on every request;
/// hoisting it into a `LazyLock` parses exactly once for the process lifetime,
/// and each request just clones a ≤`CHANGELOG_PAGE_SIZE` page out of the slice.
/// (Not a literal `const`: parsing allocates `String`s and runs the markdown
/// renderer, neither `const`-evaluable.)
static CHANGELOG_PUBLIC: std::sync::LazyLock<Vec<ChangelogEntry>> =
    std::sync::LazyLock::new(|| parse_changelog_entries(krabarena_shared::changelog::CONTENT));

#[derive(Deserialize)]
pub struct ChangelogQuery {
    #[serde(default)]
    pub page: Option<i64>,
}

/// Full page (page 1) wrapped in `base.html`. The page partial is included
/// inside `changelog.html`, so this carries the same page fields as the
/// fragment template below.
#[derive(Template)]
#[template(path = "pages/changelog.html")]
struct ChangelogTemplate {
    meta: PageMeta,
    entries: Vec<ChangelogEntry>,
    current_page: i64,
    total_pages: i64,
    has_more: bool,
    next_page: i64,
}

/// HTMX fragment for page N (page container + sentinel), appended on scroll.
#[derive(Template)]
#[template(path = "partials/changelog_page.html")]
struct ChangelogPageTemplate {
    entries: Vec<ChangelogEntry>,
    current_page: i64,
    has_more: bool,
    next_page: i64,
}

#[derive(Clone)]
struct ChangelogEntry {
    date: String,
    version: String,
    headline_html: String,
    body_html: String,
}

/// Clamp the requested page into range and clone that page out of the cached
/// slice (LIMIT/OFFSET over an in-memory `Vec`). Out-of-range pages clamp to
/// the nearest valid page so we never render an empty page.
struct ChangelogPage {
    entries: Vec<ChangelogEntry>,
    current_page: i64,
    total_pages: i64,
    has_more: bool,
    next_page: i64,
}

fn paginate_changelog(
    all: &[ChangelogEntry],
    requested: Option<i64>,
    page_size: i64,
) -> ChangelogPage {
    let page_size = page_size.max(1);
    let total = all.len() as i64;
    let total_pages = ((total + page_size - 1) / page_size).max(1);
    let current_page = requested.unwrap_or(1).clamp(1, total_pages);
    let offset = ((current_page - 1) * page_size) as usize;
    let entries = all
        .iter()
        .skip(offset)
        .take(page_size as usize)
        .cloned()
        .collect();
    let has_more = current_page < total_pages;
    ChangelogPage {
        entries,
        current_page,
        total_pages,
        has_more,
        next_page: current_page + 1,
    }
}

/// Parse raw changelog markdown into structured entries.
///
/// Each changelog h2 becomes a [`ChangelogEntry`].  Handles two formats:
///
///   - New: `## VERSION (YYYY-MM-DD) — HEADLINE. Description`
///   - Old: `## VERSION — YYYY-MM-DD — HEADLINE`
///
/// The bold `**…**` span is extracted as the headline; everything after
/// the closing `**` (plus following lines) becomes the body.
fn parse_changelog_entries(markdown: &str) -> Vec<ChangelogEntry> {
    let sections: Vec<&str> = markdown.split("\n## ").collect();
    // sections[0] is the preamble (# Title + intro); skip it.
    sections[1..]
        .iter()
        .filter_map(|section| {
            let first_nl = section.find('\n').unwrap_or(section.len());
            let first_line = section[..first_nl].trim_end_matches('\r');
            let rest_lines = if first_nl < section.len() {
                &section[first_nl + 1..]
            } else {
                ""
            };

            // Try new format: VERSION (YYYY-MM-DD) — REST
            // then old format: VERSION — YYYY-MM-DD — HEADLINE
            let (version, date_raw, after_dash) =
                parse_new_format(first_line).or_else(|| parse_old_format(first_line))?;

            let date = format_changelog_date(date_raw);
            let version = version.to_string();
            let (headline_md, inline_desc) = split_headline(after_dash);
            let body_md = combine_body(&inline_desc, rest_lines);
            let headline_html = render_inline_md(&headline_md);
            let body_html = render_body(&body_md);

            Some(ChangelogEntry {
                date,
                version,
                headline_html,
                body_html,
            })
        })
        .collect()
}

/// `0.21.0 (2026-06-08) — **Headline**. Desc` → (version, date, rest)
fn parse_new_format(line: &str) -> Option<(&str, &str, &str)> {
    let paren_open = line.find('(')?;
    let paren_close = line.find(')')?;
    let dash_sep = line.find(" — ")?;
    if paren_open == 0 || paren_close <= paren_open || dash_sep <= paren_close {
        return None;
    }
    let version = line[..paren_open].trim();
    let date_raw = &line[paren_open + 1..paren_close];
    let after_dash = &line[dash_sep + " — ".len()..];
    Some((version, date_raw, after_dash))
}

/// `0.7.0 — 2026-04-29 — Headline text` → (version, date, rest)
fn parse_old_format(line: &str) -> Option<(&str, &str, &str)> {
    let first_dash = line.find(" — ")?;
    let after_first = &line[first_dash + " — ".len()..];
    let second_dash = after_first.find(" — ")?;

    let version = line[..first_dash].trim();
    let date_raw = after_first[..second_dash].trim();
    // Sanity: date should look like YYYY-MM-DD
    if date_raw.len() != 10 || !date_raw.starts_with("20") {
        return None;
    }
    let rest = after_first[second_dash + " — ".len()..].trim();
    Some((version, date_raw, rest))
}

fn combine_body(inline_desc: &str, rest_lines: &str) -> String {
    match (inline_desc.is_empty(), rest_lines.trim().is_empty()) {
        (true, true) => String::new(),
        (true, false) => rest_lines.to_string(),
        (false, true) => inline_desc.to_string(),
        (false, false) => format!("{}\n\n{}", inline_desc, rest_lines),
    }
}

fn render_body(md: &str) -> String {
    if md.trim().is_empty() {
        String::new()
    } else {
        crate::markdown::render(md)
    }
}

/// "2026-06-08" → "June 8, 2026"
fn format_changelog_date(raw: &str) -> String {
    let parts: Vec<&str> = raw.split('-').collect();
    if parts.len() != 3 {
        return raw.to_string();
    }
    let month = match parts[1] {
        "01" => "January",
        "02" => "February",
        "03" => "March",
        "04" => "April",
        "05" => "May",
        "06" => "June",
        "07" => "July",
        "08" => "August",
        "09" => "September",
        "10" => "October",
        "11" => "November",
        "12" => "December",
        _ => return raw.to_string(),
    };
    let day = parts[2].trim_start_matches('0');
    format!("{} {}, {}", month, day, parts[0])
}

/// Extract the bold headline and the remaining inline description.
///
/// Returns `(headline_markdown, remaining_description)`.
fn split_headline(text: &str) -> (String, String) {
    if let Some(open) = text.find("**") {
        let after_open = &text[open + 2..];
        if let Some(close_off) = after_open.find("**") {
            let close_pos = open + 2 + close_off + 2; // byte after closing **
            let remainder = &text[close_pos..];

            // If ". " follows the closing ** → clean headline/body split.
            if let Some(stripped) = remainder.strip_prefix(". ") {
                return (text[..close_pos].to_string(), stripped.trim().to_string());
            }
            // Period only or empty → headline with no body.
            if remainder.is_empty() || remainder == "." {
                return (text[..close_pos].to_string(), String::new());
            }
            // Non-bold continuation after bold (e.g. REVERTED entries).
            // Find next ". " + uppercase as sentence break.
            for (i, _) in remainder.match_indices(". ") {
                let after = &remainder[i + 2..];
                if after.starts_with(|c: char| c.is_uppercase() || c == '`' || c == '*') {
                    let abs = close_pos + i;
                    return (text[..abs].to_string(), text[abs + 2..].trim().to_string());
                }
            }
            // No sentence break found → everything is headline.
            return (text.to_string(), String::new());
        }
    }
    // No bold markers — use everything as headline.
    (text.to_string(), String::new())
}

/// Render a short markdown fragment and strip the outer `<p>` wrapper
/// so the result can be placed inside an existing block element.
fn render_inline_md(md: &str) -> String {
    let html = crate::markdown::render(md);
    let t = html.trim();
    if t.starts_with("<p>") && t.ends_with("</p>") && t.matches("<p>").count() == 1 {
        t[3..t.len() - 4].to_string()
    } else {
        html
    }
}

#[derive(Template)]
#[template(path = "pages/arena.html")]
struct ArenaTemplate {
    meta: PageMeta,
    entries: Vec<ArenaRow>,
    cost: ReviewCostRollup,
}

#[derive(Template)]
#[template(path = "pages/battle_candidates.html")]
struct BattleCandidatesTemplate {
    meta: PageMeta,
    candidates: Vec<crate::services::battle_candidates_service::BattleCandidate>,
    total: i64,
    page: i64,
    has_more: bool,
}

#[derive(Template)]
#[template(path = "pages/about.html")]
struct AboutTemplate {
    meta: PageMeta,
    vocab: &'static [krabarena_shared::about::VocabEntry],
    mission: &'static [&'static str],
    faq: &'static [(&'static str, &'static str)],
    sign_off: &'static str,
    /// Pre-built OAuth authorize URLs (same shape as `/login`). `None` when
    /// the corresponding `*_CLIENT_ID` env var isn't configured — in which
    /// case the matching button isn't rendered. Avoids the prior bug where
    /// the GitHub button was hardcoded with `https://krabarena.org` as the
    /// redirect URI, breaking OAuth on test/local environments.
    github_url: Option<String>,
    google_url: Option<String>,
    /// reCAPTCHA Enterprise site key, shared with `/login`. Empty string =
    /// captcha disabled (the partial renders nothing). The `/about` email
    /// sign-in posts to the same captcha-gated endpoint, so it must carry the
    /// same widget — see `partials/recaptcha.html`.
    recaptcha_site_key: String,
}

#[derive(Template)]
#[template(path = "pages/uikit_showcase.html")]
struct UiKitShowcaseTemplate {
    meta: PageMeta,
}

// --- Markdown (text/markdown) templates for AI-agent / curl clients. ---

#[derive(Template)]
#[template(path = "md/index.md", escape = "none")]
struct IndexMdTemplate {
    meta: PageMeta,
    /// CMS-driven agent-onboarding header — install one-liner + the
    /// per-page "what's next" block, both pulled from the `config`
    /// table (`cms_install`, `cms_next_home`). Populated by
    /// `cms_service::render_header` so admins can edit either block
    /// at runtime via `krab admin config-set` without a release.
    agent_header: String,
    /// Unified Battle + Claim feed, top 5 by effort temperature
    /// (0.20.0). Same ranking as the HTML home page.
    feed: Vec<FeedItem>,
    active_status: Option<String>,
    total_battles: i64,
    total_claims: i64,
    total_verifications_today: i64,
}

#[derive(Template)]
#[template(path = "md/battle.md", escape = "none")]
struct BattleMdTemplate {
    meta: PageMeta,
    /// Agent-onboarding header — see `IndexMdTemplate::agent_header`.
    /// For Battle pages the per-page block (`cms_next_battle`) has
    /// `{battle_id}` substituted to the actual UUID.
    agent_header: String,
    battle: BattleCard,
    claims: Vec<ClaimCardView>,
}

#[derive(Template)]
#[template(path = "md/claim.md", escape = "none")]
struct ClaimMdTemplate {
    meta: PageMeta,
    /// Agent-onboarding header — see `IndexMdTemplate::agent_header`.
    /// For Claim pages the per-page block (`cms_next_claim`) has
    /// both `{claim_id}` and `{battle_id}` substituted.
    agent_header: String,
    claim: ClaimCardView,
    battle_id: String,
    author_name: String,
    verifies: Vec<VerificationRow>,
    refutes: Vec<VerificationRow>,
    comment_threads: Vec<crate::web::comment_threads::CommentView>,
    user_names: std::collections::HashMap<String, String>,
}

#[derive(Template)]
#[template(path = "md/agent.md", escape = "none")]
struct AgentMdTemplate {
    meta: PageMeta,
    agent: AgentRow,
    activity: Vec<ActivityItem>,
}

#[derive(Template)]
#[template(path = "md/arena.md", escape = "none")]
struct ArenaMdTemplate {
    meta: PageMeta,
    entries: Vec<ArenaRow>,
}

#[derive(Template)]
#[template(path = "md/leaderboard.md", escape = "none")]
struct LeaderboardMdTemplate {
    meta: PageMeta,
    agents: Vec<AgentRow>,
}

#[derive(Template)]
#[template(path = "md/about.md", escape = "none")]
struct AboutMdTemplate {
    meta: PageMeta,
    tagline: &'static str,
    vocab: &'static [krabarena_shared::about::VocabEntry],
    motto: &'static str,
    mission: &'static [&'static str],
    faq: &'static [(&'static str, &'static str)],
    sign_off: &'static str,
}

#[derive(Template)]
#[template(path = "md/changelog.md", escape = "none")]
struct ChangelogMdTemplate {
    meta: PageMeta,
    content: String,
}

#[derive(Debug, Clone)]
pub struct NotificationRow {
    pub kind_label: String,
    pub kind_css: String,
    pub actor: String,
    /// Title of the target Claim / Battle (empty when unknown).
    pub target: String,
    pub body: String,
    pub link: String,
    pub created_at: String,
    pub unread: bool,
}

#[derive(Template)]
#[template(path = "pages/notifications.html")]
struct NotificationsTemplate {
    meta: PageMeta,
    authenticated: bool,
    items: Vec<NotificationRow>,
}

#[derive(Template)]
#[template(path = "md/notifications.md", escape = "none")]
struct NotificationsMdTemplate {
    meta: PageMeta,
    authenticated: bool,
    items: Vec<NotificationRow>,
}

// --- Query params ---

#[derive(Debug, Deserialize)]
pub struct BattleCandidatesQuery {
    pub page: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct IndexQuery {
    pub status: Option<String>,
    pub domain: Option<String>,
    /// Unified tag filter (tag navigation, 0.24.0). Case-insensitively
    /// narrows every tab to items matching this tag — battles by
    /// `domain_tags`, claims/verifications by their claim's `topic_tags`
    /// or parent Battle's `domain_tags`. Driven by the home-page tag
    /// autocomplete and the `#tag` chips. Distinct from the legacy
    /// `domain` (exact, battles-only) param.
    pub tag: Option<String>,
    /// Free-text keyword filter for the home feed (hybrid filter bar).
    /// Case-insensitive substring (`ILIKE`) match against each panel's
    /// text — battles by title/description/summary, claims by claim
    /// text/topic/parent-battle title, verifications by claim text +
    /// battle title. Combines with `tag` (AND). Driven by the same
    /// home-page filter input: typed text that isn't promoted to a tag
    /// pill lands here.
    pub q: Option<String>,
    /// Entity tab selector for the home page (discovery redesign,
    /// 0.23.0): `battles` (default), `claims`, or `verifications`.
    /// Server-rendered links — no JS — so the active tab is SEO- and
    /// no-script-friendly. Unknown values fall back to `battles`.
    pub tab: Option<String>,
    /// Feed sort. `hot` is the current/default effort ranking;
    /// `new` is supported for shareable/debug URLs without adding new
    /// visible UI.
    pub sort: Option<String>,
    /// 1-based page for no-JS Next links and HTMX infinite-scroll
    /// fragments. Invalid/negative values clamp to page 1.
    pub page: Option<i64>,
    /// Legacy 0.24.0 custom-JS fragment flag. Kept accepted so old
    /// in-flight links do not 400; HTMX now keys off `HX-Request`.
    #[allow(dead_code)]
    pub fragment: Option<String>,
    /// Mirrors the `?theme=` switch handled by `base.html`'s inline
    /// script. Extracted server-side too so the very first request
    /// after a user clicks `?theme=arena` already renders the arena
    /// sections (otherwise we'd render classic-shape HTML with the
    /// arena CSS, which flashes weird until the next navigation).
    pub theme: Option<String>,
}

#[derive(Debug, Default)]
struct HomeQuery {
    status: Option<String>,
    domain: Option<String>,
    tags: Vec<String>,
    q: Option<String>,
    tab: Option<String>,
    sort: Option<String>,
    page: Option<i64>,
    fragment: Option<String>,
    theme: Option<String>,
}

fn decode_query_component(input: &str) -> String {
    let plus_fixed = input.replace('+', " ");
    urlencoding::decode(plus_fixed.as_str())
        .map(|value| value.into_owned())
        .unwrap_or(plus_fixed)
}

fn parse_home_query(raw_query: Option<&str>) -> HomeQuery {
    let raw_query = raw_query.unwrap_or("");
    if raw_query.is_empty() {
        return HomeQuery::default();
    }

    let mut query = HomeQuery::default();
    for part in raw_query.split('&') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = match part.split_once('=') {
            Some((k, v)) => (k, v),
            None => (part, ""),
        };
        let key = decode_query_component(key);
        let value = decode_query_component(value);

        match key.as_str() {
            "status" => query.status = Some(value),
            "domain" => query.domain = Some(value),
            "tag" => query.tags.push(value),
            "q" => query.q = Some(value),
            "tab" => query.tab = Some(value),
            "sort" => query.sort = Some(value),
            "page" => {
                query.page = value.parse::<i64>().ok();
            }
            "fragment" => query.fragment = Some(value),
            "theme" => query.theme = Some(value),
            _ => {}
        }
    }
    query
}

fn query_tags(query: &HomeQuery) -> Vec<String> {
    if query.tags.is_empty() {
        Vec::new()
    } else {
        normalize_home_tags(Some(&query.tags.join(",")))
    }
}

fn query_q(query: &HomeQuery) -> String {
    query.q.as_deref().map(str::trim).unwrap_or("").to_string()
}

fn home_is_indexable_default_state(query: &HomeQuery, tags: &[String], q: &str) -> bool {
    query.tab.is_none()
        && query.status.is_none()
        && query.domain.is_none()
        && tags.is_empty()
        && q.is_empty()
        && query.sort.is_none()
        && query.page.is_none()
}

fn search_should_noindex(q: &str, kind_label: &str, limit: Option<i64>) -> bool {
    !q.is_empty() || kind_label != "all" || limit.is_some()
}

fn is_hx_request(headers: &HeaderMap) -> bool {
    headers
        .get("HX-Request")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

fn normalize_home_tab(tab: Option<&str>) -> &'static str {
    match tab {
        Some("claims") => "claims",
        Some("verifications") => "verifications",
        _ => "battles",
    }
}

fn normalize_home_sort(sort: Option<&str>) -> &'static str {
    match sort {
        Some("new") => "new",
        _ => "hot",
    }
}

fn normalize_home_page(page: Option<i64>) -> i64 {
    page.unwrap_or(1).clamp(1, HOME_FEED_MAX_PAGE)
}

fn normalize_home_tags(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .fold(Vec::new(), |mut acc, t| {
            if !acc.contains(&t) {
                acc.push(t);
            }
            acc
        })
}

fn home_url(
    tab: &str,
    status: Option<&str>,
    tags: &[String],
    q: &str,
    sort: &str,
    page: i64,
) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    if tab != "battles" {
        pairs.push(("tab".into(), tab.into()));
    }
    if let Some(status) = status.filter(|s| !s.is_empty()) {
        pairs.push(("status".into(), status.into()));
    }
    if !tags.is_empty() {
        pairs.push(("tag".into(), tags.join(",")));
    }
    let q = q.trim();
    if !q.is_empty() {
        pairs.push(("q".into(), q.into()));
    }
    if sort != "hot" {
        pairs.push(("sort".into(), sort.into()));
    }
    if page > 1 {
        pairs.push(("page".into(), page.to_string()));
    }
    if pairs.is_empty() {
        return "/".into();
    }
    let query = pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(&v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("/?{query}")
}

fn query_without_last_token(q: &str) -> String {
    let trimmed_end = q.trim_end();
    let Some((prefix, _)) = trimmed_end.rsplit_once(char::is_whitespace) else {
        return String::new();
    };
    prefix.trim_end().to_string()
}

fn suggestion_prefix(q: &str) -> String {
    q.split_whitespace().last().unwrap_or("").to_lowercase()
}

fn response_with_vary<T: Template>(
    template: T,
    public_base_url: &str,
    path: &str,
    context: &'static str,
    vary: &'static str,
) -> Response {
    let mut resp =
        crate::web::errors::render_template_or_500(template, public_base_url, path, context);
    resp.headers_mut()
        .insert(header::VARY, HeaderValue::from_static(vary));
    resp
}

// --- Handlers ---

pub async fn about(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    use krabarena_shared::about;
    // Same `/auth/voter/{provider}/start` entry as /login (OAuth state
    // cookie + CSRF-checked callback; see routes::oauth).
    let (github_url, google_url) = crate::routes::oauth::web_provider_links();

    let meta = PageMeta::new(&state.public_base_url, "/about")
        .with_title("About")
        .with_description(about::TAGLINE);
    let html = AboutTemplate {
        meta: meta.clone(),
        vocab: about::VOCAB,
        mission: about::MISSION,
        faq: about::FAQ,
        sign_off: about::SIGN_OFF,
        github_url,
        google_url,
        recaptcha_site_key: state.recaptcha.site_key.clone(),
    };
    let md = AboutMdTemplate {
        meta,
        tagline: about::TAGLINE,
        vocab: about::VOCAB,
        motto: about::MOTTO,
        mission: about::MISSION,
        faq: about::FAQ,
        sign_off: about::SIGN_OFF,
    };
    render_negotiated(&state.public_base_url, "/about", &headers, html, md)
}

pub async fn uikit_showcase(State(state): State<AppState>) -> impl IntoResponse {
    if std::env::var("KRAB_UI_SHOWCASE").ok().as_deref() != Some("1") {
        return crate::web::errors::render_404(&state.public_base_url, "/dev/uikit");
    }
    let meta = PageMeta::new(&state.public_base_url, "/dev/uikit")
        .with_title("UI-kit showcase")
        .with_description("Development-only KrabArena UI-kit showcase.")
        .noindex_follow();
    crate::web::errors::render_template_or_500(
        UiKitShowcaseTemplate { meta },
        &state.public_base_url,
        "/dev/uikit",
        "uikit_showcase.html",
    )
}

pub async fn changelog(
    State(state): State<AppState>,
    Query(q): Query<ChangelogQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let p = paginate_changelog(&CHANGELOG_PUBLIC, q.page, CHANGELOG_PAGE_SIZE);

    // HTMX scroll-load (sentinel revealed): return just the page fragment —
    // the page's cards plus the next sentinel — to append into the list.
    if is_hx_request(&headers) {
        return response_with_vary(
            ChangelogPageTemplate {
                entries: p.entries,
                current_page: p.current_page,
                has_more: p.has_more,
                next_page: p.next_page,
            },
            &state.public_base_url,
            "/changelog",
            "changelog_page.html",
            "Accept, User-Agent, HX-Request",
        );
    }

    let meta = PageMeta::new(&state.public_base_url, "/changelog")
        .with_title("Changelog")
        .with_description("What's new in KrabArena — release notes and version history.");
    let html = ChangelogTemplate {
        meta: meta.clone(),
        entries: p.entries,
        current_page: p.current_page,
        total_pages: p.total_pages,
        has_more: p.has_more,
        next_page: p.next_page,
    };
    // CLI / curl (text/markdown) still get the COMPLETE changelog, unpaginated.
    let md = ChangelogMdTemplate {
        meta,
        content: krabarena_shared::changelog::CONTENT.to_string(),
    };
    let mut resp = render_negotiated(&state.public_base_url, "/changelog", &headers, html, md);
    // The shell (non-HX) and the fragment (HX) differ for the same URL.
    resp.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static("Accept, User-Agent, HX-Request"),
    );
    resp
}

/// `/battle_candidates` — full ranked list of community-proposed Battle
/// titles. Top-3 on the home page is also rendered here. Reads viewer
/// auth from the JWT cookie/header so the upvote arrow can render in
/// its active state when the user has already voted.
pub async fn battle_candidates_page(
    State(state): State<AppState>,
    Query(q): Query<BattleCandidatesQuery>,
    _headers: HeaderMap,
) -> impl IntoResponse {
    use crate::services::battle_candidates_service as svc;
    // Voter session lives in localStorage, not a cookie, so server-side
    // can't pre-populate `viewer_voted`. Client-side JS hydrates the
    // active state by re-fetching /v1/battle-candidates after load.
    let viewer = None;
    let page = q.page.unwrap_or(1).max(1);
    let page_size: i64 = 20;
    let offset = (page - 1) * page_size;
    let candidates = svc::list_candidates(&state.pool, viewer, page_size + 1, offset, None)
        .await
        .unwrap_or_default();
    let has_more = candidates.len() as i64 > page_size;
    let candidates: Vec<_> = candidates.into_iter().take(page_size as usize).collect();
    let total = svc::count_open(&state.pool, None).await.unwrap_or(0);
    let meta = PageMeta::new(&state.public_base_url, "/battle_candidates")
        .with_title("Battle candidates")
        .with_description("Community-proposed Battles. Upvote what you want to see opened.");
    let _ = page_size;
    let html = BattleCandidatesTemplate {
        meta,
        candidates,
        total,
        page,
        has_more,
    };
    crate::web::errors::render_template_or_500(
        html,
        &state.public_base_url,
        "/battle_candidates",
        "battle_candidates.html",
    )
}

pub async fn arena(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    // The merged rows + cost rollup are identical for every viewer, so
    // serve them from the short-TTL leaderboard cache and recompute at
    // most once per TTL window. See `arena_entries_and_cost`.
    let (entries, cost) = state
        .leaderboards
        .arena
        .get_or_fill("default", || arena_entries_and_cost(&state.pool))
        .await;

    let meta = PageMeta::new(&state.public_base_url, "/arena")
        .with_title("Arena — Agent Leaderboard")
        .with_description(
            "AI agent backends and models ranked by verified contributions on KrabArena.",
        );

    let html = ArenaTemplate {
        meta: meta.clone(),
        entries: entries.clone(),
        cost,
    };
    let md = ArenaMdTemplate { meta, entries };
    render_negotiated(&state.public_base_url, "/arena", &headers, html, md)
}

/// The expensive half of the `/arena` page: the per-(backend, model)
/// verify/claim/refute aggregate, the whole-arena cost rollup, and the
/// per-(backend, model) cost merge + `$ effort` sort. Extracted so the
/// `arena` handler can memoise the result (`AppState::leaderboards`).
async fn arena_entries_and_cost(pool: &sqlx::PgPool) -> (Vec<ArenaRow>, ReviewCostRollup) {
    // Arena leaderboard: superseded claim versions don't count toward
    // a (backend, model)'s claim total — only the lineage HEAD does.
    let superseded = crate::models::claim::not_superseded("c");
    let rows = sqlx::query_as::<_, (String, String, i64, i64, i64)>(&format!(
        r#"
        SELECT
            agent_backend,
            agent_model,
            SUM(verify)::bigint as total_verify,
            SUM(claims)::bigint as total_claims,
            SUM(refute)::bigint as total_refute
        FROM (
            SELECT agent_backend, COALESCE(agent_model, 'unknown') as agent_model, COUNT(*) as verify, 0::bigint as claims, 0::bigint as refute
            FROM verifications WHERE agent_backend IS NOT NULL AND verdict = 'confirmed'
            GROUP BY agent_backend, agent_model
            UNION ALL
            SELECT c.agent_backend, COALESCE(c.agent_model, 'unknown') as agent_model, 0::bigint, COUNT(*), 0::bigint
            FROM claims c WHERE c.agent_backend IS NOT NULL AND c.deleted_at IS NULL {superseded}
            GROUP BY c.agent_backend, c.agent_model
            UNION ALL
            SELECT agent_backend, COALESCE(agent_model, 'unknown') as agent_model, 0::bigint, 0::bigint, COUNT(*)
            FROM verifications WHERE agent_backend IS NOT NULL AND verdict = 'disconfirmed'
            GROUP BY agent_backend, agent_model
        ) combined
        GROUP BY agent_backend, agent_model
        ORDER BY total_verify DESC, total_claims DESC
        LIMIT 100
        "#
    ))
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut entries: Vec<ArenaRow> = rows
        .into_iter()
        .map(|(backend, model, verify, claims, refute)| ArenaRow {
            agent_backend: backend,
            agent_model: model,
            total_verify: verify,
            total_claims: claims,
            total_refute: refute,
            author_tokens: 0,
            author_cost_usd: 0.0,
            external_cost_usd: 0.0,
            krabreview_cost_usd: 0.0,
        })
        .collect();

    let cost = arena_cost_rollup(pool).await;

    // Merge cost-by-(backend, model) aggregates onto each ArenaRow so
    // the table can render cost columns inline (the standalone
    // "Cost by backend" table was folded in). Note the key normalization:
    // `cost_by_backend_query` returns COALESCE'd empty strings for null
    // backend/model, while the entries query uses
    // `COALESCE(agent_model, 'unknown')`. We map empty model to
    // "unknown" when keying so the join lands.
    let cost_by_backend = arena_cost_by_backend(pool).await;
    let cost_index: std::collections::HashMap<(String, String), CostByBackendRow> = cost_by_backend
        .into_iter()
        .map(|c| {
            let model = if c.model.is_empty() {
                "unknown".to_string()
            } else {
                c.model.clone()
            };
            ((c.backend.clone(), model), c)
        })
        .collect();
    for entry in entries.iter_mut() {
        if let Some(c) = cost_index.get(&(entry.agent_backend.clone(), entry.agent_model.clone())) {
            entry.apply_cost(c);
        }
    }

    // Sort by total $ effort desc — same metric the column header
    // labels as "$ effort". Falls back to verifies/claims so two
    // entries at $0 still get a stable order. Done in Rust because
    // the SQL stage doesn't have the cost columns; the row count is
    // capped at 100 so the in-memory sort cost is trivial.
    entries.sort_by(|a, b| {
        let total_a = a.author_cost_usd + a.external_cost_usd + a.krabreview_cost_usd;
        let total_b = b.author_cost_usd + b.external_cost_usd + b.krabreview_cost_usd;
        total_b
            .partial_cmp(&total_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.total_verify.cmp(&a.total_verify))
            .then(b.total_claims.cmp(&a.total_claims))
    });

    (entries, cost)
}

/// Sum the LLM-cost columns + claimer-reported compute across the
/// whole arena. Used by the `/arena` handler. Failures degrade to
/// zero rather than 500'ing the page.
///
/// One round-trip: a CROSS JOIN of two scalar aggregates rolls up
/// the `claims` side (author tokens + cents + external $) and the
/// `claim_reviews` side (KrabReviewer tokens + cost) in parallel
/// inside Postgres. The two halves scan different tables so the
/// planner can run them concurrently. Returns zeros on query
/// failure rather than 500'ing the page. Author-reported external
/// API spend is a JSONB scalar (`claim_structure -> cost ->>
/// external_usd`); the cast-on-NULL returns NULL and SUM skips it,
/// so no WHERE filter is needed.
async fn arena_cost_rollup(pool: &sqlx::PgPool) -> ReviewCostRollup {
    // Arena cost rollup: sum author-compute over lineage HEADs only,
    // so an edit's old version doesn't double-count its tokens / $.
    let superseded = crate::models::claim::not_superseded("cl");
    let row = sqlx::query_as::<_, (i64, i64, f64, i64, i64)>(&format!(
        "SELECT \
           c.author_tokens, c.author_cents, c.external_usd, \
           r.api_tokens, r.api_cost_micros \
         FROM \
           (SELECT \
              COALESCE(SUM(cl.tokens_consumed), 0)::bigint AS author_tokens, \
              COALESCE(SUM(cl.price_cents),     0)::bigint AS author_cents, \
              COALESCE(SUM((cl.claim_structure -> 'cost' ->> 'external_usd')::numeric), 0)::float8 \
                                                        AS external_usd \
            FROM claims cl WHERE cl.deleted_at IS NULL {superseded}) c \
         CROSS JOIN \
           (SELECT \
              COALESCE(SUM(COALESCE(prompt_tokens, 0) + COALESCE(completion_tokens, 0)), 0)::bigint \
                                                        AS api_tokens, \
              COALESCE(SUM(cost_micros), 0)::bigint     AS api_cost_micros \
            FROM claim_reviews) r",
    ))
    .fetch_one(pool)
    .await
    .unwrap_or((0, 0, 0.0, 0, 0));

    let (author_tokens, author_cents, external_cost_usd, api_tokens, api_cost_micros) = row;
    ReviewCostRollup {
        author_tokens,
        author_cost_usd: author_cents as f64 / 100.0,
        external_cost_usd,
        api_tokens,
        api_cost_usd: api_cost_micros as f64 / 1_000_000.0,
    }
}

/// Per-(backend, model) breakdown across every claim — used by the
/// arena page to show which agents drive cost. SQL groups
/// `claims.agent_backend` × `claims.agent_model` and joins
/// `claim_reviews` on `review_id` so reviewer costs land on the
/// row of the claim that triggered them.
async fn arena_cost_by_backend(pool: &sqlx::PgPool) -> Vec<CostByBackendRow> {
    cost_by_backend_query(pool, None).await
}

async fn agent_cost_by_backend(pool: &sqlx::PgPool, user_id: Uuid) -> Vec<CostByBackendRow> {
    cost_by_backend_query(pool, Some(user_id)).await
}

async fn cost_by_backend_query(
    pool: &sqlx::PgPool,
    user_id: Option<Uuid>,
) -> Vec<CostByBackendRow> {
    // GROUP BY (agent_backend, agent_model). LEFT JOIN claim_reviews
    // so claims without a review still contribute author-side cost.
    // Rollups are sums; downstream callers sort by total $ desc.
    //
    // External-API spend lives in the JSONB structure
    // (`claim_structure->'cost'->>'external_usd'`); cast to numeric on
    // the way out. The DISTINCT on c.id-keyed sums avoids double-
    // counting when a claim has multiple review rows from the LEFT
    // JOIN (matches how author_cents already deduplicates).
    // Superseded versions are excluded from the cost-by-backend
    // rollup (arena + per-agent) — only the lineage HEAD's cost counts.
    let superseded = crate::models::claim::not_superseded("c");
    let where_clause = if user_id.is_some() {
        format!("WHERE c.author_id = $1 AND c.deleted_at IS NULL {superseded}")
    } else {
        format!("WHERE c.deleted_at IS NULL {superseded}")
    };
    let sql = format!(
        "SELECT \
           COALESCE(c.agent_backend, '') AS backend, \
           COALESCE(c.agent_model, '')   AS model, \
           COUNT(DISTINCT c.id)::bigint   AS claim_count, \
           COALESCE(SUM(DISTINCT c.tokens_consumed), 0)::bigint AS author_tokens, \
           COALESCE(SUM(DISTINCT c.price_cents), 0)::bigint     AS author_cents, \
           COALESCE(SUM(DISTINCT (c.claim_structure -> 'cost' ->> 'external_usd')::numeric), 0)::float8 AS external_cost_usd, \
           COALESCE(SUM(COALESCE(r.prompt_tokens,0) + COALESCE(r.completion_tokens,0)), 0)::bigint AS api_tokens, \
           COALESCE(SUM(r.cost_micros), 0)::bigint              AS api_cost_micros \
         FROM claims c \
         LEFT JOIN claim_reviews r ON r.id = c.review_id \
         {where_clause} \
         GROUP BY COALESCE(c.agent_backend, ''), COALESCE(c.agent_model, '') \
         ORDER BY ( \
           COALESCE(SUM(DISTINCT c.price_cents), 0) \
           + COALESCE(SUM(DISTINCT (c.claim_structure -> 'cost' ->> 'external_usd')::numeric * 100), 0) \
           + (COALESCE(SUM(r.cost_micros), 0) / 10000) \
         ) DESC \
         LIMIT 50"
    );
    let mut q = sqlx::query_as::<_, (String, String, i64, i64, i64, f64, i64, i64)>(&sql);
    if let Some(uid) = user_id {
        q = q.bind(uid);
    }
    let rows = q.fetch_all(pool).await.unwrap_or_default();
    rows.into_iter()
        .map(
            |(backend, model, n, atk, acents, ext_usd, apitok, apimicros)| CostByBackendRow {
                backend,
                model,
                claim_count: n,
                author_tokens: atk,
                author_cost_usd: acents as f64 / 100.0,
                external_cost_usd: ext_usd,
                api_tokens: apitok,
                api_cost_usd: apimicros as f64 / 1_000_000.0,
            },
        )
        .collect()
}

/// Same shape, scoped to a single agent (= reviews on claims this
/// agent authored, plus their own claims' compute).
async fn agent_cost_rollup(pool: &sqlx::PgPool, user_id: Uuid) -> ReviewCostRollup {
    let review_row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
        "SELECT \
           COALESCE(SUM(COALESCE(prompt_tokens,0) + COALESCE(completion_tokens,0)),0)::bigint, \
           COALESCE(SUM(cost_micros),0)::bigint \
         FROM claim_reviews WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or((Some(0), Some(0)));
    // Per-contributor cost rollups: HEADs only (an edit's old version
    // must not double-count its author compute).
    let superseded = crate::models::claim::not_superseded("c");
    let author_row = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(&format!(
        "SELECT \
           COALESCE(SUM(c.tokens_consumed),0)::bigint, \
           COALESCE(SUM(c.price_cents),0)::bigint \
         FROM claims c WHERE c.author_id = $1 AND c.deleted_at IS NULL {superseded}"
    ))
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or((Some(0), Some(0)));
    let external_row = sqlx::query_as::<_, (Option<f64>,)>(&format!(
        "SELECT \
           COALESCE(SUM((c.claim_structure -> 'cost' ->> 'external_usd')::numeric), 0)::float8 \
         FROM claims c \
         WHERE c.author_id = $1 \
           AND c.deleted_at IS NULL {superseded} \
           AND c.claim_structure -> 'cost' -> 'external_usd' IS NOT NULL"
    ))
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or((Some(0.0),));

    let api_tokens = review_row.0.unwrap_or(0);
    let api_cost_micros = review_row.1.unwrap_or(0);
    let api_cost_usd = api_cost_micros as f64 / 1_000_000.0;
    let author_tokens = author_row.0.unwrap_or(0);
    let author_cents = author_row.1.unwrap_or(0);
    let author_cost_usd = author_cents as f64 / 100.0;
    let external_cost_usd = external_row.0.unwrap_or(0.0);
    ReviewCostRollup {
        author_tokens,
        author_cost_usd,
        external_cost_usd,
        api_tokens,
        api_cost_usd,
    }
}

/// Aggregated cost roll-up for a single battle. Fed into
/// `BattleCard::apply_cost_agg` — combined tokens + $ across every
/// claim attached to the battle (author tokens + KrabReview tokens;
/// author $ + external $ + KrabReview $).
#[derive(Debug, Clone, Default)]
pub struct BattleCostAgg {
    pub tokens: i64,
    pub cost_usd: f64,
}

/// Pull aggregated tokens + $ across every claim attached to each
/// battle in `battle_ids`, in a single round-trip (avoid N+1). Joins
/// `claim_reviews` via `review_id` so KrabReview tokens / $ are
/// summed alongside the author's own. Returns an empty map on query
/// failure — the cards just render without cost chips.
pub(crate) async fn battle_cost_aggregates(
    pool: &sqlx::PgPool,
    battle_ids: &[Uuid],
) -> std::collections::HashMap<Uuid, BattleCostAgg> {
    if battle_ids.is_empty() {
        return std::collections::HashMap::new();
    }
    // Author tokens / cents come from `claims`; one row per claim, so
    // a plain `SUM` is correct. External $ comes from a JSONB cast on
    // each claim row (also one-per-claim, plain SUM).
    //
    // KrabReview tokens / cost_micros live on `claim_reviews`. Each
    // claim has at most one `review_id`, but a single review row can
    // be referenced by multiple claims (re-runs). We join one row per
    // claim, so summing review_*` directly would double-count if the
    // same review_id appeared on more than one claim under the same
    // battle. In practice that's rare, but we use `SUM(DISTINCT …)`
    // on the review-side aggregates to be safe — same pattern as
    // `top_agents_by_effort` and `arena_cost_by_backend`.
    // Per-battle cost aggregate (feed cards): HEADs only — an edited
    // claim's old version doesn't add to the battle's cost chips.
    let superseded = crate::models::claim::not_superseded("c");
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            Option<i64>,
            Option<i64>,
            Option<f64>,
            Option<i64>,
            Option<i64>,
        ),
    >(&format!(
        "SELECT \
           c.battle_id, \
           COALESCE(SUM(c.tokens_consumed), 0)::bigint AS author_tokens, \
           COALESCE(SUM(c.price_cents), 0)::bigint     AS author_cents, \
           COALESCE(SUM((c.claim_structure -> 'cost' ->> 'external_usd')::numeric), 0)::float8 \
                                                       AS external_usd, \
           COALESCE(SUM(DISTINCT COALESCE(r.prompt_tokens, 0) + COALESCE(r.completion_tokens, 0)), 0)::bigint \
                                                       AS api_tokens, \
           COALESCE(SUM(DISTINCT r.cost_micros), 0)::bigint AS api_cost_micros \
         FROM claims c \
         LEFT JOIN claim_reviews r ON r.id = c.review_id \
         WHERE c.battle_id = ANY($1) AND c.deleted_at IS NULL {superseded} \
         GROUP BY c.battle_id"
    ))
    .bind(battle_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|(bid, atok, acents, ext_usd, apitok, apimicros)| {
            let tokens = atok.unwrap_or(0) + apitok.unwrap_or(0);
            let cost_usd = (acents.unwrap_or(0) as f64 / 100.0)
                + ext_usd.unwrap_or(0.0)
                + (apimicros.unwrap_or(0) as f64 / 1_000_000.0);
            (bid, BattleCostAgg { tokens, cost_usd })
        })
        .collect()
}

pub async fn index(
    State(state): State<AppState>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> impl IntoResponse {
    let query = parse_home_query(raw_query.as_deref());
    let hx_request = is_hx_request(&headers);
    let active_page = normalize_home_page(query.page);
    let feed_offset = (active_page - 1) * HOME_FEED_PAGE_SIZE;
    let feed_fetch_limit = HOME_FEED_PAGE_SIZE + 1;

    // Translate legacy filter values (`contested`, `holding`) to the
    // current canonical `active` so old bookmarks and external links
    // don't return empty pages after the v0.5.0 migration. Anything
    // else passes through unchanged so an invalid filter still
    // returns no results rather than silently switching to "All".
    let normalized_status = query.status.as_deref().map(|s| {
        krabarena_shared::domain::BattleStatus::parse_lenient(s)
            .map(|st| st.to_string())
            .unwrap_or_else(|| s.to_string())
    });

    // Entity tab (discovery redesign, 0.23.0). Unknown / missing →
    // "battles" (the clean default ribbon). Status chips only apply on
    // the battles tab.
    let active_tab = normalize_home_tab(query.tab.as_deref()).to_string();
    let active_sort = normalize_home_sort(query.sort.as_deref()).to_string();

    // Unified multi-tag filter (tag navigation, 0.24.0). Comma-separated
    // `?tag=rust,go`, lowercased + trimmed + deduped. AND semantics: an
    // item must match EVERY selected tag. Bound as a `text[]` into each
    // panel query below and surfaced to the template as removable pills.
    let tags = query_tags(&query);

    // Free-text keyword filter (hybrid filter bar). The raw trimmed term
    // pre-fills the input on render; `q_patterns` is the whitespace-split
    // `ILIKE` form — one `%word%` per word, each with `\`, `%`, `_` escaped
    // so user input can't act as a wildcard (bound as a `text[]` and matched
    // with `ESCAPE '\'` in each panel query). AND semantics across words: a
    // row must contain EVERY word somewhere in its searched columns (so
    // `python typescript` matches "Python vs TypeScript vs …" even though the
    // words aren't adjacent). Empty → empty array, and the SQL
    // `cardinality = 0` branch skips the filter.
    //
    // NOTE: the `NOT EXISTS (… WHERE NOT (colA ILIKE kw OR colB …))` form is
    // three-valued-logic sensitive — a NULL column (standalone claims have a
    // NULL `c.topic`/parent `h.title`; some battles a NULL `short_summary`)
    // makes `false OR NULL = NULL`, then `NOT NULL = NULL`, so a non-matching
    // row silently survives the filter. Every searched column is therefore
    // wrapped in `COALESCE(col, '')` so a missing value is a definite non-match.
    let active_q = query_q(&query);
    let q_patterns: Vec<String> = active_q
        .split_whitespace()
        .map(|w| {
            format!(
                "%{}%",
                w.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        })
        .collect();

    let partial_only = hx_request;

    // --- The three panel queries — Battles, Claims, Verifications — are
    // independent, so fire them concurrently with `tokio::join!` (perf,
    // 0.24.0). On a high-latency link the home page (and the panels-only
    // fragment swap, which runs exactly these three) pays ~1 round-trip
    // instead of three. Types + SQL are hoisted here so all three futures
    // can be built before the join. ---
    type FeedClaimRow = (
        uuid::Uuid,
        String,                        // claim text (title extracted)
        Option<String>,                // claim slug
        i32,                           // verify_count
        i32,                           // refute_count
        Option<String>,                // agent_backend
        Option<String>,                // agent_model
        Option<uuid::Uuid>,            // battle id (None = standalone)
        Option<String>,                // battle title
        Option<String>,                // battle slug
        Option<String>,                // author display_name
        f64,                           // effort temperature decayed to NOW()
        chrono::DateTime<chrono::Utc>, // claim created_at (age label)
        Option<i32>,                   // price_cents (author compute)
        Option<String>,                // topic (standalone claims)
        // `{topic_tags, produced_by}` packed into one JSONB value so the
        // feed row stays a 16-element tuple (sqlx's `FromRow` for tuples
        // caps at 16). Unpacked in-app below — mirrors the battle feed.
        serde_json::Value, // topic_tags + produced_by
    );
    const VERIF_PREFILTER: i64 = 200;
    type VerificationFeedRow = (
        uuid::Uuid,                    // verification id (stable tie-break)
        String,                        // verdict
        Option<String>,                // failure_reason
        Option<String>,                // environment
        String,                        // claim text (title extracted)
        Option<String>,                // claim slug
        uuid::Uuid,                    // claim id (href fallback)
        Option<String>,                // battle title
        Option<String>,                // battle slug
        Option<uuid::Uuid>,            // battle id (badge fallback)
        Option<String>,                // author display_name
        Option<String>,                // agent_backend
        Option<String>,                // agent_model
        chrono::DateTime<chrono::Utc>, // verification created_at
    );

    let battles_order = if active_sort == "new" {
        "h.updated_at DESC, h.id DESC"
    } else {
        "effort_decayed DESC, h.updated_at DESC, h.id DESC"
    };
    let claims_order = if active_sort == "new" {
        "c.created_at DESC, c.id DESC"
    } else {
        "effort_decayed DESC, c.created_at DESC, c.id DESC"
    };

    // Battles: effort temperature computed LIVE (0.20.0) — SUM over a
    // battle's claims of `(price_cents/100) * exp(-Δt/τ)` (τ ≈ 124766s).
    // Superseded filter in the LEFT JOIN so an edited claim's old version
    // drops out of the rollup (alias `s`).
    let battles_sql = format!(
        r#"
        SELECT
            h.id, h.title, h.description, h.status, h.domain_tags,
            MAX(s.created_at) AS last_claim_at,
            COUNT(s.id) as claim_count,
            COALESCE(SUM(s.verify_count), 0) as total_verify,
            COALESCE(SUM(s.refute_count), 0) as total_refute,
            e.domain as sponsor_domain,
            h.temperature,
            h.slug,
            COALESCE(SUM(
                (COALESCE(s.price_cents, 0) / 100.0)
                * exp(-GREATEST(0, EXTRACT(EPOCH FROM (NOW() - s.created_at)))
                      / 124766.0)
            ), 0.0)::float8 AS effort_decayed,
            h.short_summary,
            author.display_name as author_display_name,
            -- `solutions` + `presentation_config` packed into one JSONB
            -- value so the feed row stays a 16-element tuple (sqlx's
            -- `FromRow` for tuples caps at 16). Unpacked in-app below to
            -- build each card's mini Claims-Wins matrix (0.28.7).
            -- `created_epoch` rides along for the card's "opened" time.
            jsonb_build_object(
                'solutions', h.solutions,
                'presentation_config', h.presentation_config,
                'produced_by', h.produced_by,
                'created_epoch', EXTRACT(EPOCH FROM h.created_at)::float8
            ) AS solutions_and_config
        FROM battles h
        LEFT JOIN claims s ON s.battle_id = h.id AND s.deleted_at IS NULL {superseded}
        LEFT JOIN entities e ON e.id = h.sponsored_by
        LEFT JOIN users author ON author.id = h.author_id
        WHERE h.deleted_at IS NULL
          AND ($1::text IS NULL OR h.status = $1)
          AND ($2::text IS NULL OR $2 = ANY(h.domain_tags))
          AND (cardinality($3::text[]) = 0 OR NOT EXISTS (
                SELECT 1 FROM unnest($3::text[]) ft
                WHERE NOT EXISTS (
                    SELECT 1 FROM unnest(h.domain_tags) dt WHERE lower(dt) = ft)))
          AND (cardinality($4::text[]) = 0 OR NOT EXISTS (
                SELECT 1 FROM unnest($4::text[]) kw
                WHERE NOT (COALESCE(h.title, '') ILIKE kw ESCAPE '\'
                        OR COALESCE(h.description, '') ILIKE kw ESCAPE '\'
                        OR COALESCE(h.short_summary, '') ILIKE kw ESCAPE '\')))
        GROUP BY h.id, e.domain, author.display_name
        ORDER BY {order_by}
        LIMIT $5 OFFSET $6
        "#,
        order_by = battles_order,
        superseded = crate::models::claim::not_superseded("s")
    );
    // Claims: lineage HEADs only; each tag must match the claim's
    // topic_tags OR its parent Battle's domain_tags (alias `c`).
    let claims_sql = format!(
        r#"
        SELECT c.id, c.claim, c.slug, c.verify_count, c.refute_count,
               c.agent_backend, c.agent_model,
               c.battle_id,
               h.title AS battle_title, h.slug AS battle_slug,
               author.display_name AS author_display_name,
               ((COALESCE(c.price_cents, 0) / 100.0)
                * exp(-GREATEST(0, EXTRACT(EPOCH FROM (NOW() - c.created_at)))
                      / 124766.0))::float8 AS effort_decayed,
               c.created_at,
               c.price_cents,
               c.topic,
               jsonb_build_object(
                   'topic_tags', COALESCE(c.topic_tags, '{{}}'::text[]),
                   'produced_by', COALESCE(c.produced_by, '{{}}'::text[])
               ) AS topic_and_producer
        FROM claims c
        LEFT JOIN battles h ON h.id = c.battle_id
        LEFT JOIN users author ON author.id = c.author_id
        WHERE (c.visibility IS NULL OR c.visibility = 'public')
          AND c.deleted_at IS NULL
          {superseded}
          AND (c.battle_id IS NULL OR h.deleted_at IS NULL)
          AND ($1::text IS NULL OR h.status = $1)
          AND ($2::text IS NULL OR $2 = ANY(h.domain_tags))
          AND (cardinality($3::text[]) = 0 OR NOT EXISTS (
                SELECT 1 FROM unnest($3::text[]) ft
                WHERE NOT EXISTS (
                        SELECT 1 FROM unnest(COALESCE(c.topic_tags, '{{}}'::text[])) tt
                        WHERE lower(tt) = ft)
                  AND NOT EXISTS (
                        SELECT 1 FROM unnest(COALESCE(h.domain_tags, '{{}}'::text[])) dt
                        WHERE lower(dt) = ft)))
          AND (cardinality($4::text[]) = 0 OR NOT EXISTS (
                SELECT 1 FROM unnest($4::text[]) kw
                WHERE NOT (COALESCE(c.claim, '') ILIKE kw ESCAPE '\'
                        OR COALESCE(c.topic, '') ILIKE kw ESCAPE '\'
                        OR COALESCE(h.title, '') ILIKE kw ESCAPE '\')))
        ORDER BY {order_by}
        LIMIT $5 OFFSET $6
        "#,
        order_by = claims_order,
        superseded = crate::models::claim::not_superseded("c")
    );
    // Verifications: most-recent N (index-backed), ranked by hotness in
    // the app below; each tag matches the claim's topic_tags OR battle
    // domain_tags.
    let verifs_sql = format!(
        r#"
            SELECT v.id AS verification_id,
                   v.verdict, v.failure_reason, v.environment,
                   c.claim, c.slug AS claim_slug, c.id AS claim_id,
                   h.title AS battle_title, h.slug AS battle_slug, h.id AS battle_id,
                   u.display_name AS author_display_name,
                   v.agent_backend, v.agent_model,
                   v.created_at
            FROM verifications v
            JOIN claims c ON c.id = v.claim_id AND c.deleted_at IS NULL {superseded}
            LEFT JOIN battles h ON h.id = c.battle_id AND h.deleted_at IS NULL
            LEFT JOIN users u ON u.id = v.author_id
            WHERE (c.visibility IS NULL OR c.visibility = 'public')
              AND (c.battle_id IS NULL OR h.id IS NOT NULL)
              AND (cardinality($1::text[]) = 0 OR NOT EXISTS (
                    SELECT 1 FROM unnest($1::text[]) ft
                    WHERE NOT EXISTS (
                            SELECT 1 FROM unnest(COALESCE(c.topic_tags, '{{}}'::text[])) tt
                            WHERE lower(tt) = ft)
                      AND NOT EXISTS (
                            SELECT 1 FROM unnest(COALESCE(h.domain_tags, '{{}}'::text[])) dt
                            WHERE lower(dt) = ft)))
              AND (cardinality($2::text[]) = 0 OR NOT EXISTS (
                    SELECT 1 FROM unnest($2::text[]) kw
                    WHERE NOT (COALESCE(c.claim, '') ILIKE kw ESCAPE '\'
                            OR COALESCE(h.title, '') ILIKE kw ESCAPE '\')))
            ORDER BY v.created_at DESC, v.id DESC
            LIMIT $3
            "#,
        superseded = crate::models::claim::not_superseded("c")
    );

    let theme_q = query.theme.as_deref().map(|t| format!("theme={t}"));
    let theme = crate::web::meta::theme_from_request(&headers, theme_q.as_deref());

    // --- Arena-only data fetches. The classic theme doesn't render
    // these sections, so we skip the queries entirely when theme is
    // not "arena" to avoid burning roundtrips on data the page won't
    // show. Each query is best-effort — failure logs and falls back
    // to an empty list rather than blocking the home page render. ---
    /// Tuple shape returned by the home page top-claims SQL — pulled
    /// out so clippy doesn't flag the inline declaration as
    /// `type_complexity`.
    type TopClaimRow = (
        uuid::Uuid,
        String,
        Option<String>,
        i32,
        i32,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        chrono::DateTime<chrono::Utc>,
    );

    // The arena/right-rail block (top claims, live activity, top agents)
    // renders in the sidebar — not a swapped panel — so skip it in
    // fragment mode.
    let sidebar_fut = async {
        if theme == "arena" && !partial_only {
            let now = chrono::Utc::now();

            // Three independent queries — fire them concurrently via
            // tokio::join! so the page pays one RTT, not three. Each
            // query hits a different table set and has no data
            // dependency on the others; the pool has 20 connections
            // so concurrency is cheap.
            // SQL hoisted to a `let` so the format!-built String outlives
            // the future that borrows it across the `tokio::join!` below.
            let top_claims_sql = format!(
                r#"
            SELECT c.id, c.claim, c.slug, c.verify_count, c.refute_count,
                   c.agent_backend, c.agent_model, h.title, h.slug, c.created_at
            FROM claims c
            JOIN battles h ON h.id = c.battle_id
            WHERE (c.visibility IS NULL OR c.visibility = 'public')
              AND c.deleted_at IS NULL AND h.deleted_at IS NULL
              {superseded}
            ORDER BY (c.verify_count + c.refute_count) DESC, c.created_at DESC
            LIMIT 6
            "#,
                superseded = crate::models::claim::not_superseded("c")
            );
            let top_claims_fut =
                sqlx::query_as::<_, TopClaimRow>(&top_claims_sql).fetch_all(&state.pool);

            // SQL hoisted to a `let` (same lifetime reason as top_claims).
            let activity_sql = format!(
                r#"
            SELECT kind, actor, headline, link, created_at FROM (
              SELECT 'claim' AS kind,
                     COALESCE(u.display_name, '') AS actor,
                     CASE WHEN c.claim ~ '^# ' THEN regexp_replace(split_part(c.claim, E'\n', 1), '^# ', '')
                          ELSE split_part(c.claim, E'\n', 1) END AS headline,
                     '/claims/' || COALESCE(c.slug, c.id::text) AS link,
                     c.created_at
              FROM claims c
              LEFT JOIN users u ON u.id = c.author_id
              WHERE (c.visibility IS NULL OR c.visibility = 'public')
                AND c.deleted_at IS NULL
                {superseded}
              UNION ALL
              -- Verify / refute events: surface the claim title instead of
              -- a generic "Claim verified" string. The activity-row
              -- template wraps the whole row in an <a href="link">, so
              -- clicking takes the reader straight to the claim.
              SELECT CASE WHEN v.verdict = 'confirmed' THEN 'verify' ELSE 'refute' END,
                     COALESCE(u.display_name, ''),
                     CASE WHEN c.claim ~ '^# '
                          THEN regexp_replace(split_part(c.claim, E'\n', 1), '^# ', '')
                          ELSE split_part(c.claim, E'\n', 1) END,
                     '/claims/' || COALESCE(c.slug, c.id::text),
                     v.created_at
              FROM verifications v
              JOIN claims c ON c.id = v.claim_id
              LEFT JOIN users u ON u.id = v.author_id
              WHERE c.deleted_at IS NULL
            ) feed
            ORDER BY created_at DESC
            LIMIT 10
            "#,
                superseded = crate::models::claim::not_superseded("c")
            );
            let activity_fut = sqlx::query_as::<
                _,
                (
                    String,
                    String,
                    String,
                    String,
                    chrono::DateTime<chrono::Utc>,
                ),
            >(&activity_sql)
            .fetch_all(&state.pool);

            // Same query as `/agents` but capped at 10. Sort = total `$ effort`
            // (author + KrabReview), so the home block surfaces who is
            // putting the most observable work in — same row shape as the
            // full leaderboard so the templates share columns.
            let top_agents_fut = cached_top_agents(&state, 10);

            let (top_claims_rows, activity_rows, top_agents) =
                tokio::join!(top_claims_fut, activity_fut, top_agents_fut);

            let top_claims_rows: Vec<TopClaimRow> = top_claims_rows.unwrap_or_default();
            let top_claims: Vec<HomeClaimCard> = top_claims_rows
                .into_iter()
                .map(
                    |(
                        id,
                        claim,
                        slug,
                        verify,
                        refute,
                        backend,
                        model,
                        btitle,
                        bslug,
                        created_at,
                    )| {
                        let (title, _) = ClaimCardView::extract_title_body(&claim);
                        HomeClaimCard {
                            id: slug.unwrap_or_else(|| id.to_string()),
                            claim_title: title,
                            battle_title: btitle,
                            battle_slug: bslug.unwrap_or_default(),
                            agent_backend: backend.unwrap_or_default(),
                            agent_model: model.unwrap_or_default(),
                            verify_count: verify,
                            refute_count: refute,
                            age_label: relative_age(now, created_at),
                        }
                    },
                )
                .collect();

            let activity_rows: Vec<(
                String,
                String,
                String,
                String,
                chrono::DateTime<chrono::Utc>,
            )> = activity_rows.unwrap_or_default();
            let activity: Vec<HomeActivityEntry> = activity_rows
                .into_iter()
                .map(
                    |(kind, actor, headline, link, created_at)| HomeActivityEntry {
                        kind: match kind.as_str() {
                            "verify" => "verify",
                            "refute" => "refute",
                            _ => "claim",
                        },
                        actor,
                        headline,
                        link,
                        age_label: relative_age(now, created_at),
                    },
                )
                .collect();

            (top_claims, activity, top_agents)
        } else {
            (vec![], vec![], vec![])
        }
    };
    // Top-3 community-proposed Battle candidates by 7-day upvotes
    // for the inline home block (added in 0.9.3). Failures degrade
    // to zero so a regression here can't take down the home page.
    // Server-side renders with `viewer = None` because the voter
    // session is in localStorage, not a cookie; client-side JS
    // refreshes the active vote state after load.
    // Two independent candidate queries — fire concurrently so the inline
    // "Proposed battles" block costs one RTT, not two.
    let candidates_fut = async {
        if active_tab == "battles" && active_page == 1 {
            let (top_candidates, total_candidates) = tokio::join!(
                crate::services::battle_candidates_service::list_candidates(
                    &state.pool,
                    None,
                    3,
                    0,
                    Some(HOME_CANDIDATES_WINDOW_DAYS),
                ),
                crate::services::battle_candidates_service::count_open(
                    &state.pool,
                    Some(HOME_CANDIDATES_WINDOW_DAYS),
                ),
            );
            (
                top_candidates.unwrap_or_default(),
                total_candidates.unwrap_or(0),
            )
        } else {
            (vec![], 0)
        }
    };
    // Hero counters — combined into ONE round-trip via scalar
    // subqueries. The three counts hit different tables so the
    // planner runs them in parallel inside Postgres; client-side we
    // only pay a single RTT instead of three. Falls back to (0,0,0)
    // on error.
    // Hero "claims" counter = lineage HEADs only (edited claims don't
    // inflate the total by counting both versions).
    let hero_superseded = crate::models::claim::not_superseded("c");
    // Hero counters feed the hero band (not a swapped panel) — skip in
    // fragment mode.
    let hero_fut = async {
        if partial_only {
            (0, 0, 0)
        } else {
            sqlx::query_as::<_, (i64, i64, i64)>(&format!(
            "SELECT \
               (SELECT COUNT(*) FROM battles WHERE deleted_at IS NULL)::bigint, \
               (SELECT COUNT(*) FROM claims c WHERE c.deleted_at IS NULL {hero_superseded})::bigint, \
               (SELECT COUNT(*) FROM verifications WHERE created_at >= NOW() - INTERVAL '24 hours')::bigint"
        ))
        .fetch_one(&state.pool)
        .await
        .unwrap_or((0, 0, 0))
        }
    };
    let agent_header_fut = async {
        if partial_only || !wants_markdown(&headers) {
            String::new()
        } else {
            crate::services::cms_service::render_header(
                &state.pool,
                &crate::services::cms_service::PageContext::Home,
            )
            .await
        }
    };
    let need_claims = active_tab == "claims" || (!partial_only && wants_markdown(&headers));
    let vendor_fut = async {
        if !need_claims {
            return Vec::new();
        }
        crate::services::solution_names::load_vendor_patterns(&state.pool)
            .await
            .unwrap_or_default()
    };
    let feed_fut = async {
        #[allow(clippy::type_complexity)]
        let feed: (
            Result<
                Vec<(
                    uuid::Uuid,
                    String,
                    String,
                    String,
                    Vec<String>,
                    Option<chrono::DateTime<chrono::Utc>>,
                    i64,
                    i64,
                    i64,
                    Option<String>,
                    f32,
                    Option<String>,
                    f64,
                    Option<String>,
                    Option<String>,
                    serde_json::Value, // {solutions, presentation_config}
                )>,
                sqlx::Error,
            >,
            Result<Vec<FeedClaimRow>, sqlx::Error>,
            Result<Vec<VerificationFeedRow>, sqlx::Error>,
        ) = tokio::join!(
            sqlx::query_as(&battles_sql)
                .bind(normalized_status.as_deref())
                .bind(query.domain.as_deref())
                .bind(&tags)
                .bind(&q_patterns)
                .bind(feed_fetch_limit)
                .bind(feed_offset)
                .fetch_all(&state.pool),
            async {
                if !need_claims {
                    return Ok(Vec::new());
                }
                sqlx::query_as::<_, FeedClaimRow>(&claims_sql)
                    .bind(None::<&str>)
                    .bind(query.domain.as_deref())
                    .bind(&tags)
                    .bind(&q_patterns)
                    .bind(feed_fetch_limit)
                    .bind(feed_offset)
                    .fetch_all(&state.pool)
                    .await
            },
            async {
                if active_tab != "verifications" {
                    return Ok(Vec::new());
                }
                sqlx::query_as::<_, VerificationFeedRow>(&verifs_sql)
                    .bind(&tags)
                    .bind(&q_patterns)
                    .bind((feed_offset + feed_fetch_limit).max(VERIF_PREFILTER))
                    .fetch_all(&state.pool)
                    .await
            },
        );
        feed
    };
    let (
        (rows, claim_rows, verif_rows),
        (top_claims, activity, top_agents),
        (top_candidates, total_candidates),
        (total_battles, total_claims, total_verifications_today),
        agent_header,
        feed_vendor_patterns,
    ) = tokio::join!(
        crate::server_timing::timed("feed", feed_fut),
        crate::server_timing::timed("sidebar", sidebar_fut),
        crate::server_timing::timed("candidates", candidates_fut),
        crate::server_timing::timed("hero", hero_fut),
        crate::server_timing::timed("cms", agent_header_fut),
        crate::server_timing::timed("vendors", vendor_fut),
    );
    let battles_has_more = rows
        .as_ref()
        .map(|rows| rows.len() as i64 > HOME_FEED_PAGE_SIZE)
        .unwrap_or(false);
    // Log (don't swallow) decode/query errors: a silent `unwrap_or_default`
    // here is exactly how the verifications tab shipped as a permanently
    // empty feed (SELECT column order drifted from the row tuple).
    if let Err(e) = &claim_rows {
        tracing::error!(error = %e, "home feed: claims query failed");
    }
    if let Err(e) = &verif_rows {
        tracing::error!(error = %e, "home feed: verifications query failed");
    }
    let mut claim_rows: Vec<FeedClaimRow> = claim_rows.unwrap_or_default();
    let claims_has_more = claim_rows.len() as i64 > HOME_FEED_PAGE_SIZE;
    claim_rows.truncate(HOME_FEED_PAGE_SIZE as usize);
    let verif_rows: Vec<VerificationFeedRow> = verif_rows.unwrap_or_default();

    // Render time for the per-claim age labels in the unified feed.
    // (The effort temperature itself is decayed to NOW() in SQL.)
    let feed_now = chrono::Utc::now();

    let feed_battle_rows = match &rows {
        Ok(r) => &r[..r.len().min(HOME_FEED_PAGE_SIZE as usize)],
        Err(_) => &[][..],
    };
    let feed_battle_ids: Vec<Uuid> = feed_battle_rows.iter().map(|r| r.0).collect();
    let feed_stored_temps: std::collections::HashMap<Uuid, f32> =
        feed_battle_rows.iter().map(|r| (r.0, r.10)).collect();
    let feed_cost_ids: Vec<Uuid> = feed_battle_rows
        .iter()
        .filter(|r| r.6 > 0)
        .map(|r| r.0)
        .collect();
    let feed_claim_ids: Vec<Uuid> = claim_rows.iter().map(|r| r.0).collect();
    let matrix_claims_sql = format!(
        "SELECT {claim_cols}, u.display_name AS author_display_name, \
                u.avatar_url AS author_avatar_url \
         FROM claims c LEFT JOIN users u ON u.id = c.author_id \
         WHERE c.battle_id = ANY($1::uuid[]) AND c.deleted_at IS NULL {superseded} \
           AND c.visibility IS DISTINCT FROM 'private' \
         ORDER BY c.updated_at DESC, c.created_at DESC",
        claim_cols = crate::models::claim::claim_columns_for("c"),
        superseded = crate::models::claim::not_superseded("c")
    );
    let (live_temps, cost_agg, matrix_claims, claim_structures) = tokio::join!(
        crate::server_timing::timed("temps", async {
            crate::services::temperature::live_temperatures(
                &state.pool,
                &feed_battle_ids,
                &feed_stored_temps,
                feed_now,
            )
            .await
            .unwrap_or_default()
        }),
        crate::server_timing::timed("cost", battle_cost_aggregates(&state.pool, &feed_cost_ids)),
        crate::server_timing::timed("matrix", async {
            if feed_cost_ids.is_empty() {
                Vec::new()
            } else {
                sqlx::query_as::<_, ClaimWithAuthor>(&matrix_claims_sql)
                    .bind(&feed_cost_ids)
                    .fetch_all(&state.pool)
                    .await
                    .unwrap_or_default()
            }
        }),
        crate::server_timing::timed("structures", async {
            if feed_claim_ids.is_empty() {
                std::collections::HashMap::new()
            } else {
                sqlx::query_as::<_, (Uuid, serde_json::Value)>(
                    "SELECT id, claim_structure FROM claims \
                     WHERE id = ANY($1) AND claim_structure IS NOT NULL",
                )
                .bind(&feed_claim_ids)
                .fetch_all(&state.pool)
                .await
                .unwrap_or_default()
                .into_iter()
                .collect::<std::collections::HashMap<Uuid, serde_json::Value>>()
            }
        }),
    );

    // `battle_temps[i]` is the live-computed effort temperature of
    // `battles[i]` (SUM of its claims' decayed author-compute dollars) —
    // the sort key when merging into the unified feed.
    let mut battle_temps: Vec<f64> = Vec::new();
    let battles: Vec<BattleCard> = match rows {
        Ok(mut rows) => {
            rows.truncate(HOME_FEED_PAGE_SIZE as usize);
            // `effort_decayed` (computed live in the SQL above) is the
            // feed SORT key. The 🧊/🔥 intensity chip is a separate
            // signal — and (since 0.21.2) it is computed LIVE here too,
            // via the same `temperature::live_temperature` the battle
            // detail page uses, so a battle's chip can't read Cold in the
            // feed while reading Warm on its own page. The stored
            // `battles.temperature` is passed only as the per-battle
            // fallback for battles that have no events yet.
            let mut cards = Vec::with_capacity(rows.len());
            // Per-card (id, solutions, presentation_config) kept aside so
            // the batched mini-matrix pass below can rebuild the SAME
            // `WinsMatrix` the battle detail page renders without a
            // second `solutions`/`presentation_config` fetch (0.28.7).
            let mut card_matrix_inputs: Vec<(
                Uuid,
                Vec<crate::models::battle::BattleSolution>,
                Option<serde_json::Value>,
            )> = Vec::with_capacity(rows.len());
            for (
                id,
                title,
                description,
                status,
                tags,
                last_claim_at,
                count,
                verify,
                refute,
                sponsor,
                _stored_temp,
                slug,
                effort_decayed,
                short_summary,
                author_name,
                mut solutions_and_config,
            ) in rows
            {
                battle_temps.push(effort_decayed);
                // Unpack the packed `{solutions, presentation_config}`
                // JSON. `solutions` keeps the historical `parse_solutions`
                // shape; `presentation_config` is `None` when the column
                // was NULL (JSON null) so `PresentationConfig::default()`
                // applies, matching the detail page.
                let presentation_config = solutions_and_config
                    .get_mut("presentation_config")
                    .map(serde_json::Value::take)
                    .filter(|v| !v.is_null());
                let solutions_raw = solutions_and_config
                    .get_mut("solutions")
                    .map(serde_json::Value::take)
                    .unwrap_or(serde_json::Value::Null);
                let produced_by: Vec<String> = solutions_and_config
                    .get("produced_by")
                    .and_then(serde_json::Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let created_at = solutions_and_config
                    .get("created_epoch")
                    .and_then(serde_json::Value::as_f64)
                    .and_then(|secs| chrono::DateTime::from_timestamp(secs as i64, 0));
                let feed_time = created_at
                    .map(|created| FeedTime::for_battle(feed_now, created, last_claim_at));
                let solutions = crate::models::battle::parse_solutions(&solutions_raw);
                card_matrix_inputs.push((id, solutions.clone(), presentation_config));
                cards.push(
                    BattleCard::build(
                        id,
                        title,
                        description,
                        status,
                        tags,
                        count,
                        verify,
                        refute,
                        sponsor,
                    )
                    .with_temperature(live_temps.get(&id).copied().unwrap_or(0.0))
                    .with_short_summary(short_summary)
                    .with_top_leaderboard_logos(&solutions)
                    .with_slug(slug)
                    .with_author(author_name)
                    .with_top_logos(&solutions)
                    .with_produced_by(&produced_by)
                    .with_feed_time(feed_time),
                );
            }
            // Single round-trip cost aggregator across every battle
            // we're about to render. Skipping cards with `claim_count
            // == 0` — no claims means tokens / $ are both 0 and the
            // template suppresses the chips anyway, so the SQL path
            // doesn't waste rows on them.
            for card in cards.iter_mut() {
                if let Ok(uuid) = Uuid::parse_str(&card.id) {
                    if let Some(agg) = cost_agg.get(&uuid) {
                        card.apply_cost_agg(agg);
                    }
                }
            }

            // Mini Claims-Wins matrix per card (0.28.7) — the SAME real
            // standings the battle detail page shows, trimmed to top-3
            // solutions × top-3 benchmarks. ONE batched claim query for
            // ALL listed battles (`battle_id = ANY($1::uuid[])`), then
            // group by `battle_id` in-app and build each battle's matrix
            // from its slice — never N per-battle queries. Reuses the
            // claim SELECT shape from `battle_wins_matrix` /
            // `battle_detail`'s claim load.
            if !matrix_claims.is_empty() {
                let all_claims = matrix_claims;
                // Group claims by parent battle id (preserving the
                // newest-first order the query returns).
                let mut by_battle: std::collections::HashMap<Uuid, Vec<ClaimWithAuthor>> =
                    std::collections::HashMap::new();
                for c in all_claims {
                    if let Some(bid) = c.row.battle_id {
                        by_battle.entry(bid).or_default().push(c);
                    }
                }
                let inputs: std::collections::HashMap<
                    Uuid,
                    (
                        Vec<crate::models::battle::BattleSolution>,
                        Option<serde_json::Value>,
                    ),
                > = card_matrix_inputs
                    .into_iter()
                    .map(|(id, sols, cfg)| (id, (sols, cfg)))
                    .collect();
                for card in cards.iter_mut() {
                    let Ok(uuid) = Uuid::parse_str(&card.id) else {
                        continue;
                    };
                    let Some(claims) = by_battle.get(&uuid) else {
                        continue;
                    };
                    let Some((solutions, cfg)) = inputs.get(&uuid) else {
                        continue;
                    };
                    let matrix = wins_matrix_from_claims(claims, solutions, cfg.as_ref());
                    if let Some(mini) = mini_wins_matrix(&matrix, 3, 3) {
                        card.short_summary_view.mini_matrix = Some(mini);
                    }
                }
            }
            cards
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to load battle list for index page");
            vec![]
        }
    };

    // --- Build the unified Battle + Claim feed, ranked by render-time
    // effort temperature (0.20.0). Battles and Claims live on separate
    // tables and were each pre-narrowed to their hottest ~50 by the
    // queries above; here we decay both halves to `feed_now`, tag each
    // with its variant, merge, and sort descending. The merge is O(n
    // log n) over ≤100 already-loaded items — no extra DB round-trips.
    // We cap the rendered feed at 50 so the page weight stays bounded
    // regardless of corpus size. ---
    // `claim_cards` powers the Claims tab (discovery redesign, 0.23.0);
    // the merged `feed` is kept only to feed the `/` markdown digest,
    // which still shows the unified hottest list. We build each
    // FeedClaimCard once and clone it into both.
    // Rich decision-card data for the claims feed: the 16-col feed tuple
    // can't carry `claim_structure`, so batch-fetch it for the page's
    // claims and load vendor-logo patterns once. Lets home claim cards
    // render verdict + mini-leaderboard + favicons like battle-page cards.

    let mut feed: Vec<FeedItem> = Vec::with_capacity(battles.len() + claim_rows.len());
    let mut claim_cards: Vec<FeedClaimCard> = Vec::with_capacity(claim_rows.len());
    for (card, temp) in battles.iter().cloned().zip(battle_temps.iter().copied()) {
        feed.push(FeedItem::Battle {
            temperature: temp,
            card: Box::new(card),
        });
    }
    for (
        id,
        claim_text,
        slug,
        verify,
        refute,
        backend,
        model,
        battle_id,
        btitle,
        bslug,
        author_name,
        effort_decayed,
        created_at,
        price_cents,
        topic,
        topic_and_producer,
    ) in claim_rows
    {
        let temp = effort_decayed;
        // Unpack the packed `{topic_tags, produced_by}` JSON (mirrors the
        // battle feed). Both default to empty when absent / malformed.
        let json_str_array = |key: &str| -> Vec<String> {
            topic_and_producer
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let topic_tags = json_str_array("topic_tags");
        let produced_by = crate::models::producer::resolve_all(&json_str_array("produced_by"));
        let (title, _) = ClaimCardView::extract_title_body(&claim_text);
        // Author-compute dollars — the same signal that feeds the
        // temperature, shown as the $ spent on this claim (NOT decayed —
        // the chip reports real spend, the sort key reports
        // recency-weighted spend).
        let cost_usd = price_cents.unwrap_or(0).max(0) as f64 / 100.0;
        let cost_label = if cost_usd > 0.0 {
            format!("${cost_usd:.2}")
        } else {
            String::new()
        };
        // Bug #2 fix: resolve a badge target so a battle-attached claim
        // with a NULL-slug Battle no longer renders badge-less (looking
        // standalone). See `resolve_claim_battle_badge`.
        let battle_slug = resolve_claim_battle_badge(battle_id, bslug);
        // Parse this claim's structure into a benchmark label + top-3
        // leaderboard (with fallback favicons) so the card reads like a
        // battle-page decision card, not a bare title.
        let structure_raw = claim_structures.get(&id);
        let winners: Vec<String> = structure_raw
            .and_then(|v| {
                serde_json::from_value::<krabarena_shared::claim_structure::ClaimStructure>(
                    v.clone(),
                )
                .ok()
            })
            .map(|s| s.winners)
            .unwrap_or_default();
        let (benchmark_opt, mut lb_rows, _lb_cost) =
            ClaimCardView::project_structure(structure_raw, &winners);
        lb_rows.truncate(3);
        apply_vendor_logo_fallback(&feed_vendor_patterns, &mut lb_rows);
        let (benchmark_label, benchmark_metric) = match benchmark_opt {
            Some(b) => (
                b.short_label.clone().unwrap_or_else(|| b.name.clone()),
                b.metric.clone(),
            ),
            None => (String::new(), String::new()),
        };
        let card = FeedClaimCard {
            id: slug.unwrap_or_else(|| id.to_string()),
            claim_title: title,
            battle_title: btitle.unwrap_or_default(),
            battle_slug,
            author_display_name: author_name,
            agent_backend: backend.unwrap_or_default(),
            agent_model: model.unwrap_or_default(),
            verify_count: verify,
            refute_count: refute,
            cost_label,
            time: FeedTime::new("posted ", feed_now, created_at),
            topic: topic.unwrap_or_default(),
            topic_tags,
            produced_by,
            benchmark_label,
            benchmark_metric,
            leaderboard: lb_rows,
        };
        claim_cards.push(card.clone());
        feed.push(FeedItem::Claim {
            temperature: temp,
            card: Box::new(card),
        });
    }
    // Descending by decayed temperature; partial_cmp is safe (no NaN —
    // decay_effort returns finite for finite inputs). Ties keep insert
    // order (battles before claims) which is stable enough for the UI.
    feed.sort_by(|a, b| {
        b.temperature()
            .partial_cmp(&a.temperature())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    feed.truncate(50);
    // `claim_cards` is already in the requested SQL order, so the
    // Claims tab inherits the ranking without a second sort.
    claim_cards.truncate(HOME_FEED_PAGE_SIZE as usize);

    // --- Verifications tab (discovery redesign, 0.23.0). Fetched on
    // every home load (all three tab panels render up-front so the
    // client-side tab switch is instant). Ranked by "hotness": recency
    // weighted by verdict, with refutes counting 5× a verify (same
    // refute-bias as the search hotness signal), decayed with the shared
    // 24h effort half-life so a fresh refute outranks a week-old verify.
    //
    // The decay expression is non-sargable, so we DON'T order by it in
    // SQL (that's a full scan + sort every load). Instead the DB serves
    // a cheap, index-backed recency cut — `ORDER BY created_at DESC LIMIT
    // VERIF_PREFILTER` (idx_verifications_created_at, migration 054) —
    // and we compute the hotness weight and final ranking in the app via
    // `temperature::decay_effort` (the same canonical decay the feed
    // uses). Pulling far more rows than we render (200 vs 50) gives ample
    // headroom: recency dominates the decay and the refute bias is only
    // 5×, so the 200 most-recent rows safely contain the top 50 by
    // hotness for any realistic distribution. Each card deep-links to its
    // Claim. Verification text itself is NOT embedding-indexed (kept
    // nested); this is a DB-backed activity list, not a search surface.
    // The query itself ran concurrently with battles + claims above. ---
    let (verification_cards, verifications_has_more): (Vec<VerificationCard>, bool) = {
        let rows = verif_rows;

        // Rank the recency-pre-filtered rows by hotness in the app:
        // `verdict_weight × exp(-Δt/τ)` via the canonical `decay_effort`
        // (refute = 5×, verify = 1×), then keep the top 50.
        let mut ranked: Vec<(
            f64,
            chrono::DateTime<chrono::Utc>,
            uuid::Uuid,
            VerificationCard,
        )> = rows
            .into_iter()
            .map(
                |(
                    verification_id,
                    verdict,
                    failure_reason,
                    environment,
                    claim_text,
                    claim_slug,
                    claim_id,
                    btitle,
                    bslug,
                    battle_id,
                    author_name,
                    backend,
                    model,
                    created_at,
                )| {
                    let (title, _) = ClaimCardView::extract_title_body(&claim_text);
                    let is_confirmed = verdict == "confirmed";
                    let verdict_weight = if is_confirmed { 1.0 } else { 5.0 };
                    let hotness = crate::services::temperature::decay_effort(
                        verdict_weight,
                        created_at,
                        feed_now,
                    );
                    // Evidence excerpt: why-it-failed for a refute, the
                    // run environment for a verify. Trimmed to one line.
                    let raw_detail = if is_confirmed {
                        environment.unwrap_or_default()
                    } else {
                        failure_reason.unwrap_or_default()
                    };
                    let detail = crate::markdown::excerpt(&raw_detail, 180);
                    let battle_slug = resolve_claim_battle_badge(battle_id, bslug);
                    let card = VerificationCard {
                        claim_id: claim_slug.unwrap_or_else(|| claim_id.to_string()),
                        claim_title: title,
                        is_confirmed,
                        author_display_name: author_name,
                        agent_backend: backend.unwrap_or_default(),
                        agent_model: model.unwrap_or_default(),
                        detail,
                        battle_title: btitle.unwrap_or_default(),
                        battle_slug,
                        time: FeedTime::new("", feed_now, created_at),
                    };
                    (hotness, created_at, verification_id, card)
                },
            )
            .collect();
        if active_sort == "new" {
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
        } else {
            ranked.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.1.cmp(&a.1))
                    .then(b.2.cmp(&a.2))
            });
        }
        let has_more = ranked.len() as i64 > feed_offset + HOME_FEED_PAGE_SIZE;
        let cards = ranked
            .into_iter()
            .skip(feed_offset as usize)
            .take(HOME_FEED_PAGE_SIZE as usize)
            .map(|(_, _, _, card)| card)
            .collect();
        (cards, has_more)
    };

    let mut meta = PageMeta::new(&state.public_base_url, "/")
        .with_title("Know which tool wins for your job")
        .with_description(
            "Agents fight over the evidence — running, verifying, and refuting \
             claims — so you can decide with proof, not hype.",
        );
    if home_is_indexable_default_state(&query, &tags, &active_q) {
        meta = meta.with_json_ld(website_json_ld(&state.public_base_url));
    } else {
        meta = meta.noindex_follow();
    }
    // Aggregate domain-tag counts from the loaded battles so the
    // forum-style left sidebar's Topics block has live data. Stable
    // color pick via hash so the same tag always gets the same
    // dot color across renders. (since 0.9.0 forum redesign)
    let topic_counts = compute_topic_counts(&battles);

    // The markdown digest mirrors the same unified ranking — top 5
    // feed items (Battles + Claims interleaved by effort temperature),
    // so agents curling `/` see the same hot list as the HTML page.
    let md_feed: Vec<FeedItem> = feed.iter().take(5).cloned().collect();

    let active_has_more = match active_tab.as_str() {
        "claims" => claims_has_more,
        "verifications" => verifications_has_more,
        _ => battles_has_more,
    };
    let next_url = if active_has_more {
        home_url(
            &active_tab,
            normalized_status.as_deref(),
            &tags,
            &active_q,
            &active_sort,
            active_page + 1,
        )
    } else {
        String::new()
    };
    let active_tag_value = tags.join(",");
    let active_tags: Vec<ActiveTagPill> = tags
        .iter()
        .map(|tag| {
            let remaining: Vec<String> = tags.iter().filter(|t| *t != tag).cloned().collect();
            ActiveTagPill {
                tag: tag.clone(),
                remove_url: home_url(
                    &active_tab,
                    normalized_status.as_deref(),
                    &remaining,
                    &active_q,
                    &active_sort,
                    1,
                ),
            }
        })
        .collect();
    let feed_view = HomeFeedView {
        meta: meta.clone(),
        active_tab: active_tab.clone(),
        active_tags,
        active_tag_value: active_tag_value.clone(),
        active_q: active_q.clone(),
        current_page: active_page,
        battles: battles.clone(),
        claim_cards,
        verification_cards,
        active_status: normalized_status.clone(),
        has_more: active_has_more,
        next_url,
        tab_battles_url: home_url(
            "battles",
            normalized_status.as_deref(),
            &tags,
            &active_q,
            &active_sort,
            1,
        ),
        tab_claims_url: home_url(
            "claims",
            normalized_status.as_deref(),
            &tags,
            &active_q,
            &active_sort,
            1,
        ),
        tab_verifications_url: home_url(
            "verifications",
            normalized_status.as_deref(),
            &tags,
            &active_q,
            &active_sort,
            1,
        ),
        status_hot_url: home_url("battles", None, &tags, &active_q, &active_sort, 1),
        status_open_url: home_url("battles", Some("open"), &tags, &active_q, &active_sort, 1),
        status_active_url: home_url("battles", Some("active"), &tags, &active_q, &active_sort, 1),
        status_settled_url: home_url(
            "battles",
            Some("settled"),
            &tags,
            &active_q,
            &active_sort,
            1,
        ),
        hidden_tab: if active_tab == "battles" {
            String::new()
        } else {
            active_tab.clone()
        },
        hidden_status: normalized_status.clone().unwrap_or_default(),
        hidden_sort: if active_sort == "hot" {
            String::new()
        } else {
            active_sort.clone()
        },
        top_candidates,
        total_candidates,
    };
    let html = IndexTemplate {
        meta: meta.clone(),
        feed: feed_view.clone(),
        total_battles,
        total_claims,
        total_verifications_today,
        theme,
        top_claims,
        activity,
        top_agents,
        faq: krabarena_shared::about::FAQ,
        topic_counts,
    };
    // CMS header only feeds the markdown variant — the fragment swap is
    // always an HTML fetch, so skip the query in fragment mode.
    let md = IndexMdTemplate {
        meta,
        agent_header,
        feed: md_feed,
        active_status: normalized_status,
        total_battles,
        total_claims,
        total_verifications_today,
    };
    if hx_request {
        if active_page > 1 {
            return response_with_vary(
                HomeFeedPageTemplate { feed: feed_view },
                &state.public_base_url,
                "/",
                "home_feed_page.html",
                "HX-Request",
            );
        }
        return response_with_vary(
            HomeFeedShellTemplate { feed: feed_view },
            &state.public_base_url,
            "/",
            "home_feed_shell.html",
            "HX-Request",
        );
    }

    let render_started = std::time::Instant::now();
    let mut resp = render_negotiated(&state.public_base_url, "/", &headers, html, md);
    crate::server_timing::record("render", render_started);
    resp.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static("Accept, User-Agent, HX-Request"),
    );
    resp
}

pub async fn home_tag_suggestions(
    State(state): State<AppState>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let query = parse_home_query(raw_query.as_deref());
    let active_tab = normalize_home_tab(query.tab.as_deref()).to_string();
    let active_sort = normalize_home_sort(query.sort.as_deref()).to_string();
    let normalized_status = query.status.as_deref().map(|s| {
        krabarena_shared::domain::BattleStatus::parse_lenient(s)
            .map(|st| st.to_string())
            .unwrap_or_else(|| s.to_string())
    });
    let tags = query_tags(&query);
    let active_q = query_q(&query);
    let prefix = suggestion_prefix(&active_q);
    let remaining_q = query_without_last_token(&active_q);

    let rows = crate::services::tag_service::list_tags(&state.pool, &prefix, 8)
        .await
        .unwrap_or_default();
    let suggestions = rows
        .into_iter()
        .filter(|row| !tags.contains(&row.tag))
        .map(|row| {
            let mut next_tags = tags.clone();
            next_tags.push(row.tag.clone());
            HomeTagSuggestionView {
                add_url: home_url(
                    &active_tab,
                    normalized_status.as_deref(),
                    &next_tags,
                    &remaining_q,
                    &active_sort,
                    1,
                ),
                tag: row.tag,
                count: row.count,
            }
        })
        .collect();

    response_with_vary(
        HomeTagSuggestionsTemplate { suggestions },
        &state.public_base_url,
        "/fragments/home/tags",
        "home_tag_suggestions.html",
        "HX-Request",
    )
}

/// "2m ago", "3h ago", "5d ago", or an ISO date if older than 30 days.
/// Pull the first paragraph (the "headline opener" line) out of a
/// 0.8.21 `short_summary`. The prompt structures short summaries as:
///   `<opener line, ≤200 chars>\n\n### Top 3\n| ... |\n\n### Top Metrics\n...`
/// For OG / Twitter card descriptions we want JUST the opener — the
/// markdown tables and emoji notes line don't render in social
/// previews and would just be noise. Returns `None` if the input
/// is empty or starts with a markdown heading (n=1 fallback path
/// already gives us a clean opener; markdown-only input means the
/// summarizer didn't produce a usable headline).
fn extract_short_summary_headline(short_summary: &str) -> Option<String> {
    let opener = short_summary
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    if opener.starts_with('#') {
        return None;
    }
    Some(opener.to_string())
}

/// Parse the structured tables out of a `short_summary` markdown
/// blob. The summarizer prompt (battle_summary_prompt.txt) emits a
/// fixed shape since 0.8.21:
///   `<opener>\n\n### Top 3\n| Rank | Solution | Score |\n…\n\n`
///   `### Top Metrics\n| Eval | Claims |\n…\n\n`
///   `### Notes\n🏆 … · ⚡ … · …`
/// The home-page card renders these as panels matching the forum
/// reference design. Returns an empty `ShortSummaryView` when the
/// markdown lacks the headers (n=1 first-synthesis path, or pre-0.8.21
/// summaries) so the template falls back to the verify-bar-only layout.
fn parse_short_summary_panels(s: &str) -> ShortSummaryView {
    let mut out = ShortSummaryView::default();
    let mut section: &str = "intro";

    for raw in s.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        // Capture the first non-empty, non-heading line as the opener.
        if section == "intro" && out.opening_line.is_none() && !line.starts_with('#') {
            out.opening_line = Some(line.to_string());
        }

        // Section switch on `### …` headers.
        if let Some(rest) = line.strip_prefix("###") {
            let title = rest.trim().to_lowercase();
            section = if title.starts_with("top 3") || title.contains("leaderboard") {
                "leaderboard"
            } else if title.contains("top metric") {
                "metrics"
            } else if title.contains("note") {
                "notes"
            } else {
                "other"
            };
            continue;
        }

        match section {
            "leaderboard" | "metrics" => {
                // Skip table-divider rows like `|---|---|---|`.
                if line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
                    continue;
                }
                let cells: Vec<String> = line
                    .split('|')
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .map(str::to_string)
                    .collect();
                if cells.is_empty() {
                    continue;
                }
                if section == "leaderboard" {
                    if cells.len() < 3 {
                        continue;
                    }
                    let Ok(rank) = cells[0].parse::<u32>() else {
                        continue; // header row ("Rank") or stray text
                    };
                    let raw_sol = cells[1].as_str();
                    let stripped = raw_sol.trim_start_matches('*').trim_end_matches('*');
                    let is_winner = raw_sol.starts_with("**") || rank == 1;
                    // Strip redundant parenthetical from score cells, e.g.
                    // "100.0 (100.0%)" → "100.0", "84.2%" stays as-is.
                    let score_raw = cells[2].as_str();
                    let score = if let Some(idx) = score_raw.find(" (") {
                        score_raw[..idx].trim().to_string()
                    } else {
                        score_raw.to_string()
                    };
                    out.leaderboard.push(ShortSummaryLbRow {
                        rank,
                        solution: stripped.to_string(),
                        score,
                        is_winner,
                        logo_url: None,
                    });
                } else {
                    if cells.len() < 2 {
                        continue;
                    }
                    let Ok(claims) = cells[1].parse::<u32>() else {
                        continue; // header row ("Claims")
                    };
                    let claims_label = if claims == 1 {
                        "1 claim".to_string()
                    } else {
                        format!("{} claims", claims)
                    };
                    out.top_metrics.push(ShortSummaryMetric {
                        name: cells[0].clone(),
                        claims_label,
                    });
                }
            }
            "notes" => {
                for clause in line.split('·') {
                    let c = clause.trim();
                    if !c.is_empty() {
                        out.editor_notes.push(c.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    out
}

/// Aggregate every domain-tag across the loaded battles into a
/// sorted `(tag, count, color)` list for the forum redesign's left
/// Strip ` (…)` parenthetical content from cells inside markdown
/// table rows. The KrabReviewer prompt asks for `raw (normalized%)`
/// in score cells and verbose qualifiers like
/// `SimpleQA Recall (content-mode, %, higher=better)` in headers,
/// which forces wide columns. Removing the parens collapses cells
/// to just the headline value (`84.0`, `SimpleQA Recall`) so the
/// table fits the main column without horizontal scroll. Acts only
/// on lines starting with `|` so prose paragraphs keep their parens
/// intact. Falls back to the input on regex failure.
fn strip_table_cell_parens(markdown: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    // `\s*\(…anything except `)` or newline…\)`. Bounded so it can't
    // cross a row, and the `\s*` eats the leading space so we don't
    // leave double-spaces behind.
    let re =
        RE.get_or_init(|| regex::Regex::new(r"\s*\([^)\n]*\)").expect("static regex compiles"));
    markdown
        .lines()
        .map(|line| {
            if line.trim_start().starts_with('|') {
                re.replace_all(line, "").into_owned()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// sidebar Topics section. Color is a stable hash-pick from a
/// fixed palette so the dot next to each tag never flickers between
/// renders. Returns up to 12 — beyond that the sidebar gets noisy.
fn compute_topic_counts(battles: &[BattleCard]) -> Vec<TopicCount> {
    use std::collections::HashMap;
    const PALETTE: &[&str] = &[
        "#3a7da6", "#7a4d9e", "#c97e2a", "#2f8f4f", "#b7301a", "#4a90a8", "#a85b3a", "#5e7e3c",
    ];
    let mut counts: HashMap<String, usize> = HashMap::new();
    for b in battles {
        for tag in &b.domain_tags {
            *counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    let mut out: Vec<TopicCount> = counts
        .into_iter()
        .map(|(tag, count)| {
            // Cheap deterministic hash → palette index. djb2-ish.
            let idx: usize = tag.bytes().fold(5381usize, |h, b| {
                h.wrapping_mul(33).wrapping_add(b as usize)
            }) % PALETTE.len();
            TopicCount {
                tag,
                count,
                color: PALETTE[idx],
            }
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
    out.truncate(12);
    out
}

/// "just now", "2m ago", "3h ago", "5d ago" for the last week; beyond
/// 7 days a short date — "Nov 12" in the current year, "Nov 12, 2025"
/// otherwise. All in UTC (server-rendered, so no viewer timezone).
fn relative_age(now: chrono::DateTime<chrono::Utc>, then: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Datelike;
    let delta = (now - then).num_seconds().max(0);
    if delta < 60 {
        return "just now".into();
    }
    if delta < 3600 {
        return format!("{}m ago", delta / 60);
    }
    if delta < 86_400 {
        return format!("{}h ago", delta / 3600);
    }
    if delta < 7 * 86_400 {
        return format!("{}d ago", delta / 86_400);
    }
    if then.year() == now.year() {
        then.format("%b %-d").to_string()
    } else {
        then.format("%b %-d, %Y").to_string()
    }
}

/// A timestamp rendered on a home feed card as
/// `<time datetime="…" title="…">{prefix}{label}</time>`: relative /
/// short-date text from [`relative_age`], the machine-readable ISO 8601
/// instant, and the exact UTC date-time for the hover tooltip.
#[derive(Debug, Clone)]
pub struct FeedTime {
    /// Verb in front of the label ("opened ", "last claim ", "posted ").
    pub prefix: &'static str,
    pub label: String,
    /// RFC 3339 / ISO 8601, second precision, `Z` suffix.
    pub iso: String,
    /// Full UTC date-time for the `title=` tooltip.
    pub title: String,
}

impl FeedTime {
    pub fn new(
        prefix: &'static str,
        now: chrono::DateTime<chrono::Utc>,
        then: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            prefix,
            label: relative_age(now, then),
            iso: then.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            title: then.format("%Y-%m-%d %H:%M UTC").to_string(),
        }
    }

    /// Battle-card timestamp: "last claim 2h ago" when the battle has
    /// claims (latest claim's `created_at`), else "opened 3d ago"
    /// (battle `created_at`). The tooltip always carries the opened date.
    pub fn for_battle(
        now: chrono::DateTime<chrono::Utc>,
        created_at: chrono::DateTime<chrono::Utc>,
        last_claim_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Self {
        match last_claim_at {
            Some(last) => {
                let mut t = Self::new("last claim ", now, last);
                t.title = format!(
                    "Last claim {} · opened {}",
                    t.title,
                    created_at.format("%Y-%m-%d %H:%M UTC")
                );
                t
            }
            None => {
                let mut t = Self::new("opened ", now, created_at);
                t.title = format!("Opened {}", t.title);
                t
            }
        }
    }
}

/// Row shape for the battle-detail claim list: the full claim row
/// plus the author's display name from `users.display_name`.
/// `LEFT JOIN` so a claim whose author got deleted (FK violation
/// shouldn't happen, but the page should still render) still
/// produces a row with `author_display_name = NULL`.
///
/// Used in `battle_detail` for both the claim cards and the timeline
/// "Claim posted by X" entries — one query instead of two.
#[derive(sqlx::FromRow)]
pub(crate) struct ClaimWithAuthor {
    #[sqlx(flatten)]
    row: ClaimRow,
    author_display_name: Option<String>,
    author_avatar_url: Option<String>,
}

/// Row shape for the timeline's verdict entries. (Temperature events are
/// no longer derived from these — they come from the canonical
/// `temperature::fetch_events_for_battles` query so the chip matches the
/// feed exactly.)
type TimelineVerdictRow = (
    chrono::DateTime<chrono::Utc>,
    String,
    Option<String>,
    Option<String>,
    Uuid,
    Option<String>,
);

type TimelineSummaryRow = (chrono::DateTime<chrono::Utc>, Option<f32>);

struct TimelineClaim {
    id: Uuid,
    slug: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    author_display_name: Option<String>,
}

const TIMELINE_VISIBLE_ENTRIES: usize = 5;

const TIMELINE_VERDICTS_SQL: &str = "SELECT v.created_at, v.verdict, v.environment, \
            u.display_name, c.id, c.slug \
     FROM verifications v \
     JOIN claims c ON c.id = v.claim_id \
     JOIN users u ON u.id = v.author_id \
     WHERE c.battle_id = $1 AND c.deleted_at IS NULL";

const TIMELINE_SUMMARIES_SQL: &str =
    "SELECT created_at, temperature FROM battle_summary_history WHERE battle_id = $1";

pub async fn battle_detail(
    State(state): State<AppState>,
    Path(raw_id_or_slug): Path<String>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> impl IntoResponse {
    // `/battles/<slug>.md` → always the raw-markdown twin (AI crawlers
    // rarely send `Accept: text/markdown`; the `.md` URL is advertised
    // via `<link rel="alternate" type="text/markdown">`).
    let (id_or_slug, headers, md_suffix) = split_markdown_suffix(raw_id_or_slug, headers);
    let looked_up_by_uuid = id_or_slug.parse::<Uuid>().is_ok();
    // Explicit BATTLE_COLUMNS rather than `SELECT *` — see AGENTS.md
    // → "ALTER TABLE on prod" for the PgBouncer cached-plan trap that
    // bites every `SELECT *` after a migration adds a column.
    // `deleted_at IS NULL` — a soft-deleted battle 404s publicly (migration 053).
    let battle_select_by_id =
        format!("SELECT {BATTLE_COLUMNS} FROM battles WHERE id = $1 AND deleted_at IS NULL");
    let battle_select_by_slug =
        format!("SELECT {BATTLE_COLUMNS} FROM battles WHERE slug = $1 AND deleted_at IS NULL");
    let battle = if let Ok(uuid) = id_or_slug.parse::<Uuid>() {
        sqlx::query_as::<_, BattleRow>(&battle_select_by_id)
            .bind(uuid)
            .fetch_optional(&state.pool)
            .await
    } else {
        sqlx::query_as::<_, BattleRow>(&battle_select_by_slug)
            .bind(&id_or_slug)
            .fetch_optional(&state.pool)
            .await
    };

    let battle = match battle {
        Ok(Some(h)) => h,
        _ => {
            return crate::web::errors::render_404(
                &state.public_base_url,
                &format!("/battles/{id_or_slug}"),
            );
        }
    };

    // Canonicalise: if looked up by UUID but a slug exists, 301 to the slug URL.
    if looked_up_by_uuid {
        if let Some(slug) = battle.slug.as_deref() {
            let target = markdown_suffixed(slug, md_suffix);
            return canonical_slug_redirect("/battles", &target, raw_query.as_deref());
        }
    }

    let battle_id = battle.id;

    // Parse `proxy_spec` JSONB straight off the battle row so we don't
    // re-fetch it via `proxy_service::get_proxy_spec`. Falls back to
    // `None` when the column is missing/malformed — same as the old
    // `get_proxy_info(...).ok()` path returning `None`.
    let proxy_spec_parsed: Option<crate::models::proxy::ProxySpec> = battle
        .proxy_spec
        .clone()
        .and_then(|v| serde_json::from_value(v).ok());

    // Fire every remaining read in parallel — no data dependency
    // between them except the battle id, which we already have.
    //   - claim_rows + author_display_name (one query, used by both
    //     the claim cards and the timeline claim entries)
    //   - verdicts with environment/failure/conditions (used by both
    //     the timeline verdict entries and the temperature events)
    //   - battle_summary_history (timeline summary entries)
    //   - sponsor entity domain (None branch resolves immediately)
    //   - proxy service-counts (only if a spec exists)
    //   - cost aggregate (single round-trip)
    //   - CMS agent header (warm-cache => no DB call)
    //
    // Stats (claim count, total verify, total refute) are derived
    // in-memory from `claim_rows`, saving the standalone stats COUNT/SUM
    // query. Temperature events are NOT derived here — they come from the
    // canonical `temperature::fetch_events_for_battles` (one extra query,
    // issued concurrently in the join below) so the heat-pill is computed
    // from the identical event set as the feed/list cards and can't drift.
    // Battle-detail claims list (also the source for the wins matrix
    // + per-claim cards): lineage HEADs only — a superseded version
    // (the old version an edit replaced) drops out of the list and the
    // wins matrix, so its wins aren't double-counted. Still reachable
    // by its own URL. (Alias `c`.)
    // SQL hoisted to a `let` so the format!-built String outlives the
    // future that borrows it across the `tokio::join!` below.
    let claims_sql = format!(
        "SELECT {claim_cols}, u.display_name AS author_display_name, \
                u.avatar_url AS author_avatar_url \
         FROM claims c LEFT JOIN users u ON u.id = c.author_id \
         WHERE c.battle_id = $1 AND c.deleted_at IS NULL {superseded} \
           AND c.visibility IS DISTINCT FROM 'private' \
         ORDER BY c.updated_at DESC, c.created_at DESC, c.id DESC",
        claim_cols = crate::models::claim::claim_columns_for("c"),
        superseded = crate::models::claim::not_superseded("c")
    );
    let claims_fut = sqlx::query_as::<_, ClaimWithAuthor>(&claims_sql)
        .bind(battle_id)
        .fetch_all(&state.pool);

    let verdicts_fut = sqlx::query_as::<_, TimelineVerdictRow>(TIMELINE_VERDICTS_SQL)
        .bind(battle_id)
        .fetch_all(&state.pool);

    let summaries_fut = sqlx::query_as::<_, TimelineSummaryRow>(TIMELINE_SUMMARIES_SQL)
        .bind(battle_id)
        .fetch_all(&state.pool);

    let sponsor_id = battle.sponsored_by;
    let pool_for_sponsor = state.pool.clone();
    let sponsor_fut = async move {
        match sponsor_id {
            Some(sid) => {
                sqlx::query_scalar::<_, String>("SELECT domain FROM entities WHERE id = $1")
                    .bind(sid)
                    .fetch_optional(&pool_for_sponsor)
                    .await
                    .ok()
                    .flatten()
            }
            None => None,
        }
    };

    let proxy_spec_for_fut = proxy_spec_parsed.clone();
    let pool_for_proxy = state.pool.clone();
    let proxy_fut = async move {
        match proxy_spec_for_fut {
            Some(spec) => crate::services::proxy_service::get_proxy_info_with_spec(
                &pool_for_proxy,
                battle_id,
                &spec,
            )
            .await
            .ok(),
            None => None,
        }
    };

    let battle_ids_for_agg = [battle_id];
    let cost_agg_fut = battle_cost_aggregates(&state.pool, &battle_ids_for_agg);

    let agent_header_ctx = crate::services::cms_service::PageContext::Battle {
        battle_id: battle_id.to_string(),
    };
    let header_wanted = wants_markdown(&headers);
    let agent_header_fut = async {
        if header_wanted {
            crate::services::cms_service::render_header(&state.pool, &agent_header_ctx).await
        } else {
            String::new()
        }
    };

    let battle_author_id = battle.author_id;
    let pool_for_author = state.pool.clone();
    let battle_author_fut = async move {
        sqlx::query_scalar::<_, String>("SELECT display_name FROM users WHERE id = $1")
            .bind(battle_author_id)
            .fetch_optional(&pool_for_author)
            .await
            .ok()
            .flatten()
    };

    // Temperature events (chip + sparkline) from the canonical query,
    // run concurrently with the other detail-page fetches rather than
    // serialized after the join.
    let temp_events_fut =
        crate::services::temperature::fetch_events_for_battles(&state.pool, &battle_ids_for_agg);

    let (
        claims_with_author,
        verdict_rows,
        summary_rows,
        sponsor_domain,
        proxy_info,
        cost_agg,
        agent_header,
        battle_author_name,
        temp_events,
    ) = tokio::join!(
        crate::server_timing::timed("claims", claims_fut),
        crate::server_timing::timed("verdicts", verdicts_fut),
        crate::server_timing::timed("summaries", summaries_fut),
        sponsor_fut,
        crate::server_timing::timed("proxy", proxy_fut),
        crate::server_timing::timed("cost", cost_agg_fut),
        agent_header_fut,
        battle_author_fut,
        crate::server_timing::timed("temps", temp_events_fut),
    );
    let compute_started = std::time::Instant::now();

    let claims_with_author = claims_with_author.unwrap_or_default();
    let verdict_rows = verdict_rows.unwrap_or_default();
    let summary_rows = summary_rows.unwrap_or_default();
    let mut temp_events = temp_events.unwrap_or_default();

    // Stats derivation (replaces the old standalone COUNT/SUM query).
    let claim_count = claims_with_author.len() as i64;
    let total_verify: i64 = claims_with_author
        .iter()
        .map(|c| c.row.verify_count as i64)
        .sum();
    let total_refute: i64 = claims_with_author
        .iter()
        .map(|c| c.row.refute_count as i64)
        .sum();

    let exec_summary = battle.executive_summary.clone();
    // OG / Twitter card description priority (best preview text first):
    //   1. `og_summary` — dedicated, hand-curated (admin override path)
    //   2. headline opener of `short_summary` — auto-generated since
    //      0.8.21, bounded to ~200 chars by the prompt and exactly the
    //      "winner + most striking number" social cards want
    //   3. `executive_summary` — long multi-section editorial; works
    //      but verbose and noisy when truncated
    //   4. raw `battle.description`
    // The short-summary path strips the markdown sections (`### Top 3`,
    // tables, Notes line) so social cards see a plain sentence.
    let og_summary_source = battle
        .og_summary
        .clone()
        .or_else(|| {
            battle
                .short_summary
                .as_deref()
                .and_then(extract_short_summary_headline)
        })
        .or_else(|| exec_summary.clone())
        .unwrap_or_else(|| battle.description.clone());

    // Temperature events come from the ONE canonical query
    // (`fetch_events_for_battles`, fetched in the join above) — the exact
    // same source the feed/list cards use, so the chip is computed from an
    // identical event set and can't disagree between a battle's preview
    // and its own page. Used for both the chip and the sparkline below; an
    // empty set (query error) degrades to the stored snapshot fallback.
    let now = chrono::Utc::now();
    let events = temp_events.remove(&battle_id).unwrap_or_default();
    let live_temperature =
        crate::services::temperature::live_temperature(&events, battle.temperature, now);

    // Sparkline projection — 24 evenly spaced samples over the life
    // of the battle. Normalize x to 0..=100 for the SVG; the
    // template normalizes y against the local max so the line
    // always fills the chart vertically.
    let raw_spark = crate::services::temperature::sparkline(&events, now, 24);
    let spark_max = raw_spark
        .iter()
        .map(|(_, y)| *y)
        .fold(0.0f32, f32::max)
        .max(0.001);
    let sparkline: Vec<SparkPoint> = raw_spark
        .iter()
        .enumerate()
        .map(|(i, (_, y))| {
            let x = if raw_spark.len() <= 1 {
                0
            } else {
                ((i as f32 / (raw_spark.len() - 1) as f32) * 100.0).round() as u32
            };
            SparkPoint { x, y: *y }
        })
        .collect();

    // Timeline assembled from data already in hand — no extra
    // queries. Order matches the previous `build_timeline` impl
    // (newest first by `created_at`).
    let timeline_claims: Vec<TimelineClaim> = claims_with_author
        .iter()
        .map(|c| TimelineClaim {
            id: c.row.id,
            slug: c.row.slug.clone(),
            created_at: c.row.created_at,
            author_display_name: c.author_display_name.clone(),
        })
        .collect();
    let timeline = assemble_timeline(&summary_rows, &timeline_claims, &verdict_rows);

    let path_seg = battle.slug.clone().unwrap_or_else(|| battle.id.to_string());
    let share_url = format!("{}/battles/{}", state.public_base_url, path_seg);
    let canonical_link_url = share_url.clone();
    let share_title = format!("{} — KrabArena", battle.title);

    // Use the slug (falling back to id) so the og:image URL matches the
    // canonical page URL. The /og/battles/{id} route resolves either.
    let og_image_url = format!(
        "{}/og/battles/{}",
        state.public_base_url.trim_end_matches('/'),
        path_seg
    );
    let base_url = state.public_base_url.trim_end_matches('/');
    let battle_author_name_for_json_ld = battle_author_name.clone();
    let meta = PageMeta::new(&state.public_base_url, &format!("/battles/{}", path_seg))
        .with_title(battle.title.clone())
        .with_analytics_title("Battle")
        .with_description(&og_summary_source)
        .with_og_type("article")
        .with_og_image(og_image_url.clone())
        .with_og_image_alt(format!("KrabArena Battle — {}", battle.title))
        .with_markdown_alternate(format!("{share_url}.md"))
        .with_times(battle.created_at, battle.updated_at)
        .with_json_ld(breadcrumb_json_ld(vec![
            BreadcrumbItem::new("KrabArena", format!("{base_url}/")),
            BreadcrumbItem::new("Battles", format!("{base_url}/")),
            BreadcrumbItem::new(battle.title.as_str(), share_url.as_str()),
        ]))
        .with_json_ld(creative_work_json_ld_rich(
            "CreativeWork",
            &battle.title,
            &og_summary_source,
            &share_url,
            Some(&og_image_url),
            battle_author_name_for_json_ld.as_deref(),
            CreativeWorkExtras {
                public_base_url: &state.public_base_url,
                date_published: Some(battle.created_at),
                date_modified: Some(battle.updated_at),
                is_part_of: None,
                keywords: &battle.domain_tags,
            },
        ));

    let mut card = BattleCard::build(
        battle.id,
        battle.title.clone(),
        battle.description.clone(),
        battle.status.clone(),
        battle.domain_tags.clone(),
        claim_count,
        total_verify,
        total_refute,
        sponsor_domain,
    )
    .with_slug(battle.slug.clone())
    .with_author(battle_author_name)
    .with_executive_summary(exec_summary)
    .with_proxy_info(proxy_info)
    .with_temperature(live_temperature)
    .with_short_summary(battle.short_summary.clone())
    .with_cover_image(
        battle.cover_image_url.clone(),
        battle.cover_image_alt.clone(),
    )
    .with_produced_by(battle.produced_by.as_deref().unwrap_or(&[]));

    if claim_count > 0
        && let Some(agg) = cost_agg.get(&battle_id)
    {
        card.apply_cost_agg(agg);
    }

    let mut claims: Vec<ClaimCardView> = claims_with_author
        .iter()
        .map(|c| {
            ClaimCardView::from_row_with_author(
                &c.row,
                c.author_display_name.as_deref(),
                c.author_avatar_url.clone(),
            )
        })
        .collect();
    // Apply the Battle's curated solutions→logo map to every claim's
    // leaderboard rows (since 0.13.0). Battle wins over per-row
    // inline `logo_url` so the same solution renders the same logo
    // across every claim on the Battle. See `apply_battle_logos`.
    let battle_solutions = parse_solutions(&battle.solutions);
    for claim in claims.iter_mut() {
        apply_battle_logos(&battle_solutions, &mut claim.leaderboard);
    }

    // Split timeline at the 5th entry — see `docs/ui.md` →
    // "Battle history — keep it short". `split_off(5)` returns the
    // tail (entries 6+); everything before stays in `timeline`.
    let timeline_hidden_count = timeline.len().saturating_sub(TIMELINE_VISIBLE_ENTRIES);
    let mut timeline_visible = timeline;
    timeline_visible.truncate(TIMELINE_VISIBLE_ENTRIES);

    // Build the wins matrix — solutions × claims (since 0.13.4
    // rewrite; effort-sorted + column-capped since 0.26.1). One row
    // per distinct normalised (and vendor-grouped) solution; `wins` is
    // the total over ALL claims, while only the top columns render.
    // Per-battle presentation knobs (migration 056): a battle's
    // `presentation_config` JSONB overrides the house defaults; NULL /
    // partial / malformed degrades to `PresentationConfig::default()`.
    let presentation = PresentationConfig::from_json(battle.presentation_config.as_ref());
    let wins_matrix = build_wins_matrix(
        &claims,
        &claims_with_author,
        &battle_solutions,
        &presentation,
    );

    // Pie chart of the standings distribution. Derived from the SAME
    // aggregation as the matrix (`wins_matrix.rows`, already sorted
    // points-desc) so the pie, the table's Points column, and the
    // champion banner can never disagree. Sizes use the Olympic
    // `points` score (since 0.28.x), not raw win count; only solutions
    // with ≥1 point (a top-3 in a displayed benchmark) get a slice.
    // Slice colour = the player's real `brand_color` (extracted from its
    // stored logo, 0.28.5) when available, else its deterministic avatar
    // `logo_color`, else the palette — so slices match each player's
    // actual brand.
    let wins_leaderboard: Vec<WinsRow> = {
        // Colours are already resolved + de-duplicated per row in
        // `build_wins_matrix` (into `logo_color`), so the pie slice and
        // the avatar badge always match. Just read them through here.
        let scored: Vec<(&str, u32, &str)> = wins_matrix
            .rows
            .iter()
            .filter(|r| r.points > 0)
            .map(|r| (r.solution_name.as_str(), r.points, r.logo_color.as_str()))
            .collect();
        let total: u32 = scored.iter().map(|(_, p, _)| *p).sum();
        scored
            .iter()
            .map(|(sol, pts, color)| {
                let frac = if total > 0 {
                    *pts as f64 / total as f64
                } else {
                    0.0
                };
                WinsRow {
                    solution: (*sol).to_string(),
                    wins: *pts as usize,
                    pct_label: format!("{:.1}%", frac * 100.0),
                    color: (*color).to_string(),
                }
            })
            .collect()
    };

    // Match the home/list card's "quick summary" panel to the page's
    // real standings (0.28.7): project the same `wins_matrix` into the
    // top-3 × top-3 mini matrix so the embedded battle card here shows
    // identical standings (no extra query — reuses the matrix already
    // built above).
    card.short_summary_view.mini_matrix = mini_wins_matrix(&wins_matrix, 3, 3);

    // Champion banner above the matrix. Sourced from the battle's
    // `executive_summary` when it names a matrix solution; falls back
    // to the wins-count leader. `None` on empty matrices.
    let champion = build_champion(&wins_matrix, card.executive_summary.as_deref());

    // The HTML page renders only the first page of claims; the rest
    // stream in on scroll via /fragments/battles/{id}/claims (the DOM
    // stays bounded, battle_claims.js prunes off-window pages). The full
    // `claims` list still feeds the wins matrix above and the Markdown
    // variant below — both need every claim.
    let claims_has_more = claims.len() > BATTLE_CLAIMS_PAGE_SIZE as usize;
    let claims_first: Vec<ClaimCardView> = claims
        .iter()
        .take(BATTLE_CLAIMS_PAGE_SIZE as usize)
        .cloned()
        .collect();

    let html = BattleTemplate {
        meta: meta.clone(),
        share_url,
        share_title,
        battle: card.clone(),
        claims: claims_first,
        timeline_visible,
        timeline_hidden_count,
        sparkline,
        spark_max,
        wins_leaderboard,
        wins_matrix,
        champion,
        battle_uuid: battle.id.to_string(),
        claims_has_more,
        claims_next_page: 2,
    };
    let md = BattleMdTemplate {
        meta,
        agent_header,
        battle: card,
        claims,
    };
    crate::server_timing::record("compute", compute_started);
    let render_started = std::time::Instant::now();
    let resp = render_negotiated(
        &state.public_base_url,
        &format!("/battles/{id_or_slug}"),
        &headers,
        html,
        md,
    );
    crate::server_timing::record("render", render_started);
    with_canonical_link(resp, &canonical_link_url)
}

#[derive(Debug, Deserialize)]
pub struct BattleClaimsFragmentQuery {
    pub page: Option<i64>,
}

/// Build one offset page of claim cards for a Battle, matching the Battle
/// page's own claims order (`updated_at DESC, created_at DESC, id DESC`)
/// so the server-rendered first page and the scroll-loaded pages line up.
/// Returns `(cards, has_more)`. Offset (not keyset) is fine here: a single
/// Battle's claim list is bounded and indexed by `battle_id`.
async fn battle_claim_cards_page(
    state: &AppState,
    battle: &BattleRow,
    page: i64,
) -> Result<(Vec<ClaimCardView>, bool), sqlx::Error> {
    let page = page.max(1);
    let offset = (page - 1) * BATTLE_CLAIMS_PAGE_SIZE;
    let fetch_limit = BATTLE_CLAIMS_PAGE_SIZE + 1;
    let sql = format!(
        "SELECT {claim_cols}, u.display_name AS author_display_name, \
                u.avatar_url AS author_avatar_url \
         FROM claims c LEFT JOIN users u ON u.id = c.author_id \
         WHERE c.battle_id = $1 AND c.deleted_at IS NULL {superseded} \
           AND c.visibility IS DISTINCT FROM 'private' \
         ORDER BY c.updated_at DESC, c.created_at DESC, c.id DESC \
         LIMIT $2 OFFSET $3",
        claim_cols = crate::models::claim::claim_columns_for("c"),
        superseded = crate::models::claim::not_superseded("c")
    );
    let mut rows = sqlx::query_as::<_, ClaimWithAuthor>(&sql)
        .bind(battle.id)
        .bind(fetch_limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?;
    let has_more = rows.len() as i64 > BATTLE_CLAIMS_PAGE_SIZE;
    rows.truncate(BATTLE_CLAIMS_PAGE_SIZE as usize);

    let battle_solutions = parse_solutions(&battle.solutions);
    let mut cards: Vec<ClaimCardView> = rows
        .iter()
        .map(|c| {
            ClaimCardView::from_row_with_author(
                &c.row,
                c.author_display_name.as_deref(),
                c.author_avatar_url.clone(),
            )
        })
        .collect();
    for claim in cards.iter_mut() {
        apply_battle_logos(&battle_solutions, &mut claim.leaderboard);
    }
    Ok((cards, has_more))
}

/// HTMX fragment: page 2+ of a Battle's claims, fetched as the reader
/// scrolls. `battle_detail` server-renders page 1; the sentinel requests
/// the rest here. An unknown/soft-deleted Battle yields an empty page.
pub async fn battle_claims_fragment(
    State(state): State<AppState>,
    Path(battle_id): Path<Uuid>,
    Query(query): Query<BattleClaimsFragmentQuery>,
) -> Response {
    let page = query.page.unwrap_or(2).max(1);
    let path = format!("/fragments/battles/{battle_id}/claims");

    let battle = sqlx::query_as::<_, BattleRow>(&format!(
        "SELECT {BATTLE_COLUMNS} FROM battles WHERE id = $1 AND deleted_at IS NULL"
    ))
    .bind(battle_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();

    let (claims, has_more) = match &battle {
        Some(b) => battle_claim_cards_page(&state, b, page)
            .await
            .unwrap_or_default(),
        None => (Vec::new(), false),
    };

    response_with_vary(
        BattleClaimsPageTemplate {
            claims,
            has_more,
            battle_uuid: battle_id.to_string(),
            claims_next_page: page + 1,
        },
        &state.public_base_url,
        &path,
        "battle_claims_page.html",
        "HX-Request",
    )
}

pub async fn battle_timeline_fragment(
    State(state): State<AppState>,
    Path(battle_id): Path<Uuid>,
) -> Response {
    let path = format!("/fragments/battles/{battle_id}/timeline");
    let claims_sql = format!(
        "SELECT c.id, c.slug, c.created_at, u.display_name \
         FROM claims c LEFT JOIN users u ON u.id = c.author_id \
         WHERE c.battle_id = $1 AND c.deleted_at IS NULL {superseded} \
           AND c.visibility IS DISTINCT FROM 'private'",
        superseded = crate::models::claim::not_superseded("c")
    );
    let (live, claims, verdicts, summaries) = tokio::join!(
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM battles WHERE id = $1 AND deleted_at IS NULL)"
        )
        .bind(battle_id)
        .fetch_one(&state.pool),
        sqlx::query_as::<
            _,
            (
                Uuid,
                Option<String>,
                chrono::DateTime<chrono::Utc>,
                Option<String>,
            ),
        >(&claims_sql)
        .bind(battle_id)
        .fetch_all(&state.pool),
        sqlx::query_as::<_, TimelineVerdictRow>(TIMELINE_VERDICTS_SQL)
            .bind(battle_id)
            .fetch_all(&state.pool),
        sqlx::query_as::<_, TimelineSummaryRow>(TIMELINE_SUMMARIES_SQL)
            .bind(battle_id)
            .fetch_all(&state.pool),
    );
    let claims: Vec<TimelineClaim> = claims
        .unwrap_or_default()
        .into_iter()
        .map(
            |(id, slug, created_at, author_display_name)| TimelineClaim {
                id,
                slug,
                created_at,
                author_display_name,
            },
        )
        .collect();
    let entries = if !live.unwrap_or(false) {
        Vec::new()
    } else {
        assemble_timeline(
            &summaries.unwrap_or_default(),
            &claims,
            &verdicts.unwrap_or_default(),
        )
        .into_iter()
        .skip(TIMELINE_VISIBLE_ENTRIES)
        .collect()
    };
    response_with_vary(
        BattleTimelineRowsTemplate { entries },
        &state.public_base_url,
        &path,
        "battle_timeline_rows.html",
        "HX-Request",
    )
}

/// Assemble the chronological battle feed from already-fetched rows.
/// Replaces the old `build_timeline` which issued its own 3 queries —
/// `battle_detail` now fetches summaries + claims + verdicts in
/// parallel with everything else and hands them to this fn, so the
/// timeline costs zero extra round-trips. Newest first.
fn assemble_timeline(
    summaries: &[TimelineSummaryRow],
    claims: &[TimelineClaim],
    verdicts: &[TimelineVerdictRow],
) -> Vec<TimelineEntry> {
    use chrono::SecondsFormat;
    let mut entries: Vec<TimelineEntry> = Vec::new();

    for (at, temperature) in summaries {
        let bucket = temperature.map(crate::services::temperature::TempBucket::from_score);
        entries.push(TimelineEntry {
            kind: "summary",
            headline: "Summary updated".into(),
            detail: String::new(),
            timestamp: at.to_rfc3339_opts(SecondsFormat::Secs, true),
            sort_at: *at,
            temperature: *temperature,
            temp_class: bucket.map(|b| b.css_class()).unwrap_or(""),
            temp_emoji: bucket.map(|b| b.emoji()).unwrap_or(""),
            temp_label: bucket.map(|b| b.label()).unwrap_or(""),
            link: String::new(),
        });
    }

    for c in claims {
        let claim_path = c.slug.clone().unwrap_or_else(|| c.id.to_string());
        entries.push(TimelineEntry {
            kind: "claim",
            headline: format!(
                "Claim posted by {}",
                c.author_display_name
                    .clone()
                    .unwrap_or_else(|| "anonymous".into())
            ),
            detail: String::new(),
            timestamp: c.created_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            sort_at: c.created_at,
            temperature: None,
            temp_class: "",
            temp_emoji: "",
            temp_label: "",
            link: format!("/claims/{}", claim_path),
        });
    }

    for (at, verdict, env, author, claim_id, slug) in verdicts {
        let author = author.clone().unwrap_or_else(|| "anonymous".into());
        let (kind, headline, detail) = match verdict.as_str() {
            "confirmed" | "verified" => (
                "verify",
                format!("Verified by {author}"),
                env.clone().unwrap_or_default(),
            ),
            "disconfirmed" | "refuted" => ("refute", format!("Refuted by {author}"), String::new()),
            _ => continue,
        };
        let claim_path = slug.clone().unwrap_or_else(|| claim_id.to_string());
        entries.push(TimelineEntry {
            kind,
            headline,
            detail,
            timestamp: at.to_rfc3339_opts(SecondsFormat::Secs, true),
            sort_at: *at,
            temperature: None,
            temp_class: "",
            temp_emoji: "",
            temp_label: "",
            // Link straight to the comment thread on the claim — the
            // verdict's reasoning lives in the `failure_reason` /
            // `conditions` text on the verification row, but anyone
            // reading "Refuted by X" wants to dig into the discussion
            // around it. The `#comments` anchor scrolls past the
            // claim body to where the conversation actually happens.
            link: format!("/claims/{}#comments", claim_path),
        });
    }

    entries.sort_by_key(|e| std::cmp::Reverse(e.sort_at));
    entries
}

pub async fn claim_detail(
    State(state): State<AppState>,
    Path(raw_id_or_slug): Path<String>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> impl IntoResponse {
    // `/claims/<slug>.md` → always the raw-markdown twin.
    let (id_or_slug, headers, md_suffix) = split_markdown_suffix(raw_id_or_slug, headers);
    let looked_up_by_uuid = id_or_slug.parse::<Uuid>().is_ok();
    // `deleted_at IS NULL` — a soft-deleted claim 404s publicly (migration 053).
    let claim_row = if let Ok(uuid) = id_or_slug.parse::<Uuid>() {
        sqlx::query_as::<_, ClaimRow>(&format!(
            "SELECT {CLAIM_COLUMNS} FROM claims WHERE id = $1 AND deleted_at IS NULL"
        ))
        .bind(uuid)
        .fetch_optional(&state.pool)
        .await
    } else {
        sqlx::query_as::<_, ClaimRow>(&format!(
            "SELECT {CLAIM_COLUMNS} FROM claims WHERE slug = $1 AND deleted_at IS NULL"
        ))
        .bind(&id_or_slug)
        .fetch_optional(&state.pool)
        .await
    };

    // Private claims render exactly like a missing one unless the
    // caller is the author or an admin (same rule as the JSON API —
    // `services::claim_visibility`). Checked BEFORE the canonical
    // slug redirect so a UUID probe can't learn the slug either.
    let viewer =
        crate::services::claim_visibility::viewer_from_headers(&headers, &state.jwt_secret);
    let claim_row = match claim_row {
        Ok(Some(s))
            if crate::services::claim_visibility::can_view(
                s.visibility.as_deref(),
                s.author_id,
                &viewer,
            ) || crate::services::claim_visibility::voter_session_owns(
                &state.pool,
                &headers,
                s.author_id,
            )
            .await =>
        {
            s
        }
        _ => {
            return crate::web::errors::render_404(
                &state.public_base_url,
                &format!("/claims/{id_or_slug}"),
            );
        }
    };

    if looked_up_by_uuid {
        if let Some(slug) = claim_row.slug.as_deref() {
            let target = markdown_suffixed(slug, md_suffix);
            return canonical_slug_redirect("/claims", &target, raw_query.as_deref());
        }
    }

    let claim_id = claim_row.id;
    // `battle_id` is Optional since migration 035 — standalone claims
    // render without the "back to battle" crumb. Adopted claims keep
    // a battle_id, so the link still works for them.
    let battle_id_str = claim_row
        .battle_id
        .map(|bid| bid.to_string())
        .unwrap_or_default();
    let claim_id_str = claim_row.id.to_string();

    // Phase 1 — fire every claim-id-only dependent read in parallel.
    // Dropped the dedicated `author_name` query: the user lookup
    // batch below already pulls every author display name we need
    // (including the claim's own author), and we just look it up in
    // the resulting map. Same for the CMS header render — warm
    // cache makes it free, cold cache makes it a parallel RTT.
    let verifications_fut = sqlx::query_as::<_, VerificationRow>(
        "SELECT * FROM verifications WHERE claim_id = $1 ORDER BY created_at DESC",
    )
    .bind(claim_id)
    .fetch_all(&state.pool);

    // Explicit column list — `SELECT *` would reintroduce the cached-
    // plan trap after migration 044 added `battle_id` to `comments`
    // (AGENTS.md "Database" section). `let` binding keeps the format!
    // temporary alive across the join below.
    // `_WITH_PARENT` LEFT JOINs `comment_replies` (migration 073) so
    // the page can thread replies under their parent.
    let comments_query = format!(
        "SELECT {} FROM {} WHERE claim_id = $1 ORDER BY created_at DESC",
        crate::models::comment::COMMENT_COLUMNS_WITH_PARENT,
        crate::models::comment::COMMENTS_FROM_WITH_PARENT,
    );
    let comments_fut = sqlx::query_as::<_, CommentRow>(&comments_query)
        .bind(claim_id)
        .fetch_all(&state.pool);

    let attachments_fut = load_claim_attachments(&state.pool, claim_id);
    let review_cost_fut = load_review_cost(&state.pool, claim_row.review_id);

    // Parent battle's `solutions` JSONB — the canonical solution→logo
    // map (since 0.13.0). One round-trip; if it fails or the battle
    // has no curated entries, the per-row `logo_url` from the claim's
    // structure.json takes over (standalone-claim fallback).
    let battle_solutions_fut =
        sqlx::query_scalar::<_, serde_json::Value>("SELECT solutions FROM battles WHERE id = $1")
            .bind(claim_row.battle_id)
            .fetch_optional(&state.pool);

    let header_ctx = crate::services::cms_service::PageContext::Claim {
        battle_id: battle_id_str.clone(),
        claim_id: claim_id_str.clone(),
    };
    let agent_header_fut = crate::services::cms_service::render_header(&state.pool, &header_ctx);

    let (all_verifications, comments, attachments, review_cost, battle_solutions_raw, agent_header) = tokio::join!(
        verifications_fut,
        comments_fut,
        attachments_fut,
        review_cost_fut,
        battle_solutions_fut,
        agent_header_fut,
    );

    let all_verifications = all_verifications.unwrap_or_default();
    let comments = comments.unwrap_or_default();
    let (review_tokens, review_cost_usd) = review_cost;

    // Single chronological "Verification timeline" — verifies + refutes
    // interleaved by time. `all_verifications` is already sorted
    // `created_at DESC` by the query, so newest-first holds; cap to the
    // latest 10 for the panel. (The old template split this into a
    // verifies-then-refutes list, which buried the newest event.)
    let verification_timeline: Vec<VerificationRow> =
        all_verifications.iter().take(10).cloned().collect();

    let (verifies, refutes): (Vec<_>, Vec<_>) = all_verifications
        .into_iter()
        .partition(|r| r.verdict == "confirmed" || r.verdict == "verified");

    // Phase 2 — single batch user fetch for the claim's author +
    // every verifier/refuter/commenter. Replaces the old dedicated
    // `author_name` SELECT (the author_id is seeded into this list).
    let mut user_ids: Vec<Uuid> = vec![claim_row.author_id];
    for r in verifies.iter().chain(refutes.iter()) {
        user_ids.push(r.author_id);
    }
    for c in &comments {
        user_ids.push(c.author_id);
    }
    user_ids.sort();
    user_ids.dedup();

    let user_rows = sqlx::query_as::<_, (Uuid, String, Option<String>)>(
        "SELECT id, display_name, avatar_url FROM users WHERE id = ANY($1)",
    )
    .bind(&user_ids)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let mut user_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut user_avatars: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (id, name, avatar) in user_rows {
        let key = id.to_string();
        user_names.insert(key.clone(), name);
        if let Some(url) = avatar
            && !url.trim().is_empty()
        {
            user_avatars.insert(key, url);
        }
    }

    // Author name now comes from the batch lookup instead of a
    // dedicated single-row query.
    let author_name = user_names
        .get(&claim_row.author_id.to_string())
        .cloned()
        .unwrap_or_else(|| "Unknown".to_string());

    // Threaded comments with `@mentions` linked (since 0.43.6).
    let comment_ids: Vec<Uuid> = comments.iter().map(|c| c.id).collect();
    let comment_mentions =
        crate::services::comment_service::load_mentions(&state.pool, &comment_ids)
            .await
            .unwrap_or_default();
    let comment_threads = crate::web::comment_threads::build_comment_views(
        &comments,
        &comment_mentions,
        &user_names,
        &user_avatars,
    );

    let author_avatar = user_avatars.get(&claim_row.author_id.to_string()).cloned();
    let mut claim_view =
        ClaimCardView::from_row_with_author(&claim_row, Some(&author_name), author_avatar);
    claim_view.attachments = attachments;
    claim_view.review_tokens_total = review_tokens;
    claim_view.review_cost_usd = review_cost_usd;
    // Per-row logos resolve in two layers (since 0.13.0):
    //   1. parent battle's `solutions` JSONB — the canonical
    //      solution→logo map curated by the Battle's editor / the
    //      backfill admin endpoint. Wins when the normalised
    //      solution name matches an entry.
    //   2. otherwise, the per-row `logo_url` already populated from
    //      structure.json by `ClaimCardView::project_structure`
    //      (standalone-claim fallback).
    let battle_solutions: Vec<crate::models::battle::BattleSolution> = battle_solutions_raw
        .ok()
        .flatten()
        .map(|v| parse_solutions(&v))
        .unwrap_or_default();
    apply_battle_logos(&battle_solutions, &mut claim_view.leaderboard);
    // Layer 3 (since 0.28.x): for rows still without a logo — chiefly
    // standalone claims with no parent battle to curate one — fall back
    // to the data-driven `vendor_logos` map. Only hit the DB when at
    // least one row is uncovered, so battle-attached claims (already
    // fully logo'd by layer 1) pay nothing.
    if claim_view.leaderboard.iter().any(|r| r.logo_url.is_none()) {
        if let Ok(patterns) =
            crate::services::solution_names::load_vendor_patterns(&state.pool).await
        {
            apply_vendor_logo_fallback(&patterns, &mut claim_view.leaderboard);
        }
    }
    let battle_id = battle_id_str;

    let path_seg = claim_row
        .slug
        .clone()
        .unwrap_or_else(|| claim_row.id.to_string());
    let share_url = format!("{}/claims/{}", state.public_base_url, path_seg);
    let canonical_link_url = share_url.clone();
    let share_title = format!("{} — KrabArena Claim", claim_view.claim_title);

    // Prefer the claim's review-generated og_summary, fall back to
    // artifact_summary, then the claim body.
    let og_summary_source = claim_view
        .og_summary
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if !claim_view.artifact_summary.is_empty() {
                claim_view.artifact_summary.clone()
            } else {
                claim_view.claim_body.clone()
            }
        });

    let og_image_url = format!(
        "{}/og/claims/{}",
        state.public_base_url.trim_end_matches('/'),
        claim_row.id
    );
    let base_url = state.public_base_url.trim_end_matches('/');
    // Parent Battle's slug + title for the breadcrumb / `isPartOf` —
    // the bare-UUID crumb pointed crawlers at a 308 instead of the
    // canonical Battle URL. One indexed PK lookup.
    let (parent_battle_slug, parent_battle_title): (Option<String>, Option<String>) =
        match claim_row.battle_id {
            Some(bid) => sqlx::query_as::<_, (Option<String>, String)>(
                "SELECT slug, title FROM battles WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(bid)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .map(|(slug, title)| (slug, Some(title)))
            .unwrap_or((None, None)),
            None => (None, None),
        };
    let parent_battle_url = format!(
        "{base_url}/battles/{}",
        parent_battle_slug.as_deref().unwrap_or(battle_id.as_str())
    );
    let mut breadcrumbs = vec![BreadcrumbItem::new("KrabArena", format!("{base_url}/"))];
    if battle_id.is_empty() {
        breadcrumbs.push(BreadcrumbItem::new(
            "Standalone Claims",
            format!("{base_url}/claims/standalone"),
        ));
    } else {
        breadcrumbs.push(BreadcrumbItem::new(
            parent_battle_title.as_deref().unwrap_or("Battle"),
            parent_battle_url.clone(),
        ));
    }
    breadcrumbs.push(BreadcrumbItem::new(
        claim_view.claim_title.as_str(),
        share_url.as_str(),
    ));
    let meta = PageMeta::new(&state.public_base_url, &format!("/claims/{}", path_seg))
        .with_title(claim_view.claim_title.clone())
        .with_analytics_title("Claim")
        .with_description(&og_summary_source)
        .with_og_type("article")
        .with_og_image(og_image_url.clone())
        .with_og_image_alt(format!("KrabArena Claim — {}", claim_view.claim_title))
        .with_markdown_alternate(format!("{share_url}.md"))
        .with_times(claim_row.created_at, claim_row.updated_at)
        .with_json_ld(breadcrumb_json_ld(breadcrumbs))
        .with_json_ld(creative_work_json_ld_rich(
            "Article",
            &claim_view.claim_title,
            &og_summary_source,
            &share_url,
            Some(&og_image_url),
            Some(&author_name),
            CreativeWorkExtras {
                public_base_url: &state.public_base_url,
                date_published: Some(claim_row.created_at),
                date_modified: Some(claim_row.updated_at),
                is_part_of: if battle_id.is_empty() {
                    None
                } else {
                    Some((
                        parent_battle_title.as_deref().unwrap_or("Battle"),
                        parent_battle_url.as_str(),
                    ))
                },
                keywords: &[],
            },
        ));

    // Verdict strip + hero module (see `select_presentation`'s doc
    // comment for the fallback ladder). Parsed here rather than reused
    // from `claim_view` because `ClaimCardView::project_structure`
    // only keeps the v1-compatible projection — the ladder needs the
    // raw `ClaimStructure` (v2 `metrics` / `hypothesis` fields).
    let mut parsed_structure: Option<krabarena_shared::claim_structure::ClaimStructure> = claim_row
        .claim_structure
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    // Backfill vendor favicons into the raw structure leaderboard so the
    // race/frontier hero shows logos. The view model (`claim_view`) was
    // already enriched by `apply_battle_logos` above; the raw
    // `ClaimStructure` the presentation ladder reads is a separate copy.
    if let Some(ps) = parsed_structure.as_mut() {
        use crate::services::solution_names::normalize_name;
        use std::collections::HashMap;
        let lookup: HashMap<&str, &str> = battle_solutions
            .iter()
            .filter_map(|s| {
                s.logo_url
                    .as_deref()
                    .map(|u| (s.name_normalized.as_str(), u))
            })
            .collect();
        for e in ps.leaderboard.iter_mut() {
            if e.logo_url.is_none() {
                let key = normalize_name(&e.solution);
                if let Some(url) = lookup.get(key.as_str()) {
                    e.logo_url = Some((*url).to_string());
                }
            }
        }
    }
    let pres = select_presentation(&claim_row, parsed_structure.as_ref());
    let share_text_for_intent = pres
        .as_ref()
        .map(|p| p.tweet_title.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| claim_view.claim_title.clone());

    let html = ClaimTemplate {
        meta: meta.clone(),
        share_url,
        share_title,
        claim: claim_view.clone(),
        pres,
        share_text_for_intent,
        battle_id: battle_id.clone(),
        author_name: author_name.clone(),
        verifies: verifies.clone(),
        refutes: refutes.clone(),
        verification_timeline,
        comment_threads: comment_threads.clone(),
        user_names: user_names.clone(),
        user_avatars,
    };
    let md = ClaimMdTemplate {
        meta,
        agent_header,
        claim: claim_view,
        battle_id,
        author_name,
        verifies,
        refutes,
        comment_threads,
        user_names,
    };
    let resp = render_negotiated(
        &state.public_base_url,
        &format!("/claims/{id_or_slug}"),
        &headers,
        html,
        md,
    );
    with_canonical_link(resp, &canonical_link_url)
}

pub async fn leaderboard(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    // Compute + cache the full ranked list once per TTL, then slice the
    // first page. Subsequent pages stream from /fragments/agents on demand
    // (the "More" button) — the browser never holds the whole list.
    let all = cached_top_agents(&state, AGENTS_RANKED_CAP).await;
    let total_agents = all.len() as i64;
    let agents_has_more = total_agents > AGENTS_PAGE_SIZE;
    let first: Vec<AgentRow> = all.into_iter().take(AGENTS_PAGE_SIZE as usize).collect();
    let ranked = rank_agents(&first, 0);

    let meta = PageMeta::new(&state.public_base_url, "/agents")
        .with_title("Top Agents")
        .with_description(
            "Top AI agents on KrabArena, ranked by total $ effort across \
             every published claim.",
        )
        .with_json_ld(breadcrumb_json_ld(vec![
            BreadcrumbItem::new(
                "KrabArena",
                format!("{}/", state.public_base_url.trim_end_matches('/')),
            ),
            BreadcrumbItem::new(
                "Agents",
                format!("{}/agents", state.public_base_url.trim_end_matches('/')),
            ),
        ]));
    let html = LeaderboardTemplate {
        meta: meta.clone(),
        agents: ranked,
        total_agents,
        agents_has_more,
        agents_next_offset: AGENTS_PAGE_SIZE,
    };
    let md = LeaderboardMdTemplate {
        meta,
        agents: first,
    };
    render_negotiated(&state.public_base_url, "/agents", &headers, html, md)
}

/// Attach an absolute rank (`offset + position`) to each agent row.
fn rank_agents(slice: &[AgentRow], offset: i64) -> Vec<RankedAgentRow> {
    slice
        .iter()
        .enumerate()
        .map(|(i, a)| RankedAgentRow {
            rank: offset + i as i64 + 1,
            agent: a.clone(),
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct AgentsFragmentQuery {
    pub offset: Option<i64>,
}

/// HTMX fragment: the next page of leaderboard rows, served on demand by
/// the "More" button. Slices the same cached ranked list as the SSR first
/// page, so ranks are contiguous and the aggregate runs at most once/TTL.
pub async fn agents_fragment(
    State(state): State<AppState>,
    Query(query): Query<AgentsFragmentQuery>,
) -> Response {
    let offset = query.offset.unwrap_or(0).max(0);
    let all = cached_top_agents(&state, AGENTS_RANKED_CAP).await;
    let total = all.len() as i64;
    let start = offset.clamp(0, total) as usize;
    let end = (offset + AGENTS_PAGE_SIZE).clamp(0, total) as usize;
    let ranked = rank_agents(&all[start..end], offset);
    let has_more = offset + AGENTS_PAGE_SIZE < total;

    response_with_vary(
        AgentsLbPageTemplate {
            agents: ranked,
            has_more,
            agents_next_offset: offset + AGENTS_PAGE_SIZE,
        },
        &state.public_base_url,
        "/fragments/agents",
        "agents_lb_page.html",
        "HX-Request",
    )
}

/// `top_agents_by_effort` behind the short-TTL leaderboard cache
/// (`AppState::leaderboards`). The aggregate is identical for every
/// viewer, so under traffic we serve a memoised copy and recompute at
/// most once per `limit` per TTL window. Keyed by `limit` since the
/// `/agents` page (50) and the home top-10 (10) share this path.
async fn cached_top_agents(state: &AppState, limit: i64) -> Vec<AgentRow> {
    state
        .leaderboards
        .agents_web
        .get_or_fill(&limit.to_string(), || {
            top_agents_by_effort(&state.pool, limit)
        })
        .await
}

/// Single-roundtrip leaderboard: per-user cost rollup +
/// verify/refute counts, sorted by total `$ effort` (author + KrabReview)
/// desc. Powers both the full `/agents` list and the home page top-10
/// block — same row shape, same sort, just different `LIMIT`.
///
/// External-API spend (`claim_structure -> 'cost' ->> 'external_usd'`)
/// is NOT in the SQL effort total because the JSONB cast doesn't
/// aggregate cleanly across the LEFT JOIN with claim_reviews; it's
/// folded into `external_cost_usd` for display but not for ordering.
/// Agents whose effort is purely external API spend are rare, so the
/// ordering tracks reality closely enough.
pub async fn top_agents_by_effort(pool: &sqlx::PgPool, limit: i64) -> Vec<AgentRow> {
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            i64,
            i64,
            i64,
            i64,
            f64,
            f64,
            f64,
            Option<String>,
        ),
    >(&format!(
        r#"
        WITH user_claims AS (
            -- Leaderboard claim count + author-compute rollup over
            -- lineage HEADs only: an edited claim must not count its
            -- old + new version twice. (Alias `c`.)
            SELECT
                c.author_id,
                COUNT(*)::bigint AS total_claims,
                COALESCE(SUM(c.tokens_consumed), 0)::bigint AS author_tokens,
                (COALESCE(SUM(c.price_cents), 0)::float8 / 100.0) AS author_cost_usd,
                COALESCE(
                    SUM((c.claim_structure -> 'cost' ->> 'external_usd')::numeric),
                    0
                )::float8 AS external_cost_usd
            FROM claims c
            WHERE c.deleted_at IS NULL {superseded}
            GROUP BY c.author_id
        ),
        user_reviews AS (
            SELECT c.author_id,
                   (COALESCE(SUM(r.cost_micros), 0)::float8 / 1000000.0) AS krabreview_cost_usd
            FROM claims c
            JOIN claim_reviews r ON r.id = c.review_id
            WHERE c.deleted_at IS NULL {superseded}
            GROUP BY c.author_id
        ),
        user_verifies AS (
            SELECT v.author_id,
                   COUNT(*) FILTER (WHERE v.verdict = 'confirmed')::bigint AS total_verify,
                   COUNT(*) FILTER (WHERE v.verdict = 'disconfirmed')::bigint AS total_refute
            FROM verifications v
            GROUP BY v.author_id
        )
        SELECT
            u.id,
            u.display_name,
            e.domain,
            COALESCE(uv.total_verify, 0)::bigint        AS total_verify,
            COALESCE(uc.total_claims, 0)::bigint        AS total_claims,
            COALESCE(uv.total_refute, 0)::bigint        AS total_refute,
            COALESCE(uc.author_tokens, 0)::bigint       AS author_tokens,
            COALESCE(uc.author_cost_usd, 0)::float8     AS author_cost_usd,
            COALESCE(uc.external_cost_usd, 0)::float8   AS external_cost_usd,
            COALESCE(ur.krabreview_cost_usd, 0)::float8 AS krabreview_cost_usd,
            u.avatar_url
        FROM users u
        JOIN entities e ON u.entity_id = e.id
        LEFT JOIN user_claims uc    ON uc.author_id = u.id
        LEFT JOIN user_reviews ur   ON ur.author_id = u.id
        LEFT JOIN user_verifies uv  ON uv.author_id = u.id
        -- Sort key matches what the table displays as "$ effort":
        -- author $ + external proxy $ + KrabReview $. Dropping
        -- external_cost_usd here mis-ranks anyone who spent on the
        -- Battle Proxy (their displayed effort exceeds the sort key).
        ORDER BY (
            COALESCE(uc.author_cost_usd, 0)
            + COALESCE(uc.external_cost_usd, 0)
            + COALESCE(ur.krabreview_cost_usd, 0)
        ) DESC,
        COALESCE(uv.total_verify, 0) DESC,
        COALESCE(uc.total_claims, 0) DESC
        LIMIT $1
        "#,
        superseded = crate::models::claim::not_superseded("c")
    ))
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(
            |(
                id,
                display_name,
                entity_domain,
                total_verify,
                total_claims,
                total_refute,
                author_tokens,
                author_cost_usd,
                external_cost_usd,
                krabreview_cost_usd,
                avatar_url,
            )| AgentRow {
                user_id: id.to_string(),
                display_name,
                entity_domain,
                total_verify,
                total_claims,
                total_refute,
                author_tokens,
                author_cost_usd,
                external_cost_usd,
                krabreview_cost_usd,
                avatar_url,
            },
        )
        .collect()
}

/// Char-boundary-safe preview for profile-page activity feed entries.
///
/// Byte-slicing (`&s[..80]`) panics when byte 80 falls inside a
/// multi-byte UTF-8 character — this hit prod on agent profile pages
/// (503s at /agents/51a6b67c-4144-4d52-9fa3-c5f6c75013d8) whenever a
/// claim/reason/comment body had non-ASCII content near the cut point.
/// Delegates to `truncate_chars`, which counts chars, not bytes.
fn preview_80(text: &str) -> String {
    crate::services::presentation_check::truncate_chars(text, 80)
}

pub async fn contributor_profile(
    State(state): State<AppState>,
    Path(raw_user_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let (raw_user_id, headers, _) = split_markdown_suffix(raw_user_id, headers);
    let Ok(user_id) = Uuid::parse_str(&raw_user_id) else {
        return (StatusCode::NOT_FOUND, Html("Agent not found".to_string())).into_response();
    };
    // Profile total_claims = lineage HEADs only (edited claims don't
    // double-count). See `models::claim::not_superseded`.
    let superseded = crate::models::claim::not_superseded("c");
    let user_row = sqlx::query_as::<_, (Uuid, String, String, i64, i64, i64, Option<String>)>(&format!(
        r#"
        SELECT
            u.id,
            u.display_name,
            e.domain,
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'confirmed') as total_verify,
            (SELECT COUNT(*) FROM claims c WHERE c.author_id = u.id AND c.deleted_at IS NULL {superseded}) as total_claims,
            (SELECT COUNT(*) FROM verifications WHERE author_id = u.id AND verdict = 'disconfirmed') as total_refute,
            u.avatar_url
        FROM users u
        JOIN entities e ON u.entity_id = e.id
        WHERE u.id = $1
        "#
    ))
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await;

    let (uid, name, domain, verify, claims, refute, avatar_url) = match user_row {
        Ok(Some(r)) => r,
        _ => return (StatusCode::NOT_FOUND, Html("Agent not found".to_string())).into_response(),
    };

    let agent = AgentRow {
        user_id: uid.to_string(),
        display_name: name,
        entity_domain: domain,
        total_verify: verify,
        total_claims: claims,
        total_refute: refute,
        author_tokens: 0,
        author_cost_usd: 0.0,
        external_cost_usd: 0.0,
        krabreview_cost_usd: 0.0,
        avatar_url,
    };

    let mut activity: Vec<ActivityItem> = Vec::new();

    let battles_sql = format!(
        "SELECT {BATTLE_COLUMNS} FROM battles WHERE author_id = $1 AND deleted_at IS NULL \
         ORDER BY created_at DESC LIMIT 20"
    );
    let battles = sqlx::query_as::<_, BattleRow>(&battles_sql)
        .bind(user_id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();

    for h in battles {
        activity.push(ActivityItem {
            kind: "Battle".to_string(),
            title: h.title,
            link: format!("/battles/{}", h.id),
            detail: format!("[{}] {}", h.status, h.domain_tags.join(", ")),
            created_at: h.created_at.format("%Y-%m-%d %H:%M").to_string(),
        });
    }

    // Profile "recent claims" list: lineage HEADs only (the old
    // version an edit replaced drops out of the profile feed).
    let claim_rows = sqlx::query_as::<_, ClaimRow>(&format!(
        "SELECT {claim_cols} FROM claims c WHERE c.author_id = $1 AND c.deleted_at IS NULL {superseded} \
         ORDER BY c.created_at DESC LIMIT 20",
        claim_cols = crate::models::claim::claim_columns_for("c"),
        superseded = crate::models::claim::not_superseded("c")
    ))
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    // Private claims appear only on the author's own (or an admin's)
    // view of the profile.
    let profile_viewer =
        crate::services::claim_visibility::viewer_from_headers(&headers, &state.jwt_secret);
    let claim_rows = claim_rows.into_iter().filter(|s| {
        crate::services::claim_visibility::can_view(
            s.visibility.as_deref(),
            s.author_id,
            &profile_viewer,
        )
    });

    for s in claim_rows {
        let preview = preview_80(&s.claim);
        activity.push(ActivityItem {
            kind: "Claim".to_string(),
            title: preview,
            link: format!("/claims/{}", s.id),
            detail: format!(
                "✅ {} verify · ❌ {} refute",
                s.verify_count, s.refute_count
            ),
            created_at: s.created_at.format("%Y-%m-%d %H:%M").to_string(),
        });
    }

    let verify_rows = sqlx::query_as::<_, VerificationRow>(
        "SELECT * FROM verifications WHERE author_id = $1 AND verdict = 'confirmed' ORDER BY created_at DESC LIMIT 20",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    for r in verify_rows {
        activity.push(ActivityItem {
            kind: "Verify".to_string(),
            title: format!("Verified in {}ms", r.duration_ms.unwrap_or(0)),
            link: format!("/claims/{}", r.claim_id),
            detail: r.environment.unwrap_or_default().chars().take(80).collect(),
            created_at: r.created_at.format("%Y-%m-%d %H:%M").to_string(),
        });
    }

    let refute_rows = sqlx::query_as::<_, VerificationRow>(
        "SELECT * FROM verifications WHERE author_id = $1 AND verdict = 'disconfirmed' ORDER BY created_at DESC LIMIT 20",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    for r in refute_rows {
        let reason = r.failure_reason.unwrap_or_default();
        let preview = preview_80(&reason);
        activity.push(ActivityItem {
            kind: "Refute".to_string(),
            title: preview,
            link: format!("/claims/{}", r.claim_id),
            detail: r.conditions.unwrap_or_default().chars().take(80).collect(),
            created_at: r.created_at.format("%Y-%m-%d %H:%M").to_string(),
        });
    }

    // Explicit column list — see migration 044 cached-plan note above.
    let comment_rows = sqlx::query_as::<_, CommentRow>(&format!(
        "SELECT {} FROM comments WHERE author_id = $1 ORDER BY created_at DESC LIMIT 20",
        crate::models::comment::COMMENT_COLUMNS,
    ))
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    for c in comment_rows {
        let preview = preview_80(&c.body);
        // Post-migration 044, a comment can target a Battle instead
        // of a Claim. Link to whichever side is set; the XOR
        // constraint guarantees one of the two is.
        let link = if let Some(claim_id) = c.claim_id {
            format!("/claims/{}", claim_id)
        } else if let Some(battle_id) = c.battle_id {
            format!("/battles/{}", battle_id)
        } else {
            // Defensive: comments_target_xor makes this unreachable.
            String::new()
        };
        activity.push(ActivityItem {
            kind: "Comment".to_string(),
            title: preview,
            link,
            detail: String::new(),
            created_at: c.created_at.format("%Y-%m-%d %H:%M").to_string(),
        });
    }

    activity.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    // The Markdown variant keeps the full feed; the HTML page shows the
    // latest 5 inline and tucks the rest behind a "Show N more" toggle.
    let activity_full = activity;
    let mut activity_head = activity_full.clone();
    let activity_more = if activity_head.len() > 5 {
        activity_head.split_off(5)
    } else {
        Vec::new()
    };

    let agent_path = format!("/agents/{}", agent.user_id);
    let agent_url = format!(
        "{}{}",
        state.public_base_url.trim_end_matches('/'),
        agent_path
    );
    let meta = PageMeta::new(&state.public_base_url, &agent_path)
        .with_title(agent.display_name.clone())
        .with_analytics_title("Agent")
        .with_description(format!(
            "{} ({}) — {} verifies, {} claims, {} refutes on KrabArena.",
            agent.display_name,
            agent.entity_domain,
            agent.total_verify,
            agent.total_claims,
            agent.total_refute,
        ))
        .with_og_type("profile")
        .with_markdown_alternate(format!("{agent_url}.md"))
        .with_json_ld(breadcrumb_json_ld(vec![
            BreadcrumbItem::new(
                "KrabArena",
                format!("{}/", state.public_base_url.trim_end_matches('/')),
            ),
            BreadcrumbItem::new(
                "Agents",
                format!("{}/agents", state.public_base_url.trim_end_matches('/')),
            ),
            BreadcrumbItem::new(agent.display_name.as_str(), agent_url.as_str()),
        ]));
    let cost = agent_cost_rollup(&state.pool, user_id).await;
    let cost_by_backend = agent_cost_by_backend(&state.pool, user_id).await;
    let html = AgentTemplate {
        meta: meta.clone(),
        agent: agent.clone(),
        activity: activity_head,
        activity_more,
        cost,
        cost_by_backend,
    };
    let md = AgentMdTemplate {
        meta,
        agent,
        activity: activity_full,
    };
    render_negotiated(
        &state.public_base_url,
        &format!("/agents/{user_id}"),
        &headers,
        html,
        md,
    )
}

/// CSS accent class for a notification kind on `/notifications`:
/// green for a Verify / passing review, red for a Refute / blocking
/// review, neutral otherwise.
pub(crate) fn notification_kind_css(kind: &str, verdict: Option<&str>) -> &'static str {
    use crate::services::activity_service::notification_kind::{CLAIM_REVIEW, RESPONSE};
    match (kind, verdict) {
        (RESPONSE, Some("confirmed" | "verified")) => "verify",
        (RESPONSE, Some("disconfirmed" | "refuted")) => "refute",
        (CLAIM_REVIEW, Some("pass")) => "verify",
        (CLAIM_REVIEW, Some("block")) => "refute",
        _ => "",
    }
}

/// HTML + Markdown notifications feed for the authenticated agent.
///
/// Auth: the agent's JWT (from `x-krabarena-token` or `Authorization`).
/// Browsers don't send that header on ordinary navigations, so for a
/// logged-out human visitor we render a "sign in via CLI" CTA instead
/// of 401'ing.
pub async fn notifications(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let user_id = crate::routes::auth::extract_user_id(&headers, &state.jwt_secret).ok();

    let meta = PageMeta::new(&state.public_base_url, "/notifications")
        .with_title("Notifications")
        .with_description(
            "Verifies, refutes, and comments on your work, replies to your comments and @mentions, new claims and comments on your battles, reviewer verdicts, and summary updates.",
        )
        .noindex_follow();

    let (items, authenticated) = match user_id {
        None => (Vec::new(), false),
        Some(uid) => {
            let raw =
                crate::services::activity_service::user_notifications(&state.pool, uid, None, 50)
                    .await
                    .map(|(items, _)| items)
                    .unwrap_or_default();

            // `user_notifications` already resolved actor names and
            // the display-ready label / title / preview / path, so the
            // page only picks the CSS accent (see `notification_kind_css`).
            let rows: Vec<NotificationRow> = raw
                .into_iter()
                .map(|i| NotificationRow {
                    kind_css: notification_kind_css(&i.kind, i.verdict.as_deref()).to_string(),
                    kind_label: i.label.clone(),
                    actor: i.author_name.clone().unwrap_or_else(|| {
                        if i.author_id.is_none() {
                            "KrabReviewer".to_string()
                        } else {
                            "Unknown".to_string()
                        }
                    }),
                    target: i.title.clone().unwrap_or_default(),
                    body: i.preview.clone(),
                    link: i.path.clone(),
                    created_at: i.created_at.format("%Y-%m-%d %H:%M").to_string(),
                    unread: !i.read,
                })
                .collect();

            (rows, true)
        }
    };

    let html = NotificationsTemplate {
        meta: meta.clone(),
        authenticated,
        items: items.clone(),
    };
    let md = NotificationsMdTemplate {
        meta,
        authenticated,
        items,
    };
    render_negotiated(&state.public_base_url, "/notifications", &headers, html, md)
}

// --- Standalone claims listing (migration 035) -------------------------------

#[derive(Debug, Clone)]
pub struct StandaloneClaimCard {
    pub slug: String,
    pub title: String,
    pub topic: String,
    pub tags_label: String,
    pub author_name: String,
    pub created_at: String,
}

#[derive(Template)]
#[template(path = "pages/standalone_claims.html")]
struct StandaloneClaimsTemplate {
    meta: PageMeta,
    claims: Vec<StandaloneClaimCard>,
}

/// `/claims/standalone` — public list of standalone Claims that have
/// been promoted to public (visibility='public', battle_id IS NULL).
/// Private standalone claims stay hidden from this view.
pub async fn standalone_claims(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    use crate::services::claim_service;
    let claims_resp = claim_service::list_standalone_claims(&state.pool, None, 100)
        .await
        .map(|(items, _)| items)
        .unwrap_or_default();

    let claims: Vec<StandaloneClaimCard> = claims_resp
        .into_iter()
        .map(|c| {
            // Title = first `# heading` or first line of the claim body.
            let title = c
                .claim
                .lines()
                .find(|l| l.trim().starts_with("# "))
                .map(|l| l.trim().trim_start_matches("# ").to_string())
                .unwrap_or_else(|| c.claim.lines().next().unwrap_or("").to_string());
            let tags_label = c.topic_tags.join(", ");
            let author_name = c
                .author
                .as_ref()
                .map(|a| a.display_name.clone())
                .unwrap_or_else(|| "Unknown".into());
            StandaloneClaimCard {
                slug: c.slug.clone().unwrap_or_else(|| c.id.to_string()),
                title,
                topic: c.topic.unwrap_or_default(),
                tags_label,
                author_name,
                created_at: c.created_at.to_rfc3339(),
            }
        })
        .collect();

    let meta = PageMeta::new(&state.public_base_url, "/claims/standalone")
        .with_title("Standalone Claims")
        .with_description(
            "Results posted against topics that don't yet have a curated KrabArena Battle.",
        );
    let html = StandaloneClaimsTemplate {
        meta: meta.clone(),
        claims,
    };
    // Avoid a second template; the standalone listing is HTML-only
    // for the v0 surface. Markdown variant can land in a follow-up.
    let _ = meta;
    let _ = headers;
    crate::web::errors::render_template_or_500(
        html,
        &state.public_base_url,
        "/claims/standalone",
        "standalone_claims.html",
    )
}

// ---------------------------------------------------------------------------
// /search — hybrid embedding + freshness + hotness search HTML page.
//
// Companion to `GET /v1/search`. The form posts (GET) `q` + `kind` back to
// the same URL so the topnav box can submit a plain HTML form without any
// JS round-trip. Empty `q` returns the form-only state.
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
pub struct SearchPageQuery {
    pub q: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<i64>,
    pub ai: Option<String>,
}

#[derive(Template)]
#[template(path = "pages/search.html")]
struct SearchTemplate {
    meta: PageMeta,
    q: String,
    kind: String,
    hits: Vec<crate::services::search_service::SearchHit>,
    /// Whether the AI (semantic) toggle was on for this query. Carried
    /// to the "show more" loader so it requests the same mode.
    ai: bool,
    /// AI was requested but the embedding failed, so results are
    /// lexical-only. Surfaced so the page can say so rather than
    /// silently degrade.
    degraded: bool,
    /// URL for the "show more" loader (empty = no more results within
    /// the candidate window). Pre-built in Rust so the query string is
    /// properly encoded; used as the no-JS fallback href. JS intercepts
    /// the click and appends via the JSON API instead of navigating.
    more_url: String,
    /// Next `limit` the "show more" loader should request (0 = none).
    more_limit: i64,
}

pub async fn search_page(
    State(state): State<AppState>,
    Query(params): Query<SearchPageQuery>,
) -> impl IntoResponse {
    use crate::services::search_service::{self, DEFAULT_LIMIT, SearchKind};

    let q = params.q.as_deref().map(str::trim).unwrap_or("").to_string();
    let kind = SearchKind::parse(params.kind.as_deref());
    let kind_label = kind.as_str();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let want_ai = crate::routes::search::parse_ai_flag(params.ai.as_deref());

    let client = crate::services::embedding_service::global();

    let mut degraded = false;
    let hits = if q.is_empty() {
        Vec::new()
    } else {
        // In AI mode with a *live* provider, embed; on failure degrade
        // to lexical-only rather than rendering an empty page (a broken
        // search box looks like "no results", a worse lie than "fewer").
        // With the hash fallback (no GEMINI_API_KEY) the embedding is
        // noise, so skip it — AI-on then matches AI-off (lexical-only).
        let embedding = if want_ai && client.is_live() {
            match client.embed_query(&q).await {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!(error = %e, "search.page.embed_failed.degrading_to_lexical");
                    degraded = true;
                    None
                }
            }
        } else {
            None
        };
        search_service::search(
            &state.pool,
            &state.public_base_url,
            &q,
            embedding.as_deref(),
            kind,
            limit,
        )
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "search.page.query_failed");
            Vec::new()
        })
    };

    // "Show more": when the page is full (hits == limit) and we're
    // below the candidate-window ceiling, offer the next slice. Built
    // here so the query string is encoded correctly. `more_url` is the
    // no-JS fallback href; `more_limit` is the next limit the JS loader
    // requests from the JSON API to append in place.
    let (more_url, more_limit) =
        if !q.is_empty() && (hits.len() as i64) == limit && limit < search_service::HARD_LIMIT {
            let next = (limit + DEFAULT_LIMIT).min(search_service::HARD_LIMIT);
            let url = format!(
                "/search?q={}&kind={}&ai={}&limit={}",
                urlencoding::encode(&q),
                kind_label,
                if want_ai { 1 } else { 0 },
                next
            );
            (url, next)
        } else {
            (String::new(), 0)
        };

    let mut meta = PageMeta::new(&state.public_base_url, "/search")
        .with_title("Search")
        .with_description(
            "Hybrid semantic + keyword search across KrabArena \
             battles, claims, and agents.",
        );
    if search_should_noindex(&q, kind_label, params.limit) {
        meta = meta.noindex_follow();
    }
    let tmpl = SearchTemplate {
        meta,
        q,
        kind: kind_label.to_string(),
        hits,
        ai: want_ai,
        degraded,
        more_url,
        more_limit,
    };
    crate::web::errors::render_template_with_status_or_500(
        tmpl,
        StatusCode::OK,
        &state.public_base_url,
        "/search",
        "search.html",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn relative_age_relative_within_a_week_then_short_date() {
        let now = utc("2026-11-19T12:00:00Z");
        assert_eq!(relative_age(now, utc("2026-11-19T11:59:30Z")), "just now");
        assert_eq!(relative_age(now, utc("2026-11-19T11:15:00Z")), "45m ago");
        assert_eq!(relative_age(now, utc("2026-11-19T09:00:00Z")), "3h ago");
        assert_eq!(relative_age(now, utc("2026-11-13T12:00:01Z")), "5d ago");
        // Exactly 7 days and beyond: short date, year only when it differs.
        assert_eq!(relative_age(now, utc("2026-11-12T12:00:00Z")), "Nov 12");
        assert_eq!(relative_age(now, utc("2026-01-03T08:00:00Z")), "Jan 3");
        assert_eq!(
            relative_age(now, utc("2025-11-12T08:00:00Z")),
            "Nov 12, 2025"
        );
        // Clock skew (future timestamp) clamps to "just now".
        assert_eq!(relative_age(now, utc("2026-11-19T12:05:00Z")), "just now");
    }

    #[test]
    fn feed_time_carries_iso_and_exact_utc_title() {
        let now = utc("2026-11-19T12:00:00Z");
        let t = FeedTime::new("posted ", now, utc("2026-11-19T09:04:05.678Z"));
        assert_eq!(t.prefix, "posted ");
        assert_eq!(t.label, "2h ago");
        assert_eq!(t.iso, "2026-11-19T09:04:05Z");
        assert_eq!(t.title, "2026-11-19 09:04 UTC");
    }

    #[test]
    fn feed_time_for_battle_prefers_last_claim_over_opened() {
        let now = utc("2026-11-19T12:00:00Z");
        let opened = utc("2026-10-01T10:00:00Z");
        let fresh = FeedTime::for_battle(now, opened, None);
        assert_eq!(fresh.prefix, "opened ");
        assert_eq!(fresh.label, "Oct 1");
        assert_eq!(fresh.iso, "2026-10-01T10:00:00Z");
        assert_eq!(fresh.title, "Opened 2026-10-01 10:00 UTC");

        let active = FeedTime::for_battle(now, opened, Some(utc("2026-11-19T10:00:00Z")));
        assert_eq!(active.prefix, "last claim ");
        assert_eq!(active.label, "2h ago");
        assert_eq!(active.iso, "2026-11-19T10:00:00Z");
        assert_eq!(
            active.title,
            "Last claim 2026-11-19 10:00 UTC · opened 2026-10-01 10:00 UTC"
        );
    }

    /// Owner report (2026-11-19): the home feed showed no date/time on
    /// battle cards, and claim/verification ages weren't machine-readable.
    /// Every feed card must render a `<time datetime=… title=…>`.
    #[test]
    fn home_feed_cards_render_time_elements() {
        let now = utc("2026-11-19T12:00:00Z");
        let battle = BattleCard::build(
            Uuid::nil(),
            "Best search API?".into(),
            "desc".into(),
            "active".into(),
            vec![],
            1,
            0,
            0,
            None,
        )
        .with_feed_time(Some(FeedTime::for_battle(
            now,
            utc("2026-10-01T10:00:00Z"),
            Some(utc("2026-11-19T09:00:00Z")),
        )));
        let claim = FeedClaimCard {
            id: "c1".into(),
            claim_title: "Claim".into(),
            battle_title: String::new(),
            battle_slug: String::new(),
            author_display_name: None,
            agent_backend: String::new(),
            agent_model: String::new(),
            verify_count: 0,
            refute_count: 0,
            cost_label: String::new(),
            time: FeedTime::new("posted ", now, utc("2026-11-17T12:00:00Z")),
            topic: String::new(),
            topic_tags: vec![],
            produced_by: vec![],
            benchmark_label: String::new(),
            benchmark_metric: String::new(),
            leaderboard: vec![],
        };
        let verification = VerificationCard {
            claim_id: "c1".into(),
            claim_title: "Claim".into(),
            is_confirmed: false,
            author_display_name: None,
            agent_backend: String::new(),
            agent_model: String::new(),
            detail: String::new(),
            battle_title: String::new(),
            battle_slug: String::new(),
            time: FeedTime::new("", now, utc("2025-03-02T08:00:00Z")),
        };
        let render = |tab: &str| {
            HomeFeedPageTemplate {
                feed: HomeFeedView {
                    meta: PageMeta::new("http://localhost", "/"),
                    active_tab: tab.into(),
                    active_tags: vec![],
                    active_tag_value: String::new(),
                    active_q: String::new(),
                    current_page: 1,
                    battles: vec![battle.clone()],
                    claim_cards: vec![claim.clone()],
                    verification_cards: vec![verification.clone()],
                    active_status: None,
                    has_more: false,
                    next_url: String::new(),
                    tab_battles_url: String::new(),
                    tab_claims_url: String::new(),
                    tab_verifications_url: String::new(),
                    status_hot_url: String::new(),
                    status_open_url: String::new(),
                    status_active_url: String::new(),
                    status_settled_url: String::new(),
                    hidden_tab: String::new(),
                    hidden_status: String::new(),
                    hidden_sort: String::new(),
                    top_candidates: vec![],
                    total_candidates: 0,
                },
            }
            .render()
            .expect("renders")
        };
        let html = render("battles");
        assert!(html.contains(
            r#"<time class="feed-time" datetime="2026-11-19T09:00:00Z" title="Last claim 2026-11-19 09:00 UTC · opened 2026-10-01 10:00 UTC">last claim 3h ago</time>"#
        ), "{html}");
        let html = render("claims");
        assert!(html.contains(
            r#"<time class="feed-time" datetime="2026-11-17T12:00:00Z" title="Posted 2026-11-17 12:00 UTC">posted 2d ago</time>"#
        ), "{html}");
        let html = render("verifications");
        assert!(html.contains(
            r#"<time class="feed-time" datetime="2025-03-02T08:00:00Z" title="Refuted 2025-03-02 08:00 UTC">Mar 2, 2025</time>"#
        ), "{html}");
    }

    fn changelog_entry(version: &str) -> ChangelogEntry {
        ChangelogEntry {
            date: "June 1, 2026".to_string(),
            version: version.to_string(),
            headline_html: "headline".to_string(),
            body_html: String::new(),
        }
    }

    fn changelog_entries(n: usize) -> Vec<ChangelogEntry> {
        (0..n).map(|i| changelog_entry(&i.to_string())).collect()
    }

    #[test]
    fn preview_80_does_not_panic_on_multibyte_char_boundary() {
        // 79 ASCII bytes followed by a 3-byte UTF-8 char ('—' at byte
        // offset 79..82) — byte-slicing at [..80] would panic because
        // byte 80 falls inside the multi-byte character. This
        // reproduces the prod 503 on agent profile pages.
        let s = format!("{}—{}", "a".repeat(79), "more text after the dash");
        let preview = preview_80(&s); // must not panic
        assert!(
            preview.chars().count() <= 80,
            "preview should be at most 80 chars, got {}",
            preview.chars().count()
        );
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn preview_80_passes_short_strings_through_unchanged() {
        let s = "short claim text";
        assert_eq!(preview_80(s), s);
    }

    #[test]
    fn paginate_changelog_empty_is_single_page() {
        let p = paginate_changelog(&[], None, 15);
        assert!(p.entries.is_empty());
        assert_eq!(p.current_page, 1);
        assert_eq!(p.total_pages, 1);
        assert!(!p.has_more);
        assert_eq!(p.next_page, 2);
    }

    #[test]
    fn paginate_changelog_first_page_has_more() {
        let all = changelog_entries(46);
        let p = paginate_changelog(&all, None, 15);
        assert_eq!(p.current_page, 1);
        assert_eq!(p.total_pages, 4); // ceil(46 / 15)
        assert_eq!(p.entries.len(), 15);
        assert!(p.has_more);
        assert_eq!(p.next_page, 2);
        assert_eq!(p.entries[0].version, "0");
        assert_eq!(p.entries[14].version, "14");
    }

    #[test]
    fn paginate_changelog_second_page_slice() {
        let all = changelog_entries(46);
        let p = paginate_changelog(&all, Some(2), 15);
        assert_eq!(p.entries.len(), 15);
        // Page 2 = entries 16–30 (1-based) = indices 15–29.
        assert_eq!(p.entries[0].version, "15");
        assert_eq!(p.entries[14].version, "29");
    }

    #[test]
    fn paginate_changelog_last_page_is_partial_and_final() {
        let all = changelog_entries(46);
        let p = paginate_changelog(&all, Some(4), 15);
        assert_eq!(p.current_page, 4);
        assert_eq!(p.entries.len(), 1); // 46 - 45
        assert!(!p.has_more);
    }

    #[test]
    fn paginate_changelog_clamps_out_of_range_pages() {
        let all = changelog_entries(46);
        assert_eq!(paginate_changelog(&all, Some(0), 15).current_page, 1);
        assert_eq!(paginate_changelog(&all, Some(-5), 15).current_page, 1);
        assert_eq!(paginate_changelog(&all, Some(999), 15).current_page, 4);
    }

    #[test]
    fn changelog_caches_parse_non_empty() {
        assert!(!CHANGELOG_PUBLIC.is_empty());
    }

    #[test]
    fn canonical_slug_location_preserves_battle_utm_query() {
        let query = "utm_source=github&utm_medium=issue&utm_campaign=github_issues_outreach&utm_content=7e7ab1e4-91a2-47e2-b046-8bfbfe1e6824&utm_term=mcp%20server";

        assert_eq!(
            canonical_slug_location(
                "/battles",
                "claude-skills-vs-mcp-servers-which-tool-delivery-earns-its-context-budget",
                Some(query),
            ),
            format!(
                "/battles/claude-skills-vs-mcp-servers-which-tool-delivery-earns-its-context-budget?{query}"
            )
        );
    }

    #[test]
    fn canonical_slug_location_preserves_claim_query() {
        let query = "utm_source=github&utm_medium=discussion&utm_campaign=github_claim_invitation&utm_content=2cde0a8a-28f4-47b9-aa46-7af18bc532c1&utm_term=Claude%20Skill";

        assert_eq!(
            canonical_slug_location("/claims", "claude-skill-integration-claim", Some(query),),
            format!("/claims/claude-skill-integration-claim?{query}")
        );
    }

    #[test]
    fn canonical_slug_location_omits_empty_query() {
        assert_eq!(
            canonical_slug_location("/battles", "rust-vs-go", None),
            "/battles/rust-vs-go"
        );
        assert_eq!(
            canonical_slug_location("/claims", "rust-vs-go-claim", Some("")),
            "/claims/rust-vs-go-claim"
        );
    }

    #[test]
    fn markdown_suffix_forces_markdown_and_keeps_slug() {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        let (slug, h2, md) = split_markdown_suffix("mem0-vs-cognee.md".to_string(), h.clone());
        assert_eq!(slug, "mem0-vs-cognee");
        assert!(md);
        assert!(wants_markdown(&h2));
        let (slug, h3, md) = split_markdown_suffix("mem0-vs-cognee".to_string(), h);
        assert_eq!(slug, "mem0-vs-cognee");
        assert!(!md);
        assert!(!wants_markdown(&h3));
        // A bare ".md" is not a slug — leave it alone (404s normally).
        let (slug, _, md) = split_markdown_suffix(".md".to_string(), HeaderMap::new());
        assert_eq!(slug, ".md");
        assert!(!md);
        assert_eq!(markdown_suffixed("x", true), "x.md");
        assert_eq!(markdown_suffixed("x", false), "x");
    }

    #[test]
    fn canonical_link_header_only_on_ok() {
        let ok = with_canonical_link(
            (StatusCode::OK, "body").into_response(),
            "https://krabarena.com/claims/x",
        );
        assert_eq!(
            ok.headers().get(header::LINK).and_then(|v| v.to_str().ok()),
            Some("<https://krabarena.com/claims/x>; rel=\"canonical\"")
        );
        let nf = with_canonical_link(
            (StatusCode::NOT_FOUND, "nope").into_response(),
            "https://krabarena.com/claims/x",
        );
        assert!(nf.headers().get(header::LINK).is_none());
    }

    #[test]
    fn canonical_slug_redirect_query_response_is_not_cacheable() {
        let response = canonical_slug_redirect("/battles", "rust-vs-go", Some("utm_source=github"));
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/battles/rust-vs-go?utm_source=github")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );

        let response = canonical_slug_redirect("/battles", "rust-vs-go", None);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            None,
            "clean canonical redirects stay cacheable"
        );
    }

    #[test]
    fn wins_pie_colors_stay_distinct_on_letter_bucket_collision() {
        // Regression guard: on the Skills-vs-MCP battle the two
        // solutions "Claude Skill" (C) and "MCP server" (M) both
        // resolved to avatar-palette index 2 (`#e9c46a`) via
        // `(letter - 'A') % 10`, so the pie painted two identical
        // yellow slices that merged together. Feed the same colliding
        // colours in and assert the resolver hands back distinct ones.
        let (_, claude_color, _) = leaderboard_logo_style("Claude Skill");
        let (_, mcp_color, _) = leaderboard_logo_style("MCP server");
        assert_eq!(
            claude_color, mcp_color,
            "precondition: the avatar palette must still collide C/M"
        );

        let colors = distinct_wins_colors(&[claude_color.as_str(), mcp_color.as_str()]);
        assert_eq!(colors.len(), 2);
        assert_eq!(
            colors[0], claude_color,
            "first slice keeps its preferred colour"
        );
        assert_ne!(
            colors[0], colors[1],
            "colliding slices must end up different colours"
        );

        // A distinct preferred colour (e.g. a real brand_color) is
        // preserved untouched, and empty entries fall back to the
        // index palette without colliding with neighbours.
        let mixed = distinct_wins_colors(&["#112233", "#112233", ""]);
        assert_eq!(mixed[0], "#112233");
        assert_ne!(mixed[1], mixed[0]);
        assert_ne!(mixed[2], mixed[0]);
        assert_ne!(mixed[2], mixed[1]);
    }

    #[test]
    fn contrast_text_picks_legible_color() {
        // Light backgrounds → dark text; dark backgrounds → light text.
        assert_eq!(contrast_text("#e9c46a"), "#1a1a18"); // yellow avatar
        assert_eq!(contrast_text("#dea584"), "#1a1a18"); // tan substitute
        assert_eq!(contrast_text("#264653"), "#fff"); // slate
        assert_eq!(contrast_text("#663399"), "#fff"); // purple substitute
        // Non-hex (CSS var) and malformed input fall back to white.
        assert_eq!(contrast_text("var(--ink-3)"), "#fff");
        assert_eq!(contrast_text("#fff"), "#fff");
    }

    #[test]
    fn home_query_state_normalizes_conservatively() {
        assert_eq!(normalize_home_tab(None), "battles");
        assert_eq!(normalize_home_tab(Some("claims")), "claims");
        assert_eq!(normalize_home_tab(Some("verifications")), "verifications");
        assert_eq!(normalize_home_tab(Some("unknown")), "battles");

        assert_eq!(normalize_home_sort(None), "hot");
        assert_eq!(normalize_home_sort(Some("new")), "new");
        assert_eq!(normalize_home_sort(Some("weird")), "hot");

        assert_eq!(normalize_home_page(None), 1);
        assert_eq!(normalize_home_page(Some(-9)), 1);
        assert_eq!(normalize_home_page(Some(2)), 2);
        assert_eq!(
            normalize_home_page(Some(HOME_FEED_MAX_PAGE + 10)),
            HOME_FEED_MAX_PAGE
        );

        assert_eq!(
            normalize_home_tags(Some(" Rust,ai, rust ,,Search ")),
            vec!["rust".to_string(), "ai".to_string(), "search".to_string()]
        );
        assert_eq!(
            query_tags(&parse_home_query(Some(
                "tag=rust&tag=ai&tag=rust&tag=dev%20ops"
            ))),
            vec!["rust".to_string(), "ai".to_string(), "dev ops".to_string()]
        );
        assert_eq!(
            query_q(&parse_home_query(Some("q=python+type+check"))),
            "python type check".to_string()
        );
    }

    #[test]
    fn seo_home_default_state_is_indexable_only_without_filters() {
        let query = HomeQuery::default();
        assert!(home_is_indexable_default_state(&query, &[], ""));

        let query = HomeQuery {
            tab: Some("claims".to_string()),
            ..HomeQuery::default()
        };
        assert!(!home_is_indexable_default_state(&query, &[], ""));

        let query = HomeQuery {
            sort: Some("new".to_string()),
            ..HomeQuery::default()
        };
        assert!(!home_is_indexable_default_state(&query, &[], ""));

        assert!(!home_is_indexable_default_state(
            &HomeQuery::default(),
            &["e2e".to_string()],
            ""
        ));
        assert!(!home_is_indexable_default_state(
            &HomeQuery::default(),
            &[],
            "admin"
        ));
    }

    #[test]
    fn seo_search_query_states_are_noindex() {
        assert!(!search_should_noindex("", "all", None));
        assert!(search_should_noindex("krab", "all", None));
        assert!(search_should_noindex("", "claims", None));
        assert!(search_should_noindex("", "all", Some(25)));
    }

    #[test]
    fn home_url_preserves_non_default_state() {
        let tags = vec!["rust".to_string(), "ai search".to_string()];
        let url = home_url("claims", Some("open"), &tags, "foo bar", "new", 3);
        let parsed = reqwest::Url::parse(&format!("https://example.test{url}"))
            .expect("parse generated home URL");
        assert_eq!(parsed.path(), "/");
        let query_pairs: Vec<_> = parsed
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            query_pairs,
            vec![
                ("tab".to_string(), "claims".to_string()),
                ("status".to_string(), "open".to_string()),
                ("tag".to_string(), "rust,ai search".to_string()),
                ("q".to_string(), "foo bar".to_string()),
                ("sort".to_string(), "new".to_string()),
                ("page".to_string(), "3".to_string()),
            ]
        );
        assert_eq!(home_url("battles", None, &[], "", "hot", 1), "/");
    }

    #[test]
    fn home_tag_suggestion_helpers_use_last_word_only() {
        assert_eq!(suggestion_prefix("python typ"), "typ");
        assert_eq!(suggestion_prefix("  rust  "), "rust");
        assert_eq!(query_without_last_token("python typ"), "python");
        assert_eq!(query_without_last_token("solo"), "");
        assert_eq!(query_without_last_token("python   typ  "), "python");
    }

    #[test]
    fn strip_table_cell_parens_only_targets_table_rows() {
        let md = "Some prose with parens (kept).\n\
                  | Rank | Solution | Score (raw) |\n\
                  |------|----------|-------------|\n\
                  | 1    | Parallel | 84.0 (95.5%) |\n\
                  | 2    | Exa (deprecated) | 82.0 (93.2%) |\n\
                  \n\
                  More prose (also kept).";
        let out = strip_table_cell_parens(md);
        assert!(out.contains("Some prose with parens (kept)."));
        assert!(out.contains("More prose (also kept)."));
        assert!(out.contains("| Rank | Solution | Score |"));
        assert!(out.contains("| 1    | Parallel | 84.0 |"));
        assert!(out.contains("| 2    | Exa | 82.0 |"));
    }

    // Bug #2 regression (discovery redesign, 0.23.0): a battle-attached
    // claim whose Battle had a NULL slug used to render with an empty
    // badge target, so the `b/<…>` badge was suppressed and the claim
    // looked standalone in the feed. The badge resolver must always
    // produce a non-empty target when the claim has a battle.
    #[test]
    fn battle_attached_claim_never_renders_as_standalone() {
        let battle_id = Uuid::from_u128(0x1234);

        // Battle with a slug → badge uses the slug.
        assert_eq!(
            resolve_claim_battle_badge(Some(battle_id), Some("my-battle".to_string())),
            "my-battle"
        );

        // Battle with a NULL slug → badge falls back to the UUID, NOT
        // empty (this is the exact bug-#2 condition).
        let badge = resolve_claim_battle_badge(Some(battle_id), None);
        assert!(
            !badge.is_empty(),
            "battle-attached claim must keep a badge even when the battle slug is NULL"
        );
        assert_eq!(badge, battle_id.to_string());

        // Genuinely standalone claim (no battle) → empty, so the
        // template renders the standalone treatment.
        assert_eq!(resolve_claim_battle_badge(None, None), "");
        assert_eq!(
            resolve_claim_battle_badge(None, Some("ignored".to_string())),
            ""
        );
    }

    #[test]
    fn token_formatter_handles_thousands_and_millions() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1_500), "1.5K");
        assert_eq!(format_token_count(1_500_000), "1.5M");
        assert_eq!(format_token_count(2_345_678), "2.3M");
    }

    #[test]
    fn rollup_formats_dollars_with_four_decimals() {
        let r = ReviewCostRollup {
            author_tokens: 32_000,
            author_cost_usd: 1.50,
            external_cost_usd: 0.5,
            api_tokens: 0,
            api_cost_usd: 0.0123,
        };
        // Cost formatters round to 2 decimals (rebrand 0.9.0).
        assert_eq!(r.krabreview_cost_str(), "$0.01");
        assert_eq!(r.author_cost_str(), "$1.50");
        assert_eq!(r.external_cost_str(), "$0.50");
        // Combined "true cost" surface — author + external + API.
        assert_eq!(r.total_cost_str(), "$2.01");
    }

    #[test]
    fn cost_label_handles_tokens_and_dollars_with_note() {
        use krabarena_shared::claim_structure::ClaimCost;
        let label = format_cost_label(&ClaimCost {
            tokens: Some(32_000),
            usd: Some(0.84),
            external_usd: None,
            note: Some("Claude Opus 4.7".into()),
        })
        .unwrap();
        assert_eq!(label, "32,000 tokens · $0.8400 (Claude Opus 4.7)");
    }

    #[test]
    fn review_cost_label_combines_tokens_and_dollars() {
        // Both fields populated — render "12,345 tokens · $0.0125".
        let mut v = ClaimCardView {
            id: String::new(),
            claim: String::new(),
            claim_title: String::new(),
            claim_body: String::new(),
            created_relative: String::new(),
            updated_relative: None,
            effort_usd: 0.0,
            attachments: Vec::new(),
            artifact_summary: String::new(),
            og_summary: None,
            verify_count: 0,
            refute_count: 0,
            verify_pct: 0,
            refute_pct: 0,
            upvote_count: 0,
            downvote_count: 0,
            agent_backend: String::new(),
            agent_initials: "?".into(),
            agent_model: String::new(),
            visibility: String::new(),
            price_cents: None,
            tokens_consumed: None,
            wall_clock_ms: None,
            benchmark: None,
            leaderboard: Vec::new(),
            winners_label: String::new(),
            review_tokens_total: Some(12_345),
            review_cost_usd: Some(0.0125),
            external_cost_usd: None,
            topic: None,
            tags_label: String::new(),
            author_display_name: "Unknown".into(),
            author_avatar_url: None,
            author_initials: "?".into(),
            produced_by: Vec::new(),
        };
        assert_eq!(v.review_cost_label().unwrap(), "12,345 tokens · $0.0125");
        // Tokens only — render dash for the missing dollar half.
        v.review_cost_usd = None;
        assert_eq!(v.review_cost_label().unwrap(), "12,345 tokens · —");
        // Zero tokens — treat as legacy/no-data, suppress entirely.
        v.review_tokens_total = Some(0);
        assert!(v.review_cost_label().is_none());
        // Both absent — also None.
        v.review_tokens_total = None;
        assert!(v.review_cost_label().is_none());
    }

    #[test]
    fn cost_label_returns_none_for_note_only() {
        use krabarena_shared::claim_structure::ClaimCost;
        let label = format_cost_label(&ClaimCost {
            tokens: None,
            usd: None,
            external_usd: None,
            note: Some("just a note".into()),
        });
        assert!(label.is_none());
    }

    #[test]
    fn arena_row_cost_merge_by_backend_model() {
        // Build two ArenaRow values and a cost-by-(backend, model)
        // index, then verify `apply_cost` propagates the rollup
        // values onto the matching ArenaRow.
        let mut rows = [
            ArenaRow {
                agent_backend: "Claude Code".into(),
                agent_model: "claude-opus-4-7".into(),
                total_verify: 5,
                total_claims: 3,
                total_refute: 1,
                author_tokens: 0,
                author_cost_usd: 0.0,
                external_cost_usd: 0.0,
                krabreview_cost_usd: 0.0,
            },
            ArenaRow {
                agent_backend: "OpenClaw".into(),
                agent_model: "unknown".into(),
                total_verify: 2,
                total_claims: 1,
                total_refute: 0,
                author_tokens: 0,
                author_cost_usd: 0.0,
                external_cost_usd: 0.0,
                krabreview_cost_usd: 0.0,
            },
        ];

        let cost_rows = [
            CostByBackendRow {
                backend: "Claude Code".into(),
                model: "claude-opus-4-7".into(),
                claim_count: 3,
                author_tokens: 12_345,
                author_cost_usd: 0.42,
                external_cost_usd: 0.10,
                api_tokens: 9_999,
                api_cost_usd: 0.0033,
            },
            // Mirrors how cost_by_backend_query returns "" for null
            // model — the merge logic should normalize this to
            // "unknown" so it joins against the entries query.
            CostByBackendRow {
                backend: "OpenClaw".into(),
                model: "".into(),
                claim_count: 1,
                author_tokens: 100,
                author_cost_usd: 0.01,
                external_cost_usd: 0.0,
                api_tokens: 0,
                api_cost_usd: 0.0,
            },
        ];

        let cost_index: std::collections::HashMap<(String, String), CostByBackendRow> = cost_rows
            .into_iter()
            .map(|c| {
                let model = if c.model.is_empty() {
                    "unknown".to_string()
                } else {
                    c.model.clone()
                };
                ((c.backend.clone(), model), c)
            })
            .collect();

        for entry in rows.iter_mut() {
            if let Some(c) =
                cost_index.get(&(entry.agent_backend.clone(), entry.agent_model.clone()))
            {
                entry.apply_cost(c);
            }
        }

        assert_eq!(rows[0].author_tokens, 12_345);
        assert_eq!(rows[0].author_cost_str(), "$0.42");
        assert_eq!(rows[0].external_cost_str(), "$0.10");
        assert_eq!(rows[0].krabreview_cost_str(), "$0.00");
        assert_eq!(rows[0].total_cost_str(), "$0.52");
        // Unknown model normalization: the empty-string cost row gets
        // joined against the "unknown" entries-side bucket.
        assert_eq!(rows[1].author_tokens, 100);
        assert_eq!(rows[1].author_cost_str(), "$0.01");
    }

    #[test]
    fn battle_card_total_tokens_str_buckets_correctly() {
        let mut card = BattleCard::build(
            Uuid::nil(),
            "title".into(),
            "desc".into(),
            "active".into(),
            vec![],
            1,
            0,
            0,
            None,
        );
        // Zero — chip suppressed.
        assert!(card.total_tokens_str().is_none());
        card.agg_tokens = 999;
        assert_eq!(card.total_tokens_str().unwrap(), "999");
        card.agg_tokens = 312_000;
        assert_eq!(card.total_tokens_str().unwrap(), "312.0K");
        card.agg_tokens = 1_500_000;
        assert_eq!(card.total_tokens_str().unwrap(), "1.5M");
    }

    #[test]
    fn battle_card_total_cost_str_two_or_four_decimals() {
        let mut card = BattleCard::build(
            Uuid::nil(),
            "title".into(),
            "desc".into(),
            "active".into(),
            vec![],
            1,
            0,
            0,
            None,
        );
        // Zero / negative → None.
        assert!(card.total_cost_str().is_none());
        card.agg_cost_usd = -0.01;
        assert!(card.total_cost_str().is_none());
        // < $1 → 4 decimals.
        card.agg_cost_usd = 0.51;
        assert_eq!(card.total_cost_str().unwrap(), "$0.5100");
        // >= $1 → 2 decimals.
        card.agg_cost_usd = 31.555;
        assert_eq!(card.total_cost_str().unwrap(), "$31.55");
    }

    #[test]
    fn claim_card_view_chip_helpers_suppress_zero_and_missing() {
        let mut v = ClaimCardView {
            id: String::new(),
            claim: String::new(),
            claim_title: String::new(),
            claim_body: String::new(),
            created_relative: String::new(),
            updated_relative: None,
            effort_usd: 0.0,
            attachments: Vec::new(),
            artifact_summary: String::new(),
            og_summary: None,
            verify_count: 0,
            refute_count: 0,
            verify_pct: 0,
            refute_pct: 0,
            upvote_count: 0,
            downvote_count: 0,
            agent_backend: String::new(),
            agent_initials: "?".into(),
            agent_model: String::new(),
            visibility: String::new(),
            price_cents: None,
            tokens_consumed: None,
            wall_clock_ms: None,
            benchmark: None,
            leaderboard: Vec::new(),
            winners_label: String::new(),
            review_tokens_total: None,
            review_cost_usd: None,
            external_cost_usd: None,
            topic: None,
            tags_label: String::new(),
            author_display_name: "Unknown".into(),
            author_avatar_url: None,
            author_initials: "?".into(),
            produced_by: Vec::new(),
        };
        // All None → all helpers suppress.
        assert!(v.tokens_str().is_none());
        assert!(v.author_cost_str().is_none());
        assert!(v.external_cost_str().is_none());
        assert!(v.krabreview_cost_str().is_none());

        // Zero → still suppressed (mixed-data legacy guard).
        v.tokens_consumed = Some(0);
        v.price_cents = Some(0);
        v.external_cost_usd = Some(0.0);
        v.review_cost_usd = Some(0.0);
        assert!(v.tokens_str().is_none());
        assert!(v.author_cost_str().is_none());
        assert!(v.external_cost_str().is_none());
        assert!(v.krabreview_cost_str().is_none());

        // Populated → comma-formatted tokens + 4-decimal dollars.
        v.tokens_consumed = Some(123_456);
        v.price_cents = Some(500); // $5.0000
        v.external_cost_usd = Some(0.51);
        v.review_cost_usd = Some(0.0125);
        assert_eq!(v.tokens_str().unwrap(), "123,456");
        assert_eq!(v.author_cost_str().unwrap(), "$5.0000");
        assert_eq!(v.external_cost_str().unwrap(), "$0.5100");
        assert_eq!(v.krabreview_cost_str().unwrap(), "$0.0125");
    }

    #[test]
    fn project_external_cost_extracts_from_structure_jsonb() {
        let base = serde_json::json!({
            "schema_version": 1,
            "benchmark": {
                "name": "n", "task": "t", "dataset": "d", "metric": "m",
                "higher_is_better": true, "setup": "s"
            },
            "leaderboard": [],
            "winners": []
        });
        // None → None.
        assert!(ClaimCardView::project_external_cost(None).is_none());
        // No `cost` block → None.
        assert!(ClaimCardView::project_external_cost(Some(&base)).is_none());
        // Zero external_usd → None (suppression rule).
        let mut zero = base.clone();
        zero["cost"] = serde_json::json!({"external_usd": 0.0});
        assert!(ClaimCardView::project_external_cost(Some(&zero)).is_none());
        // Positive → Some.
        let mut positive = base.clone();
        positive["cost"] = serde_json::json!({"external_usd": 0.51});
        let extracted = ClaimCardView::project_external_cost(Some(&positive)).unwrap();
        assert!((extracted - 0.51).abs() < 1e-9);
    }

    #[test]
    fn cost_label_renders_partial_fields() {
        use krabarena_shared::claim_structure::ClaimCost;
        let label = format_cost_label(&ClaimCost {
            tokens: Some(123),
            usd: None,
            external_usd: None,
            note: None,
        })
        .unwrap();
        assert_eq!(label, "123 tokens");

        let label = format_cost_label(&ClaimCost {
            tokens: None,
            usd: Some(2.5),
            external_usd: None,
            note: None,
        })
        .unwrap();
        // >= $1 path drops to two-decimal precision.
        assert_eq!(label, "$2.50");
    }

    fn lb_row(rank: u32, solution: &str, logo_url: Option<&str>) -> LeaderboardRowView {
        LeaderboardRowView {
            rank,
            solution: solution.to_string(),
            score_label: "100".into(),
            score: 100.0,
            notes: String::new(),
            evidence_url: None,
            is_winner: rank == 1,
            logo_initials: "X".into(),
            logo_color: "#000".into(),
            logo_text_color: "#fff".into(),
            logo_url: logo_url.map(|s| s.to_string()),
        }
    }

    /// Battle's `solutions` curated entry wins over the per-row
    /// `logo_url` set inline on a claim's structure.json. This is
    /// the headline guarantee of the 0.13.0 feature — claims on the
    /// same Battle render identical logos for identical solutions.
    #[test]
    fn apply_battle_logos_overrides_per_row_inline() {
        use crate::models::battle::BattleSolution;
        let battle_solutions = vec![BattleSolution {
            name: "Browserless".into(),
            name_normalized: "browserless".into(),
            logo_url: Some("https://battle/browserless.png".into()),
            website_url: Some("https://browserless.io".into()),
            brand_color: None,
        }];
        let mut rows = vec![lb_row(
            1,
            "Browserless v2 (Chromium)",
            Some("https://row/legacy.png"),
        )];
        apply_battle_logos(&battle_solutions, &mut rows);
        // Battle wins. (Note: normalisation strips the paren suffix
        // and lowercases — "browserless v2" matches a curated key of
        // "browserless v2" if present. Here we test the case where
        // the curated key is the bare "browserless" — see the next
        // test for the exact-match case. This row WON'T be re-keyed
        // because "browserless v2" != "browserless".)
        assert_eq!(
            rows[0].logo_url.as_deref(),
            Some("https://row/legacy.png"),
            "no exact-match curated entry — per-row logo_url preserved"
        );
    }

    /// Exact-match case: the curated key matches the normalised
    /// solution, so the Battle's logo replaces the per-row one.
    #[test]
    fn apply_battle_logos_exact_match_wins() {
        use crate::models::battle::BattleSolution;
        let battle_solutions = vec![BattleSolution {
            name: "Browserless".into(),
            name_normalized: "browserless".into(),
            // GCP-only: curated logo must be self-hosted to be eligible.
            logo_url: Some(
                "https://storage.googleapis.com/krabarena-artifacts/logos/browserless.io.png"
                    .into(),
            ),
            website_url: None,
            brand_color: None,
        }];
        let mut rows = vec![lb_row(1, "Browserless", Some("https://row/legacy.png"))];
        apply_battle_logos(&battle_solutions, &mut rows);
        assert_eq!(
            rows[0].logo_url.as_deref(),
            Some("https://storage.googleapis.com/krabarena-artifacts/logos/browserless.io.png"),
            "Battle's curated (self-hosted) logo must override per-row inline"
        );
    }

    /// Lookup precedence — row with NO inline logo + no Battle entry
    /// = `None` (template falls back to coloured-initials avatar).
    /// Row with NO inline logo + Battle entry = Battle wins.
    /// Row with inline logo + NO Battle match = inline survives
    /// (standalone-claim fallback).
    #[test]
    fn apply_battle_logos_precedence_chain() {
        use crate::models::battle::BattleSolution;
        let battle_solutions = vec![BattleSolution {
            name: "Tavily".into(),
            name_normalized: "tavily".into(),
            logo_url: Some(
                "https://storage.googleapis.com/krabarena-artifacts/logos/tavily.com.png".into(),
            ),
            website_url: None,
            brand_color: None,
        }];
        let exa_inline = "https://storage.googleapis.com/krabarena-artifacts/logos/exa.ai.png";
        let mut rows = vec![
            lb_row(1, "Tavily", None),          // Battle wins
            lb_row(2, "Exa", Some(exa_inline)), // inline (self-hosted) survives
            lb_row(3, "Unknown", None),         // stays None
        ];
        apply_battle_logos(&battle_solutions, &mut rows);
        assert_eq!(
            rows[0].logo_url.as_deref(),
            Some("https://storage.googleapis.com/krabarena-artifacts/logos/tavily.com.png")
        );
        assert_eq!(rows[1].logo_url.as_deref(), Some(exa_inline));
        assert_eq!(rows[2].logo_url, None);
    }

    /// Empty Battle solutions list — no-op (covers the early-return
    /// path so an empty `battles.solutions` JSONB doesn't blow up).
    #[test]
    fn apply_battle_logos_empty_solutions_noop() {
        let mut rows = vec![lb_row(1, "Tavily", Some("https://row/tavily.png"))];
        apply_battle_logos(&[], &mut rows);
        assert_eq!(rows[0].logo_url.as_deref(), Some("https://row/tavily.png"));
    }

    /// Vendor-logo fallback (PR-C): standalone-claim rows with no logo
    /// get the `vendor_logos` favicon; rows that already have one (battle
    /// or inline) are left untouched; unknown vendors stay `None`.
    #[test]
    fn apply_vendor_logo_fallback_fills_only_uncovered_rows() {
        use crate::services::solution_names::CompiledVendor;
        use regex::Regex;
        let tavily_stored =
            "https://storage.googleapis.com/krabarena-artifacts/logos/tavily.com.png";
        let patterns: Vec<CompiledVendor> = vec![
            CompiledVendor {
                regex: Regex::new("(?i)tavily").unwrap(),
                domain: "tavily.com".to_string(),
                // GCP-only: fallback fills from the self-hosted stored_url.
                stored_url: Some(tavily_stored.to_string()),
                brand_color: None,
            },
            CompiledVendor {
                regex: Regex::new("(?i)exa").unwrap(),
                domain: "exa.ai".to_string(),
                // No self-hosted asset → no logo (never an external favicon).
                stored_url: None,
                brand_color: None,
            },
        ];
        let mut rows = vec![
            lb_row(1, "Tavily", None), // filled from GCP stored_url
            lb_row(2, "Exa", Some("https://battle/exa.png")), // already set → kept
            lb_row(3, "MysteryTool", None), // no match → stays None
        ];
        apply_vendor_logo_fallback(&patterns, &mut rows);
        assert_eq!(rows[0].logo_url.as_deref(), Some(tavily_stored));
        assert_eq!(rows[1].logo_url.as_deref(), Some("https://battle/exa.png"));
        assert_eq!(rows[2].logo_url, None);
        // Empty patterns → no-op.
        let mut r2 = vec![lb_row(1, "Tavily", None)];
        apply_vendor_logo_fallback(&[], &mut r2);
        assert_eq!(r2[0].logo_url, None);
    }

    // --- Wins matrix (solutions × claims) — 0.13.4 rewrite ---

    /// Build a fake `ClaimWithAuthor` with the given `claim_structure`
    /// JSON, `created_at`, and `id`. The aggregator only reads
    /// `row.slug`, `row.id`, `row.created_at`, and
    /// `row.claim_structure` — everything else can stay at default.
    fn fake_claim(
        id: Uuid,
        created_at: chrono::DateTime<chrono::Utc>,
        structure: serde_json::Value,
    ) -> ClaimWithAuthor {
        ClaimWithAuthor {
            row: crate::models::claim::ClaimRow {
                id,
                battle_id: None,
                author_id: Uuid::nil(),
                claim: String::new(),
                artifact_hash: String::new(),
                artifact_url: String::new(),
                artifact_size: 0,
                predecessor_id: None,
                verify_count: 0,
                refute_count: 0,
                created_at,
                updated_at: created_at,
                agent_backend: None,
                agent_model: None,
                upvote_count: 0,
                downvote_count: 0,
                comment_count: 0,
                review_id: None,
                warnings_ignored: false,
                artifact_summary: None,
                artifact_highlights: None,
                tokens_consumed: None,
                wall_clock_ms: None,
                env_hash: None,
                visibility: Some("public".into()),
                price_cents: None,
                slug: None,
                artifact_filename: None,
                private_artifact_url: None,
                private_artifact_filename: None,
                og_summary: None,
                claim_structure: Some(structure),
                winners: None,
                topic: None,
                topic_tags: None,
                deleted_at: None,
                produced_by: None,
                presentation: None,
            },
            author_display_name: None,
            author_avatar_url: None,
        }
    }

    /// Project a `ClaimWithAuthor` into the matching `ClaimCardView`
    /// the aggregator walks (it reads `view.leaderboard` for the
    /// row-by-row pass).
    fn fake_view(c: &ClaimWithAuthor) -> ClaimCardView {
        ClaimCardView::from_row_with_author(&c.row, None, None)
    }

    /// Rewritten in 0.13.4 (second pass): aggregation is now a true
    /// solutions × claims matrix. One row per distinct normalised
    /// solution; one column per claim that has a leaderboard. Cell
    /// value = the rank that solution achieved in that claim, or
    /// `None` if the solution didn't appear.
    ///
    /// The fixture uses 3 claims and 4 solutions:
    ///   - Claim c_a (oldest, 2026-01-01): A=1, B=2, C=3
    ///   - Claim c_b (mid,    2026-02-01): A=1, B=2,    D=3
    ///   - Claim c_c (newest, 2026-03-01):       B=1, C=2, D=3
    ///
    /// Newest-first column order is c_c, c_b, c_a. So:
    ///   - A: ranks = [None, Some(1), Some(1)]  wins=2 appearances=2
    ///   - B: ranks = [Some(1), Some(2), Some(2)]  wins=1 appearances=3
    ///   - C: ranks = [Some(2), None, Some(3)]  wins=0 appearances=2
    ///   - D: ranks = [Some(3), Some(3), None]  wins=0 appearances=2
    ///
    /// Default sort: A (wins=2) first, then B (wins=1), then C/D
    /// (wins=0, appearances=2, alphabetical → C before D).
    #[test]
    fn build_wins_matrix_solutions_times_claims() {
        use chrono::{TimeZone, Utc};
        let id_a = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let id_b = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let id_c = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        let s_a = serde_json::json!({
            "schema_version": 1,
            "benchmark": {"name":"alpha","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
            "winners": ["A"],
            "leaderboard": [
                {"rank": 1, "solution": "A", "score": 0.9},
                {"rank": 2, "solution": "B", "score": 0.8},
                {"rank": 3, "solution": "C", "score": 0.7}
            ]
        });
        let s_b = serde_json::json!({
            "schema_version": 1,
            "benchmark": {"name":"beta","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
            "winners": ["A"],
            "leaderboard": [
                {"rank": 1, "solution": "A", "score": 0.9},
                {"rank": 2, "solution": "B", "score": 0.8},
                {"rank": 3, "solution": "D", "score": 0.6}
            ]
        });
        let s_c = serde_json::json!({
            "schema_version": 1,
            "benchmark": {"name":"gamma","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
            "winners": ["B"],
            "leaderboard": [
                {"rank": 1, "solution": "B", "score": 0.95},
                {"rank": 2, "solution": "C", "score": 0.8},
                {"rank": 3, "solution": "D", "score": 0.6}
            ]
        });
        let c_a = fake_claim(
            id_a,
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            s_a,
        );
        let c_b = fake_claim(
            id_b,
            Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap(),
            s_b,
        );
        let c_c = fake_claim(
            id_c,
            Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(),
            s_c,
        );
        // Pass in arbitrary input order — the aggregator sorts
        // newest-first internally.
        let views: Vec<ClaimCardView> = [&c_a, &c_b, &c_c].iter().map(|c| fake_view(*c)).collect();
        let claims_with_author = vec![c_a, c_b, c_c];
        let matrix = build_wins_matrix(
            &views,
            &claims_with_author,
            &[],
            &PresentationConfig::default(),
        );

        // --- ASSERT (a): shape — 4 solution rows × 3 claim columns.
        assert_eq!(matrix.rows.len(), 4, "expected 4 solution rows");
        assert_eq!(matrix.claim_count, 3, "claim_count must equal #claims");
        assert_eq!(matrix.claims.len(), 3, "expected 3 claim columns");
        for row in &matrix.rows {
            assert_eq!(
                row.ranks.len(),
                3,
                "row {} ranks must be 3-wide (one per claim column)",
                row.solution_name
            );
        }

        // --- ASSERT (b): claim ordering is newest-first by created_at.
        // c_c (id_c, 2026-03-01) is the newest, then c_b, then c_a.
        assert_eq!(
            matrix.claims[0].id, id_c,
            "column 0 must be the newest claim"
        );
        assert_eq!(matrix.claims[1].id, id_b);
        assert_eq!(matrix.claims[2].id, id_a);

        // --- ASSERT (c): the solution that wins claim c_b (id_b) AND
        // claim c_a (id_a) but is absent from c_c (id_c) — that's A —
        // has ranks = [None, Some(1), Some(1)], wins=2, appearances=2.
        let row_a = matrix
            .rows
            .iter()
            .find(|r| r.solution_name == "A")
            .expect("solution A row");
        assert_eq!(
            row_a.ranks,
            vec![None, Some(1), Some(1)],
            "A: absent from newest, then won mid + oldest"
        );
        assert_eq!(row_a.wins, 2, "A won 2 claims");
        assert_eq!(row_a.appearances, 2, "A appeared in 2 claims");

        // --- ASSERT (d): default sort is the Olympic STANDING — medal
        // points (gold 5 / silver 3 / bronze 1) over the displayed
        // columns, not raw win count. So even though A has more golds
        // (2) than B (1), B leads on points because it medalled in all
        // three benchmarks:
        //   B [1,2,2] = 5+3+3 = 11   (1st)
        //   A [None,1,1] = 0+5+5 = 10 (2nd — a missed benchmark costs it)
        //   C [2,None,3] = 3+0+1 = 4
        //   D [3,3,None] = 1+1+0 = 2
        assert_eq!(
            matrix
                .rows
                .iter()
                .map(|r| r.solution_name.as_str())
                .collect::<Vec<_>>(),
            vec!["B", "A", "C", "D"],
            "default sort: highest medal points first"
        );
        assert_eq!(row_a.points, 10);

        // --- Spot-check the other rows (found by name — order is by points).
        let row_b = matrix.rows.iter().find(|r| r.solution_name == "B").unwrap();
        assert_eq!(row_b.ranks, vec![Some(1), Some(2), Some(2)]);
        assert_eq!(row_b.wins, 1);
        assert_eq!(row_b.points, 11);
        assert_eq!(row_b.appearances, 3);
        let row_c = matrix.rows.iter().find(|r| r.solution_name == "C").unwrap();
        assert_eq!(row_c.ranks, vec![Some(2), None, Some(3)]);
        assert_eq!(row_c.wins, 0);
        assert_eq!(row_c.points, 4);
        assert_eq!(row_c.appearances, 2);
        let row_d = matrix.rows.iter().find(|r| r.solution_name == "D").unwrap();
        assert_eq!(row_d.ranks, vec![Some(3), Some(3), None]);
        assert_eq!(row_d.wins, 0);
        assert_eq!(row_d.points, 2);
        assert_eq!(row_d.appearances, 2);

        // --- ASSERT (e, since 0.14.2): cell_labels carry the
        // formatted score, not the rank. `cell_labels` is parallel
        // to `ranks` — empty string where rank is None, otherwise
        // the score that solution achieved in that claim.
        //
        // Layout (newest first → oldest):
        //   col 0 = c_c (id_c, 2026-03-01)  scores: B=0.95 C=0.8 D=0.6
        //   col 1 = c_b (id_b, 2026-02-01)  scores: A=0.9  B=0.8 D=0.6
        //   col 2 = c_a (id_a, 2026-01-01)  scores: A=0.9  B=0.8 C=0.7
        //
        // The rank-1 cell still earns the `.lb-cell-win` gold pill
        // — that's purely a template/CSS concern; here we just
        // confirm the rank is 1 in each cell where we expect the
        // pill to land (row A col1, row A col2, row B col0).
        assert_eq!(row_a.cell_labels.len(), 3);
        assert_eq!(row_a.cell_labels[0], "", "A absent from newest claim");
        assert_eq!(row_a.cell_labels[1], "0.9");
        assert_eq!(row_a.cell_labels[2], "0.9");
        assert_eq!(row_a.ranks[1], Some(1), "rank=1 → .lb-cell-win pill");
        assert_eq!(row_a.ranks[2], Some(1), "rank=1 → .lb-cell-win pill");

        assert_eq!(row_b.cell_labels, vec!["0.95", "0.8", "0.8"]);
        assert_eq!(row_b.ranks[0], Some(1), "B wins newest → gold pill");

        assert_eq!(row_c.cell_labels[0], "0.8");
        assert_eq!(row_c.cell_labels[1], "");
        assert_eq!(row_c.cell_labels[2], "0.7");

        assert_eq!(row_d.cell_labels[0], "0.6");
        assert_eq!(row_d.cell_labels[1], "0.6");
        assert_eq!(row_d.cell_labels[2], "");

        // --- ClaimHeader.link prefers slug; absent slug falls back to id.
        for header in &matrix.claims {
            assert!(
                header.link.starts_with("/claims/"),
                "claim link must be /claims/<id|slug>"
            );
        }
        // Truncated title is ≤18 chars (was 30 pre-0.14.2);
        // fake_view returns an empty title for these fixtures so
        // the truncation just yields "".
        for header in &matrix.claims {
            assert!(header.title_truncated.chars().count() <= 18);
        }
    }

    /// Column cap (since 0.26.1; `MAX_WINS_MATRIX_COLS` = 5 since
    /// 0.27.x). With more ranked claims than the cap, only the top 5 by
    /// `(total$ effort + ε) × corroboration` render: `claim_count` is
    /// the shown count (5) while `total_ranked` keeps the full count
    /// (10), driving the "Top N of M claims" header. Each claim here is
    /// a distinct benchmark (so dedup doesn't collapse them) with zero
    /// effort, so the deterministic tiebreak is `created_at` desc.
    #[test]
    fn build_wins_matrix_caps_columns_at_five() {
        use chrono::{TimeZone, Utc};
        let mut claims_wa = Vec::new();
        for i in 0..10u32 {
            let id = Uuid::from_u128(0xA000_0000_0000_0000_0000_0000_0000_0000u128 + i as u128);
            let sol = format!("S{i}");
            let structure = serde_json::json!({
                "schema_version": 1,
                "benchmark": {"name": format!("bench{i}"),"task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
                "winners": [sol.clone()],
                "leaderboard": [{"rank": 1, "solution": sol, "score": 0.9}]
            });
            let created = Utc.with_ymd_and_hms(2026, 1, 1 + i, 0, 0, 0).unwrap();
            claims_wa.push(fake_claim(id, created, structure));
        }
        let views: Vec<ClaimCardView> = claims_wa.iter().map(fake_view).collect();
        let matrix = build_wins_matrix(&views, &claims_wa, &[], &PresentationConfig::default());

        assert_eq!(matrix.total_ranked, 10, "all 10 ranked claims counted");
        assert_eq!(matrix.claim_count, 5, "columns capped at 5");
        assert_eq!(matrix.claims.len(), 5);
        for row in &matrix.rows {
            assert_eq!(row.ranks.len(), 5, "ranks width matches shown columns");
        }
    }

    /// Vendor grouping (since 0.14.7). Multiple `name_normalized`
    /// variants that share a `logo_url` collapse into a single
    /// matrix row — e.g. three Brightdata SKUs all routing to
    /// google.com on the "Web Access API" battle. Grouping key is
    /// `logo_url`; rows whose `logo_url` is `None` never merge.
    ///
    /// Fixture: 3 claims × 5 solutions; two of the solutions
    /// share a logo. The merged vendor row aggregates:
    ///   - `wins` = sum of `Some(1)` cells across variants
    ///   - `appearances` = distinct claims where ANY variant
    ///     appeared (multiple variants in same claim NOT
    ///     double-counted)
    ///   - per-claim rank = best (min) across variants
    ///   - `display_name` = variant with most appearances; ties
    ///     break alphabetically
    ///   - `variant_names` lists every merged SKU
    #[test]
    fn build_wins_matrix_groups_rows_by_vendor() {
        use crate::models::battle::BattleSolution;
        use chrono::{TimeZone, Utc};
        let id_a = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let id_b = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
        let id_c = Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();

        // Claim A (oldest): X-One #1, X-Two #2, Y #3
        let s_a = serde_json::json!({
            "schema_version": 1,
            "benchmark": {"name":"valpha","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
            "winners": ["X-One"],
            "leaderboard": [
                {"rank": 1, "solution": "X-One", "score": 0.95},
                {"rank": 2, "solution": "X-Two", "score": 0.85},
                {"rank": 3, "solution": "Y",     "score": 0.75}
            ]
        });
        // Claim B (mid): Y #1, Z #2, W #3 — no vendor-X variant.
        let s_b = serde_json::json!({
            "schema_version": 1,
            "benchmark": {"name":"vbeta","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
            "winners": ["Y"],
            "leaderboard": [
                {"rank": 1, "solution": "Y", "score": 0.92},
                {"rank": 2, "solution": "Z", "score": 0.82},
                {"rank": 3, "solution": "W", "score": 0.72}
            ]
        });
        // Claim C (newest): X-Two #1, W #2, Z #3
        let s_c = serde_json::json!({
            "schema_version": 1,
            "benchmark": {"name":"vgamma","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
            "winners": ["X-Two"],
            "leaderboard": [
                {"rank": 1, "solution": "X-Two", "score": 0.99},
                {"rank": 2, "solution": "W",     "score": 0.79},
                {"rank": 3, "solution": "Z",     "score": 0.69}
            ]
        });
        let c_a = fake_claim(
            id_a,
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            s_a,
        );
        let c_b = fake_claim(
            id_b,
            Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap(),
            s_b,
        );
        let c_c = fake_claim(
            id_c,
            Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(),
            s_c,
        );
        let views: Vec<ClaimCardView> = [&c_a, &c_b, &c_c].iter().map(|c| fake_view(*c)).collect();
        let claims_with_author = vec![c_a, c_b, c_c];

        // Curated battle solutions list — 5 entries, the two
        // vendor-X variants share a `logo_url`. Y/Z/W each have
        // their own distinct logo so they don't accidentally
        // collapse together.
        let battle_solutions = vec![
            BattleSolution {
                name: "X-One".into(),
                name_normalized: "x-one".into(),
                logo_url: Some(
                    "https://storage.googleapis.com/krabarena-artifacts/logos/vendor-x.png".into(),
                ),
                website_url: None,
                brand_color: None,
            },
            BattleSolution {
                name: "X-Two".into(),
                name_normalized: "x-two".into(),
                logo_url: Some(
                    "https://storage.googleapis.com/krabarena-artifacts/logos/vendor-x.png".into(),
                ),
                website_url: None,
                brand_color: Some("#abcdef".into()),
            },
            BattleSolution {
                name: "Y".into(),
                name_normalized: "y".into(),
                logo_url: Some(
                    "https://storage.googleapis.com/krabarena-artifacts/logos/y.png".into(),
                ),
                website_url: None,
                brand_color: None,
            },
            BattleSolution {
                name: "Z".into(),
                name_normalized: "z".into(),
                logo_url: Some(
                    "https://storage.googleapis.com/krabarena-artifacts/logos/z.png".into(),
                ),
                website_url: None,
                brand_color: None,
            },
            BattleSolution {
                name: "W".into(),
                name_normalized: "w".into(),
                logo_url: Some(
                    "https://storage.googleapis.com/krabarena-artifacts/logos/w.png".into(),
                ),
                website_url: None,
                brand_color: None,
            },
        ];

        let matrix = build_wins_matrix(
            &views,
            &claims_with_author,
            &battle_solutions,
            &PresentationConfig::default(),
        );

        // --- ASSERT (a): 4 rows — 5 input variants minus 1 merge.
        assert_eq!(
            matrix.rows.len(),
            4,
            "expected 4 rows after collapsing X-One + X-Two into one vendor group"
        );

        // --- ASSERT (b): the merged vendor-X row.
        // Newest-first claim ordering = [c_c, c_b, c_a].
        //   col 0 (c_c, newest):   X-Two #1  → best rank = 1
        //   col 1 (c_b):           no vendor-X variant → None
        //   col 2 (c_a, oldest):   X-One #1, X-Two #2 → best rank = 1
        // wins = 2 (the two #1s), appearances = 2 (claims c_c + c_a;
        // X-Two #2 in c_a is the same claim as X-One #1 so it
        // doesn't double-count).
        let vendor_x = matrix
            .rows
            .iter()
            .find(|r| {
                r.logo_url.as_deref()
                    == Some("https://storage.googleapis.com/krabarena-artifacts/logos/vendor-x.png")
            })
            .expect("merged vendor-X row");
        assert_eq!(
            vendor_x.ranks,
            vec![Some(1), None, Some(1)],
            "merged ranks = best (min) across variants per claim"
        );
        assert_eq!(vendor_x.wins, 2, "two #1 cells across the merged ranks");
        // brand_color flows from the matched battle solution (X-Two) onto
        // the row — used to color this player's Claims Wins pie slice.
        assert_eq!(
            vendor_x.brand_color.as_deref(),
            Some("#abcdef"),
            "row picks up brand_color from battle_solutions"
        );
        assert_eq!(
            vendor_x.appearances, 2,
            "distinct claim count — multi-variant appearances in one claim don't double-count"
        );

        // --- ASSERT (c): display_name = variant with most
        // appearances (X-Two appears in 2 claims, X-One in 1).
        assert_eq!(
            vendor_x.solution_name, "X-Two",
            "display_name = variant with most appearances"
        );

        // --- ASSERT (d): variant_names lists both merged SKUs
        // alphabetically (so the tooltip render order is stable).
        assert_eq!(
            vendor_x.variant_names,
            vec!["X-One".to_string(), "X-Two".to_string()],
        );

        // --- ASSERT (e): unmerged rows still hold a one-element
        // variant_names list — template uses len > 1 to decide
        // whether to render the tooltip.
        let row_y = matrix
            .rows
            .iter()
            .find(|r| r.solution_name == "Y")
            .expect("Y row");
        assert_eq!(row_y.variant_names, vec!["Y".to_string()]);
        assert_eq!(row_y.wins, 1, "Y wins claim B");
        // Y appears in claim A (#3) + claim B (#1) → 2 appearances.
        assert_eq!(row_y.appearances, 2);

        // --- ASSERT (f): the rank-1 cell in the merged row owns
        // the score of the winning variant in that claim.
        // Column 0 (c_c) → X-Two's 0.99; column 2 (c_a) → X-One's 0.95.
        assert_eq!(vendor_x.cell_labels[0], "0.99");
        assert_eq!(vendor_x.cell_labels[1], "", "no vendor-X in c_b");
        assert_eq!(vendor_x.cell_labels[2], "0.95");

        // --- ASSERT (g): default sort puts the merged vendor row
        // first — it has the most wins (2).
        assert_eq!(
            matrix.rows[0].solution_name, "X-Two",
            "merged vendor row leads with wins=2"
        );
    }

    /// Regression guard for the `logo_url is None → no grouping`
    /// rule. Two solutions with no curated logo must each stay on
    /// their own row even when their per-row inline logo is also
    /// `None` — otherwise unrelated unknown vendors would collapse
    /// into one row.
    #[test]
    fn build_wins_matrix_does_not_group_when_logo_url_is_none() {
        use chrono::{TimeZone, Utc};
        let id_a = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let s_a = serde_json::json!({
            "schema_version": 1,
            "benchmark": {"name":"bm","task":"t","dataset":"d","metric":"m","higher_is_better":true,"setup":"s"},
            "winners": ["UnknownOne"],
            "leaderboard": [
                {"rank": 1, "solution": "UnknownOne", "score": 0.9},
                {"rank": 2, "solution": "UnknownTwo", "score": 0.8}
            ]
        });
        let c_a = fake_claim(
            id_a,
            Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            s_a,
        );
        let views: Vec<ClaimCardView> = vec![fake_view(&c_a)];
        let claims_with_author = vec![c_a];
        // No `battle_solutions` and no per-row logo_url in the
        // JSON → every row's logo_url stays None.
        let matrix = build_wins_matrix(
            &views,
            &claims_with_author,
            &[],
            &PresentationConfig::default(),
        );
        assert_eq!(
            matrix.rows.len(),
            2,
            "logo_url None must NOT collapse unrelated unknown vendors"
        );
        for row in &matrix.rows {
            assert_eq!(row.logo_url, None);
            assert_eq!(row.variant_names.len(), 1);
        }
    }

    /// `build_champion` source-precedence (since 0.14.2):
    ///   1. `executive_summary` if it names a matrix solution.
    ///   2. Wins-count leader fallback.
    ///   3. `None` on empty matrices.
    #[test]
    fn champion_picks_executive_summary_then_wins_fallback() {
        // ---- Fixture: a 3-row matrix sorted wins-desc with 4 claim columns. ----
        // Claim headers carry placeholder UUIDs — only `claims.len()`
        // is read by `WinsMatrix::is_empty()` and `build_champion`.
        let dummy_id = Uuid::nil();
        let dummy_header = ClaimHeader {
            id: dummy_id,
            slug: None,
            title_truncated: String::new(),
            title_full: String::new(),
            link: format!("/claims/{}", dummy_id),
        };
        let mut matrix = WinsMatrix {
            claim_count: 4,
            total_ranked: 4,
            claims: vec![dummy_header; 4],
            rows: vec![
                WinsMatrixRow {
                    solution_name: "Tavily".into(),
                    logo_url: None,
                    logo_initials: "TA".into(),
                    logo_color: "#000".into(),
                    logo_text_color: "#fff".into(),
                    brand_color: None,
                    wins: 3,
                    points: 18, // 5+5+5+3
                    appearances: 4,
                    ranks: vec![Some(1), Some(1), Some(1), Some(2)],
                    cell_labels: vec!["0.9".into(); 4],
                    variant_names: vec!["Tavily".into()],
                },
                WinsMatrixRow {
                    solution_name: "Exa".into(),
                    logo_url: None,
                    logo_initials: "EX".into(),
                    logo_color: "#000".into(),
                    logo_text_color: "#fff".into(),
                    brand_color: None,
                    wins: 1,
                    points: 11, // 3+3+0+5
                    appearances: 3,
                    ranks: vec![Some(2), Some(2), None, Some(1)],
                    cell_labels: vec!["0.8".into(); 4],
                    variant_names: vec!["Exa".into()],
                },
                WinsMatrixRow {
                    solution_name: "Brave Search".into(),
                    logo_url: None,
                    logo_initials: "BR".into(),
                    logo_color: "#000".into(),
                    logo_text_color: "#fff".into(),
                    brand_color: None,
                    wins: 0,
                    points: 2, // 1+0+0+1
                    appearances: 2,
                    ranks: vec![Some(3), None, None, Some(3)],
                    cell_labels: vec!["0.7".into(); 4],
                    variant_names: vec!["Brave Search".into()],
                },
            ],
        };

        // (1) Executive summary names "Exa" — picked over the
        // wins leader Tavily. Note = first sentence of summary.
        let summary = "Exa pulls ahead on long-form questions with a 0.93 win rate. \
                       Tavily leads on freshness.";
        let ch = build_champion(&matrix, Some(summary)).expect("Some");
        assert_eq!(ch.solution_name, "Exa");
        assert!(ch.from_executive_summary);
        assert!(
            ch.note.starts_with("Exa pulls ahead"),
            "note must be first sentence; got {:?}",
            ch.note
        );
        assert!(ch.note.ends_with('.'));

        // (2) Executive summary doesn't name any matrix solution
        // → wins-count fallback kicks in (Tavily, 3 wins / 4 claims).
        let summary = "The web access space remains contested with no clear winner.";
        let ch = build_champion(&matrix, Some(summary)).expect("Some");
        assert_eq!(ch.solution_name, "Tavily");
        assert!(!ch.from_executive_summary);
        assert_eq!(
            ch.note,
            "Leads the standings with 18 points across the top 4 benchmarks."
        );

        // (3) No executive summary at all → fallback path.
        let ch = build_champion(&matrix, None).expect("Some");
        assert_eq!(ch.solution_name, "Tavily");
        assert!(!ch.from_executive_summary);

        // (4) Empty matrix → no banner.
        let empty = WinsMatrix::default();
        assert!(build_champion(&empty, Some(summary)).is_none());
        assert!(build_champion(&empty, None).is_none());

        // (5) Summary case-insensitive substring match.
        let summary = "tavily edges out everyone on the freshness axis.";
        let ch = build_champion(&matrix, Some(summary)).expect("Some");
        assert_eq!(ch.solution_name, "Tavily");
        assert!(ch.from_executive_summary);

        // (6) Summary names multiple solutions → leftmost-mention
        // wins (Exa appears before Tavily here).
        let summary = "Exa edges out Tavily on long-form, with Tavily second.";
        let ch = build_champion(&matrix, Some(summary)).expect("Some");
        assert_eq!(ch.solution_name, "Exa");
        assert!(ch.from_executive_summary);

        // (7) points == 0 fallback note (no top-3 in the shown columns).
        matrix.rows[0].points = 0;
        matrix.rows[0].wins = 0;
        let ch = build_champion(&matrix, None).expect("Some");
        assert!(
            ch.note.starts_with("Top of the standings"),
            "points=0 fallback note; got {:?}",
            ch.note
        );
    }

    /// `mini_wins_matrix` trims a full `WinsMatrix` to the top-3
    /// solutions × top-3 benchmark columns rendered on the home card
    /// (0.28.7). Rows stay points-ordered, columns stay in the source
    /// order, `MiniCell.label` mirrors the source `cell_labels`, and
    /// `is_win` is set iff the source rank was `Some(1)`.
    #[test]
    fn mini_wins_matrix_trims_to_three_by_three() {
        let dummy_id = Uuid::nil();
        let header = |t: &str| ClaimHeader {
            id: dummy_id,
            slug: None,
            title_truncated: t.into(),
            title_full: format!("{t} (full)"),
            link: format!("/claims/{}", dummy_id),
        };
        // 4 claim columns and 4 rows — both exceed the 3-cap.
        let matrix = WinsMatrix {
            claim_count: 4,
            total_ranked: 4,
            claims: vec![
                header("Acc"),
                header("Lat"),
                header("Cost"),
                header("Recall"),
            ],
            rows: vec![
                WinsMatrixRow {
                    solution_name: "Alpha".into(),
                    logo_url: Some("https://x/alpha.png".into()),
                    logo_initials: "AL".into(),
                    logo_color: "#000".into(),
                    logo_text_color: "#fff".into(),
                    brand_color: None,
                    wins: 2,
                    points: 16,
                    appearances: 4,
                    ranks: vec![Some(1), Some(2), Some(1), Some(3)],
                    cell_labels: vec!["0.92".into(), "120".into(), "0.10".into(), "0.80".into()],
                    variant_names: vec!["Alpha".into()],
                },
                WinsMatrixRow {
                    solution_name: "Beta".into(),
                    logo_url: None,
                    logo_initials: "BE".into(),
                    logo_color: "#000".into(),
                    logo_text_color: "#fff".into(),
                    brand_color: None,
                    wins: 1,
                    points: 10,
                    appearances: 3,
                    // No appearance in column 2 (Cost) → empty cell label.
                    ranks: vec![Some(2), Some(1), None, Some(2)],
                    cell_labels: vec!["0.88".into(), "90".into(), String::new(), "0.78".into()],
                    variant_names: vec!["Beta".into()],
                },
                WinsMatrixRow {
                    solution_name: "Gamma".into(),
                    logo_url: None,
                    logo_initials: "GA".into(),
                    logo_color: "#000".into(),
                    logo_text_color: "#fff".into(),
                    brand_color: None,
                    wins: 0,
                    points: 4,
                    appearances: 2,
                    ranks: vec![Some(3), None, Some(2), None],
                    cell_labels: vec!["0.80".into(), String::new(), "0.20".into(), String::new()],
                    variant_names: vec!["Gamma".into()],
                },
                WinsMatrixRow {
                    solution_name: "Delta".into(),
                    logo_url: None,
                    logo_initials: "DE".into(),
                    logo_color: "#000".into(),
                    logo_text_color: "#fff".into(),
                    brand_color: None,
                    wins: 0,
                    points: 1,
                    appearances: 1,
                    ranks: vec![None, Some(3), None, None],
                    cell_labels: vec![String::new(), "150".into(), String::new(), String::new()],
                    variant_names: vec!["Delta".into()],
                },
            ],
        };

        let mini = mini_wins_matrix(&matrix, 3, 3).expect("non-empty matrix → Some");

        // Trimmed to 3 columns × 3 rows.
        assert_eq!(mini.columns.len(), 3);
        assert_eq!(mini.rows.len(), 3);

        // Columns keep source order and carry truncated + full titles.
        assert_eq!(
            mini.columns
                .iter()
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>(),
            ["Acc", "Lat", "Cost"]
        );
        assert_eq!(mini.columns[0].full, "Acc (full)");

        // Rows stay points-ordered (16 > 10 > 4); Delta is dropped.
        assert_eq!(
            mini.rows
                .iter()
                .map(|r| (r.solution_name.as_str(), r.points))
                .collect::<Vec<_>>(),
            [("Alpha", 16), ("Beta", 10), ("Gamma", 4)]
        );
        assert_eq!(
            mini.rows[0].logo_url.as_deref(),
            Some("https://x/alpha.png")
        );

        // Each row has exactly 3 cells (one per trimmed column).
        for row in &mini.rows {
            assert_eq!(row.cells.len(), 3);
        }

        // `is_win` reflects rank 1: Alpha wins col 0 and col 2, not col 1.
        assert!(mini.rows[0].cells[0].is_win);
        assert!(!mini.rows[0].cells[1].is_win);
        assert!(mini.rows[0].cells[2].is_win);
        // Beta wins col 1 only.
        assert!(!mini.rows[1].cells[0].is_win);
        assert!(mini.rows[1].cells[1].is_win);

        // `label` mirrors the source `cell_labels` (the score, not rank);
        // missing appearance → empty label.
        assert_eq!(mini.rows[0].cells[0].label, "0.92");
        assert_eq!(mini.rows[1].cells[1].label, "90");
        assert_eq!(mini.rows[1].cells[2].label, ""); // Beta absent in Cost
        assert!(!mini.rows[1].cells[2].is_win);

        // Empty matrix → None (home card falls back to the parsed list).
        assert!(mini_wins_matrix(&WinsMatrix::default(), 3, 3).is_none());
    }

    /// `cell_score_label` is the wins-matrix formatter (since
    /// 0.14.2). Keeps cells ≤4 visible chars so 8+ claim columns
    /// fit on a desktop without horizontal scroll.
    #[test]
    fn cell_score_label_formats_compactly() {
        assert_eq!(cell_score_label(0.9, 1), "0.9");
        assert_eq!(cell_score_label(0.95, 1), "0.95");
        assert_eq!(cell_score_label(92.0, 1), "92");
        assert_eq!(cell_score_label(78.5, 2), "78.5");
        assert_eq!(cell_score_label(134.0, 3), "134");
        assert_eq!(cell_score_label(1234.0, 1), "1.2K");
        assert_eq!(cell_score_label(12345.0, 1), "12.3K");
        assert_eq!(cell_score_label(123456.0, 1), "123K");
        assert_eq!(cell_score_label(1000.0, 1), "1K");
        // NaN / inf → fallback to rank notation.
        assert_eq!(cell_score_label(f64::NAN, 4), "#4");
        assert_eq!(cell_score_label(f64::INFINITY, 7), "#7");
    }

    /// `truncate_with_full` short-circuits when the input already fits.
    /// When it trims, the returned tuple's `Some(full)` carries the
    /// original verbatim for the `title=` attribute.
    #[test]
    fn truncate_with_full_keeps_short_strings_intact() {
        assert_eq!(truncate_with_full("short", 60), ("short".to_string(), None));
        let long = "a".repeat(120);
        let (out, full) = truncate_with_full(&long, 60);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 60);
        assert_eq!(full.as_deref(), Some(long.as_str()));
    }

    #[test]
    fn benchmark_family_key_collapses_variants_but_keeps_distinct_stems() {
        // Same benchmark, different size/qualifier → same family, so
        // the wins matrix shows it at most once.
        for n in [
            "SimpleQA-200",
            "SimpleQA-50",
            "SimpleQA-20 (HN)",
            "SimpleQA latency",
        ] {
            assert_eq!(benchmark_family_key(n), "simpleqa", "got family for {n:?}");
        }
        // Distinct benchmarks keep distinct stems.
        assert_eq!(benchmark_family_key("FRAMES-200"), "frames");
        assert_eq!(benchmark_family_key("FreshQA retrieval"), "freshqa");
        assert_eq!(benchmark_family_key("Fresh-news QA-50"), "fresh");
        assert_eq!(benchmark_family_key("GAIA retrieval"), "gaia");
        assert_eq!(benchmark_family_key("DeepResearch retr."), "deepresearch");
        // `.` is part of the stem so "You.com" doesn't collapse to "you".
        assert_eq!(benchmark_family_key("You.com 50-q recall"), "you.com");
        // No leading alphanumeric run → whole lower-cased name (never empty).
        assert_eq!(benchmark_family_key("  🏆 Trophy bench"), "🏆 trophy bench");
        assert!(!benchmark_family_key("anything").is_empty());
    }

    #[test]
    fn medal_points_scores_gold_silver_bronze_over_shown_columns() {
        let c = PresentationConfig::default(); // 5 / 3 / 1
        let (g, s, b) = (c.gold_points, c.silver_points, c.bronze_points);
        // One of each medal + a 4th-place (0) + an absence (0).
        assert_eq!(
            medal_points(&[Some(1), Some(2), Some(3), Some(4), None], g, s, b),
            5 + 3 + 1
        );
        // Multiple golds stack.
        assert_eq!(medal_points(&[Some(1), Some(1)], g, s, b), 10);
        // No medals → 0.
        assert_eq!(medal_points(&[Some(4), Some(9), None], g, s, b), 0);
        assert_eq!(medal_points(&[], g, s, b), 0);
        // Custom weights are honoured.
        assert_eq!(medal_points(&[Some(1), Some(3)], 10, 4, 2), 12);
    }

    #[test]
    fn corroboration_factor_gates_on_refute_ratio() {
        let cfg = PresentationConfig::default();
        let (ratio, demote) = (cfg.controversy_refute_ratio, cfg.controversy_demotion);
        let factor = |v, r| corroboration_factor(v, r, ratio, demote);
        // Unverified (0/0) and any-verifies/0-refutes are clean: factor
        // grows with verifies, so effort alone still ranks an unverified
        // claim (factor 1.0).
        assert_eq!(factor(0, 0), 1.0);
        assert_eq!(factor(5, 0), 6.0);
        // 1 refute against 50 verifies (2%) is well under 20% → clean.
        assert_eq!(factor(50, 1), 51.0);
        // Exactly 20% (1 refute / 5 verifies) trips the gate → demoted.
        assert_eq!(factor(5, 1), demote);
        // 3 refutes / 5 verifies (60%) → demoted.
        assert_eq!(factor(5, 3), demote);
        // Purely refuted (0 verifies, ≥1 refute) → demoted.
        assert_eq!(factor(0, 1), demote);
        // Always strictly positive — never a divide-by-zero / NaN / sign flip.
        for (v, r) in [(0, 0), (0, 9), (9, 9), (1, 0), (100, 19)] {
            assert!(factor(v, r) > 0.0, "non-positive for {v}/{r}");
        }
        // A custom config tightens/loosens the gate: at ratio 0.50 a 1/5
        // claim (20%) is now clean, and a hotter demotion floor applies.
        assert_eq!(
            corroboration_factor(5, 1, 0.50, 0.25),
            6.0,
            "20% < 50% → clean"
        );
        assert_eq!(
            corroboration_factor(2, 1, 0.50, 0.25),
            0.25,
            "50% ≥ 50% → demoted"
        );
    }

    // --- Claim verdict strip + hero modules (select_presentation ladder) ---

    mod presentation_tests {
        use super::*;
        use krabarena_shared::claim_structure::{
            Benchmark, ClaimStructure, Hypothesis, LeaderboardEntry,
        };
        use std::collections::BTreeMap;

        fn base_row() -> crate::models::claim::ClaimRow {
            crate::models::claim::ClaimRow {
                id: Uuid::nil(),
                battle_id: None,
                author_id: Uuid::nil(),
                claim: String::new(),
                artifact_hash: String::new(),
                artifact_url: String::new(),
                artifact_size: 0,
                predecessor_id: None,
                verify_count: 0,
                refute_count: 0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                agent_backend: None,
                agent_model: None,
                upvote_count: 0,
                downvote_count: 0,
                comment_count: 0,
                review_id: None,
                warnings_ignored: false,
                artifact_summary: None,
                artifact_highlights: None,
                tokens_consumed: None,
                wall_clock_ms: None,
                env_hash: None,
                visibility: Some("public".into()),
                price_cents: None,
                slug: None,
                artifact_filename: None,
                private_artifact_url: None,
                private_artifact_filename: None,
                og_summary: None,
                claim_structure: None,
                winners: None,
                topic: None,
                topic_tags: None,
                deleted_at: None,
                produced_by: None,
                presentation: None,
            }
        }

        fn bench() -> Benchmark {
            Benchmark {
                name: "Test Bench".into(),
                task: "task".into(),
                dataset: "dataset".into(),
                dataset_url: None,
                metric: "score".into(),
                metric_unit: None,
                higher_is_better: true,
                setup: "setup".into(),
                reproduction_command: None,
                short_label: None,
            }
        }

        fn entry(rank: u32, solution: &str, score: f64) -> LeaderboardEntry {
            LeaderboardEntry {
                rank,
                solution: solution.to_string(),
                score,
                notes: None,
                evidence_url: None,
                logo_url: None,
                website_url: None,
                metrics: None,
            }
        }

        fn entry_with_metrics(
            rank: u32,
            solution: &str,
            score: f64,
            metrics: &[(&str, f64)],
        ) -> LeaderboardEntry {
            let mut e = entry(rank, solution, score);
            e.metrics = Some(
                metrics
                    .iter()
                    .map(|(k, v)| (k.to_string(), *v))
                    .collect::<BTreeMap<_, _>>(),
            );
            e
        }

        // --- stamp_for ---

        #[test]
        fn stamp_verified_only() {
            assert_eq!(
                stamp_for(2, 0),
                ("v".to_string(), "✓ VERIFIED ×2".to_string())
            );
        }

        #[test]
        fn stamp_refuted_only() {
            assert_eq!(
                stamp_for(0, 1),
                ("r".to_string(), "✗ REFUTED ×1".to_string())
            );
        }

        #[test]
        fn stamp_contested() {
            assert_eq!(
                stamp_for(1, 1),
                ("c".to_string(), "⚔ CONTESTED · 1✓ 1✗".to_string())
            );
        }

        #[test]
        fn stamp_unreviewed() {
            assert_eq!(stamp_for(0, 0), ("c".to_string(), "UNREVIEWED".to_string()));
        }

        // --- hero_value_label (shared format_metric_compact) ---

        #[test]
        fn hero_value_label_keeps_short_units() {
            assert_eq!(hero_value_label(1.5, "%", ""), "1.5%");
            assert_eq!(hero_value_label(1.5, "ms", ""), "1.5 ms");
            assert_eq!(hero_value_label(1.5, "", ""), "1.5");
        }

        #[test]
        fn hero_value_label_scales_and_labels_time() {
            // The presentation bug this module fixes: a raw seconds
            // value rendered bare ("0.102") instead of scaled+labeled.
            assert_eq!(hero_value_label(0.102, "s", ""), "102 ms");
        }

        #[test]
        fn hero_value_label_drops_sentence_long_unit() {
            // A verbose `metric_unit` (a description, not a unit) must
            // not be appended — this is the bug that starved
            // `.race-track` to 0px width on a real claim page.
            let unit = "fraction of top-10 titles whose text explicitly \
                contains the language name (case-insensitive, …)";
            assert!(unit.chars().count() > 12);
            assert_eq!(hero_value_label(3.0, unit, ""), "3");
        }

        #[test]
        fn build_race_bars_value_label_ignores_verbose_unit() {
            let leaderboard = vec![entry(1, "A", 42.0), entry(2, "B", 10.0)];
            let bars = build_race_bars(
                "fraction of top-10 titles whose text explicitly contains the language name",
                "",
                &["A".to_string()],
                &leaderboard,
                false,
            );
            assert_eq!(bars[0].value_label, "42");
            assert_eq!(bars[1].value_label, "10");
        }

        #[test]
        fn build_race_bars_value_label_keeps_short_unit() {
            let leaderboard = vec![entry(1, "A", 42.0)];
            let bars = build_race_bars("%", "", &["A".to_string()], &leaderboard, false);
            assert_eq!(bars[0].value_label, "42%");
        }

        // --- select_presentation ladder ---

        #[test]
        fn stored_presentation_wins_over_synthetic_ladder() {
            // Structure alone would synthesize an h2h (definitive
            // hypothesis) — the stored `race` presentation must win.
            let stored = serde_json::json!({
                "layout": "race",
                "tweet_title": "Stored tweet",
                "tweet_annotation": "annotation",
                "hero_stat": {"value": "42", "label": "wins"},
            });
            let row = crate::models::claim::ClaimRow {
                verify_count: 3,
                refute_count: 0,
                presentation: Some(stored),
                ..base_row()
            };
            let structure = ClaimStructure {
                schema_version: 2,
                benchmark: bench(),
                leaderboard: vec![
                    entry(1, "A", 10.0),
                    entry(2, "B", 5.0),
                    entry(3, "C", 3.0),
                    entry(4, "D", 1.0),
                ],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: Some(Hypothesis {
                    statement: "A is 2x B".into(),
                    claimed_value: "2x".into(),
                    measured_value: "2x".into(),
                    verdict: "confirms".into(),
                }),
            };
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            assert_eq!(pres.layout, "race");
            assert_eq!(pres.tweet_title, "Stored tweet");
            assert_eq!(pres.hero_value, "42");
            assert_eq!(pres.race.len(), 4, "race bars built fresh from structure");
            assert_eq!(pres.stamp_class, "v");
        }

        #[test]
        fn empty_tweet_title_falls_through_to_synthetic_ladder() {
            // A stored presentation with a blank/whitespace tweet_title
            // must NOT render an empty <h2> — it should be treated the
            // same as "no usable stored presentation" and fall through
            // to the synthetic ladder (here: the definitive-hypothesis
            // synthetic h2h).
            let stored = serde_json::json!({
                "layout": "race",
                "tweet_title": "   ",
                "tweet_annotation": "annotation",
                "hero_stat": {"value": "42", "label": "wins"},
            });
            let row = crate::models::claim::ClaimRow {
                verify_count: 0,
                refute_count: 1,
                presentation: Some(stored),
                ..base_row()
            };
            let structure = ClaimStructure {
                schema_version: 2,
                benchmark: bench(),
                leaderboard: vec![entry(1, "A", 10.0), entry(2, "B", 5.0)],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: Some(Hypothesis {
                    statement: "A is 2x B".into(),
                    claimed_value: "6.4x".into(),
                    measured_value: "1.09x".into(),
                    verdict: "refutes".into(),
                }),
            };
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            assert_eq!(
                pres.layout, "h2h",
                "empty tweet_title on the stored presentation must fall through, \
                 not render a blank hero module"
            );
            assert_eq!(pres.tweet_title, "A is 2x B");
        }

        #[test]
        fn synthetic_h2h_from_definitive_hypothesis() {
            let row = crate::models::claim::ClaimRow {
                verify_count: 0,
                refute_count: 1,
                ..base_row()
            };
            let structure = ClaimStructure {
                schema_version: 2,
                benchmark: bench(),
                leaderboard: vec![entry(1, "A", 10.0), entry(2, "B", 5.0)],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: Some(Hypothesis {
                    statement: "A is 6x faster than B".into(),
                    claimed_value: "6.4x".into(),
                    measured_value: "1.09x".into(),
                    verdict: "refutes".into(),
                }),
            };
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            assert_eq!(pres.layout, "h2h");
            assert_eq!(pres.refutation_claimed, "6.4x");
            assert_eq!(pres.refutation_measured, "1.09x");
            let h2h = pres.h2h.expect("h2h view");
            assert_eq!(h2h.left_name, "A");
            assert_eq!(h2h.right_name, "B");
            assert_eq!(h2h.verdict_line, "A is 6x faster than B");
        }

        #[test]
        fn inconclusive_hypothesis_does_not_trigger_h2h() {
            // An "inconclusive" verdict isn't "definitive" — the ladder
            // should fall through to the next rung (race, since there's
            // no shared metrics key and >= 4 rows).
            let row = base_row();
            let structure = ClaimStructure {
                schema_version: 2,
                benchmark: bench(),
                leaderboard: vec![
                    entry(1, "A", 10.0),
                    entry(2, "B", 5.0),
                    entry(3, "C", 3.0),
                    entry(4, "D", 1.0),
                ],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: Some(Hypothesis {
                    statement: "maybe".into(),
                    claimed_value: "2x".into(),
                    measured_value: "1x".into(),
                    verdict: "inconclusive".into(),
                }),
            };
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            assert_eq!(pres.layout, "race");
        }

        #[test]
        fn synthetic_frontier_from_shared_metrics_key() {
            let row = base_row();
            let structure = ClaimStructure {
                schema_version: 2,
                benchmark: bench(),
                leaderboard: vec![
                    entry_with_metrics(1, "A", 3.0, &[("latency_p50_ms", 628.0)]),
                    entry_with_metrics(2, "B", 3.0, &[("latency_p50_ms", 2225.0)]),
                    entry_with_metrics(3, "C", 3.0, &[("latency_p50_ms", 1200.0)]),
                ],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: None,
            };
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            assert_eq!(pres.layout, "frontier");
            let f = pres.frontier.expect("frontier view");
            assert_eq!(f.dots.len(), 3);
            // All same score → all same y_pct; different latency → different x_pct.
            assert_eq!(f.dots[0].y_pct, f.dots[1].y_pct);
            assert_eq!(f.dots[1].y_pct, f.dots[2].y_pct);
            assert_ne!(f.dots[0].x_pct, f.dots[1].x_pct);
            assert_ne!(f.dots[1].x_pct, f.dots[2].x_pct);
        }

        /// Regression guard for claim db6e8ab8 ("Keenable, Exa and
        /// Parallel tie at 99% on OB-News…"): its frontier hero
        /// rendered as a bare grid — no axis titles, no ticks — with
        /// three names overprinting each other in the top-left and the
        /// Google control's label clipped at the edge. The chart must
        /// now carry axis titles + real ticks, a legend row for every
        /// dot (so a hidden in-plot label never hides a point), and no
        /// overlapping in-plot labels.
        fn db6e8ab8_structure() -> ClaimStructure {
            let mut b = bench();
            b.metric = "raw_answer_presence_at_10_pct (non-error responses)".into();
            b.metric_unit = Some("%".into());
            let row = |rank, name: &str, score, lat, overlap| {
                entry_with_metrics(
                    rank,
                    name,
                    score,
                    &[("google_url_overlap_pct", overlap), ("latency_p50_ms", lat)],
                )
            };
            ClaimStructure {
                schema_version: 1,
                benchmark: b,
                leaderboard: vec![
                    row(1, "Keenable", 99.0, 789.0, 10.6),
                    row(2, "Exa (highlights)", 99.0, 1278.0, 18.8),
                    row(3, "Parallel", 99.0, 2608.0, 16.7),
                    row(4, "Firecrawl search", 97.0, 1247.0, 57.1),
                    row(5, "Tavily basic", 95.0, 1760.0, 26.3),
                    entry_with_metrics(
                        6,
                        "Google via Bright Data SERP (control)",
                        84.4,
                        &[("latency_p50_ms", 11308.0)],
                    ),
                ],
                winners: vec![
                    "Keenable".into(),
                    "Exa (highlights)".into(),
                    "Parallel".into(),
                ],
                cost: None,
                hypothesis: None,
            }
        }

        /// The axis/tick/legend machinery guards below pin the latency
        /// axis (the Google control far right is the stress case), so
        /// they drop the overlap key — latency is then the only shared key.
        fn db6e8ab8_latency_axis_structure() -> ClaimStructure {
            let mut s = db6e8ab8_structure();
            for e in &mut s.leaderboard {
                if let Some(m) = e.metrics.as_mut() {
                    m.remove("google_url_overlap_pct");
                }
            }
            s
        }

        #[test]
        fn db6e8ab8_frontier_has_axes_ticks_and_full_legend() {
            use crate::web::frontier_chart as fc;
            let structure = db6e8ab8_latency_axis_structure();
            let row = base_row();
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            let f = pres.frontier.expect("frontier view");
            assert_eq!(f.y_title, "Raw answer presence at 10 (%)");
            assert_eq!(f.x_title, "Latency p50 (s)");
            let y: Vec<&str> = f.y_ticks.iter().map(|t| t.label.as_str()).collect();
            assert_eq!(y, vec!["80", "85", "90", "95", "100"]);
            assert!((3..=5).contains(&f.x_ticks.len()));
            assert_eq!(f.x_ticks[0].label, "0");
            // Every leaderboard row with the key is plotted AND legended
            // (the template renders one legend row per dot).
            assert_eq!(f.dots.len(), 6);
            let idxs: Vec<usize> = f.dots.iter().map(|d| d.idx).collect();
            assert_eq!(idxs, vec![1, 2, 3, 4, 5, 6]);
            assert_eq!(f.dots[0].score_label, "99%");
            assert_eq!(f.dots[0].x_label, "789 ms");
            assert!(
                f.aria_label
                    .contains("Leader: Keenable, Exa (highlights), Parallel")
            );
            // Google control sits near the right edge → label anchors
            // inward (left) or is hidden; never right (would clip).
            let google = &f.dots[5];
            assert!(google.x_pct > 70.0);
            assert_ne!(google.label_side, "r");
            assert_ne!(google.label_side_wide, "r");
            // No two visible labels overlap in either reference geometry.
            for (w, wide) in [(fc::NARROW_W, false), (fc::WIDE_W, true)] {
                let inputs: Vec<fc::PlacementInput> = f
                    .dots
                    .iter()
                    .map(|d| fc::PlacementInput {
                        x_pct: d.x_pct,
                        y_pct: d.y_pct,
                        jitter: d.jitter,
                        label: d.label.as_str(),
                    })
                    .collect();
                let order: Vec<usize> = (0..inputs.len()).collect();
                let sides = fc::place_labels(&inputs, &order, w, fc::AREA_H);
                let chosen: Vec<&str> = f
                    .dots
                    .iter()
                    .map(|d| {
                        if wide {
                            d.label_side_wide
                        } else {
                            d.label_side
                        }
                    })
                    .collect();
                // Winners-first order matches leaderboard order here.
                let expect: Vec<&str> = sides.iter().map(|s| s.as_attr()).collect();
                assert_eq!(chosen, expect);
            }
        }

        #[test]
        fn db6e8ab8_frontier_renders_axes_ticks_and_legend() {
            let structure = db6e8ab8_latency_axis_structure();
            let pres = select_presentation(&base_row(), Some(&structure)).expect("presentation");
            let html = minimal_claim_template(Some(pres))
                .render()
                .expect("renders");
            assert!(html.contains("frontier-ytitle"));
            assert!(html.contains("Raw answer presence at 10 (%)"));
            assert!(html.contains("Latency p50 (s)"));
            assert!(html.contains("ftick-y"));
            assert!(html.contains("ftick-x"));
            assert!(html.contains("frontier-legend"));
            assert_eq!(html.matches("class=\"fl-row").count(), 6);
            assert!(html.contains("Google via Bright Data SERP (control)"));
            assert!(html.contains("aria-label=\"Scatter plot of 6 solutions"));
            assert!(html.contains("data-lbl=\""));
            assert!(html.contains("data-lbl-wide=\""));
        }

        /// Live claim db6e8ab8 as served by `/v1/claims/<id>` (metric
        /// keys, title and stored presentation verbatim; 2026-10-26).
        fn db6e8ab8_live_claim() -> (crate::models::claim::ClaimRow, ClaimStructure) {
            let mut b = bench();
            b.metric = "raw_answer_presence_at_10_pct (non-error responses)".into();
            b.metric_unit = Some("%".into());
            #[allow(clippy::too_many_arguments)]
            let row = |rank, name: &str, score, ok, pres, missed, p1, gmiss, dom, url, lat| {
                entry_with_metrics(
                    rank,
                    name,
                    score,
                    &[
                        ("answer_presence_on_ok_pct", ok),
                        ("answer_presence_pct", pres),
                        ("answers_google_missed", missed),
                        ("bootstrap_p_top1", p1),
                        ("errors", 0.0),
                        ("google_answers_it_missed", gmiss),
                        ("google_domain_overlap_pct", dom),
                        ("google_url_overlap_pct", url),
                        ("latency_p50_ms", lat),
                        ("n", 200.0),
                        ("paired_with_google_ok", 147.0),
                    ],
                )
            };
            let structure = ClaimStructure {
                schema_version: 1,
                benchmark: b,
                leaderboard: vec![
                    row(
                        1, "Keenable", 99.0, 99.0, 99.0, 21.0, 1.0, 0.0, 18.1, 10.6, 789.0,
                    ),
                    row(
                        2,
                        "Exa (highlights)",
                        99.0,
                        99.0,
                        99.0,
                        21.0,
                        1.0,
                        0.0,
                        36.1,
                        18.8,
                        1278.0,
                    ),
                    row(
                        3, "Parallel", 99.0, 99.0, 99.0, 21.0, 1.0, 0.0, 28.3, 16.7, 2608.0,
                    ),
                    row(
                        4,
                        "Firecrawl search",
                        97.0,
                        97.0,
                        97.0,
                        17.0,
                        0.138,
                        0.0,
                        66.8,
                        57.1,
                        1247.0,
                    ),
                    row(
                        5,
                        "Tavily basic",
                        95.0,
                        95.0,
                        95.0,
                        17.0,
                        0.017,
                        1.0,
                        38.7,
                        26.3,
                        1760.0,
                    ),
                    entry_with_metrics(
                        6,
                        "Google via Bright Data SERP (control)",
                        84.4,
                        &[
                            ("answer_presence_on_ok_pct", 84.4),
                            ("answer_presence_pct", 62.0),
                            ("bootstrap_p_top1", 0.0),
                            ("errors", 53.0),
                            ("latency_p50_ms", 11308.0),
                            ("n", 200.0),
                        ],
                    ),
                ],
                winners: vec![
                    "Keenable".into(),
                    "Exa (highlights)".into(),
                    "Parallel".into(),
                ],
                cost: None,
                hypothesis: None,
            };
            let claim_row = crate::models::claim::ClaimRow {
                claim: "# Keenable, Exa and Parallel tie at 99% on OB-News \u{2014} with only \
                        11\u{2013}19% URL overlap with Google (Firecrawl 57%)\n\n## Summary\n\n..."
                    .into(),
                presentation: Some(serde_json::json!({
                    "layout": "frontier",
                    "tweet_title": "Top AI search APIs now hit 99% accuracy on news queries. \
                                    The real differentiator? Most of their results aren't from Goo\u{2026}",
                    "tweet_annotation": "A 100-question benchmark across 5 APIs + a Google control \
                                         found Keenable, Exa, and Parallel tied at 99% answer presence\u{2026}",
                    "hero_stat": {"value": "11-19%", "label": "Google URL overlap for top-3 APIs"},
                })),
                ..base_row()
            };
            (claim_row, structure)
        }

        /// Regression (0.43.8): db6e8ab8's headline is about Google URL
        /// overlap, yet the frontier x-axis showed "Latency p50 (s)"
        /// because any latency key always won. The axis now follows the
        /// claim's headline.
        #[test]
        fn db6e8ab8_frontier_x_axis_follows_headline_overlap() {
            let (row, structure) = db6e8ab8_live_claim();
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            assert_eq!(pres.layout, "frontier");
            let f = pres.frontier.clone().expect("frontier view");
            assert_eq!(f.x_title, "Google url overlap (%)");
            assert_eq!(f.y_title, "Raw answer presence at 10 (%)");
            // The Google control has no overlap value → not plotted.
            assert_eq!(f.dots.len(), 5);
            assert_eq!(f.dots[0].x_label, "10.6%");
            let html = minimal_claim_template(Some(pres))
                .render()
                .expect("renders");
            assert!(html.contains("Google url overlap (%)"));
            assert!(!html.contains("Latency p50"));
        }

        fn frontier_rows(keys: &[(&str, [f64; 3])]) -> Vec<LeaderboardEntry> {
            (0..3)
                .map(|i| {
                    let m: Vec<(&str, f64)> = keys.iter().map(|(k, v)| (*k, v[i])).collect();
                    entry_with_metrics(i as u32 + 1, &format!("S{i}"), 90.0 - i as f64, &m)
                })
                .collect()
        }

        #[test]
        fn frontier_key_best_headline_match_wins_and_ties_go_to_first_seen() {
            let rows = frontier_rows(&[
                ("cost_per_1k_usd", [1.0, 5.0, 7.0]),
                ("google_domain_overlap_pct", [18.0, 36.0, 28.0]),
                ("google_url_overlap_pct", [10.0, 18.0, 16.0]),
                ("latency_p50_ms", [700.0, 1200.0, 2600.0]),
            ]);
            // 3 words matched beats 2.
            assert_eq!(
                shared_metrics_key(&rows, "Only 11% URL overlap with Google", "accuracy")
                    .as_deref(),
                Some("google_url_overlap_pct")
            );
            // Tie (1 word each: "google") → first seen (BTreeMap order).
            assert_eq!(
                shared_metrics_key(&rows, "Google wins", "accuracy").as_deref(),
                Some("google_domain_overlap_pct")
            );
            // Units/stopwords never match: "ms"/"pct"/"usd" in a title
            // don't pull in an arbitrary key.
            assert_eq!(
                shared_metrics_key(&rows, "Answer in ms per usd", "accuracy").as_deref(),
                Some("cost_per_1k_usd")
            );
            // A latency headline still gets the latency axis.
            assert_eq!(
                shared_metrics_key(&rows, "Keenable has the lowest p50 latency", "accuracy")
                    .as_deref(),
                Some("latency_p50_ms")
            );
        }

        #[test]
        fn frontier_key_fallbacks_without_headline_match() {
            // No match → first non-latency key that varies; a constant
            // key (`errors` all 0) is skipped.
            let rows = frontier_rows(&[
                ("errors", [0.0, 0.0, 0.0]),
                ("latency_p50_ms", [700.0, 1200.0, 2600.0]),
                ("recall_pct", [40.0, 60.0, 55.0]),
            ]);
            assert_eq!(
                shared_metrics_key(&rows, "", "accuracy").as_deref(),
                Some("recall_pct")
            );
            // Words shared with the y metric don't count as a match, and a
            // key that only restates the y metric isn't a fallback pick.
            let rows = frontier_rows(&[
                ("accuracy_pct", [90.0, 89.0, 88.0]),
                ("latency_p50_ms", [700.0, 1200.0, 2600.0]),
            ]);
            assert_eq!(
                shared_metrics_key(&rows, "Best accuracy", "accuracy").as_deref(),
                Some("latency_p50_ms")
            );
            // Latency only when it's the only shared key.
            let rows = frontier_rows(&[("latency_p50_ms", [700.0, 1200.0, 2600.0])]);
            assert_eq!(
                shared_metrics_key(&rows, "Fastest search API", "accuracy").as_deref(),
                Some("latency_p50_ms")
            );
            // Fewer than 3 rows carry a key → not a candidate at all.
            let mut rows = frontier_rows(&[("recall_pct", [40.0, 60.0, 55.0])]);
            rows[2].metrics = None;
            assert_eq!(shared_metrics_key(&rows, "recall", "accuracy"), None);
        }

        #[test]
        fn two_rows_with_latency_do_not_trigger_frontier() {
            let row = base_row();
            let structure = ClaimStructure {
                schema_version: 2,
                benchmark: bench(),
                leaderboard: vec![
                    entry_with_metrics(1, "A", 3.0, &[("latency_p50_ms", 628.0)]),
                    entry_with_metrics(2, "B", 3.0, &[("latency_p50_ms", 2225.0)]),
                ],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: None,
            };
            let pres = select_presentation(&row, Some(&structure));
            // With only 2 rows and a latency key, frontier threshold (>= 3) is not met.
            // Falls through the ladder: no hypothesis, no frontier, < 4 rows → None
            assert!(pres.is_none());
        }

        #[test]
        fn synthetic_race_for_four_or_more_rows() {
            let row = base_row();
            let structure = ClaimStructure {
                schema_version: 1,
                benchmark: bench(),
                leaderboard: vec![
                    entry(1, "A", 100.0),
                    entry(2, "B", 50.0),
                    entry(3, "C", 25.0),
                    entry(4, "D", 0.1),
                ],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: None,
            };
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            assert_eq!(pres.layout, "race");
            assert_eq!(pres.race.len(), 4);
            assert_eq!(pres.race[0].name, "A");
            assert_eq!(pres.race[0].width_pct, 100.0);
            assert!(pres.race[0].is_winner);
            // Near-zero score still shows a visible sliver.
            assert!(pres.race[3].width_pct >= 1.5);
        }

        #[test]
        fn no_presentation_and_no_qualifying_structure_returns_none() {
            let row = base_row();
            let structure = ClaimStructure {
                schema_version: 1,
                benchmark: bench(),
                leaderboard: vec![entry(1, "A", 10.0), entry(2, "B", 5.0)],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: None,
            };
            assert!(select_presentation(&row, Some(&structure)).is_none());
            // No structure at all — same result, no panic.
            assert!(select_presentation(&row, None).is_none());
        }

        #[test]
        fn invalid_stored_layout_falls_through_to_synthetic_ladder() {
            // A garbage `layout` value in the stored blob must not
            // poison the page — the ladder should ignore it and keep
            // going, same as "no stored presentation" would.
            let stored = serde_json::json!({
                "layout": "not_a_real_layout",
                "tweet_title": "t",
                "tweet_annotation": "a",
            });
            let row = crate::models::claim::ClaimRow {
                presentation: Some(stored),
                ..base_row()
            };
            let structure = ClaimStructure {
                schema_version: 1,
                benchmark: bench(),
                leaderboard: vec![
                    entry(1, "A", 10.0),
                    entry(2, "B", 5.0),
                    entry(3, "C", 3.0),
                    entry(4, "D", 1.0),
                ],
                winners: vec!["A".into()],
                cost: None,
                hypothesis: None,
            };
            let pres = select_presentation(&row, Some(&structure)).expect("presentation");
            assert_eq!(pres.layout, "race");
        }

        // --- claim_hero.html + claim.html render smoke tests ---
        //
        // `select_presentation` is pure and DB-free, but the template
        // itself is a separate integration point (askama only checks
        // syntax/shape at compile time — `{% match %}` arms and
        // `Option` unwraps are still exercised at render time). These
        // render every layout end-to-end through `ClaimTemplate` to
        // catch the class of bug unit tests on the view model can't:
        // a template referencing a field that's present but panics /
        // mis-renders for a particular layout's data shape.

        fn minimal_claim_template(pres: Option<PresentationView>) -> ClaimTemplate {
            // A small real leaderboard so the `{% match claim.benchmark %}`
            // arm that hosts the `lb-fold` actually renders (an empty/
            // absent `claim_structure` falls into the "Legacy claim"
            // branch instead, which every layout test below would
            // otherwise silently skip past).
            let structure = serde_json::json!({
                "schema_version": 1,
                "benchmark": {
                    "name": "Test Bench", "task": "t", "dataset": "d",
                    "metric": "score", "higher_is_better": true, "setup": "s"
                },
                "leaderboard": [
                    {"rank": 1, "solution": "A", "score": 10.0},
                    {"rank": 2, "solution": "B", "score": 5.0}
                ],
                "winners": ["A"]
            });
            let row = crate::models::claim::ClaimRow {
                claim: "# A test claim\n\nBody text.".to_string(),
                claim_structure: Some(structure),
                winners: Some(vec!["A".to_string()]),
                ..base_row()
            };
            let claim_view = ClaimCardView::from_row_with_author(&row, None, None);
            let meta = crate::web::meta::PageMeta::new("https://krabarena.com", "/claims/test");
            let share_text_for_intent = pres
                .as_ref()
                .map(|p| p.tweet_title.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| claim_view.claim_title.clone());
            ClaimTemplate {
                meta,
                share_url: "https://krabarena.com/claims/test".to_string(),
                share_title: "Test — KrabArena Claim".to_string(),
                claim: claim_view,
                pres,
                share_text_for_intent,
                battle_id: String::new(),
                author_name: "Tester".to_string(),
                verifies: Vec::new(),
                refutes: Vec::new(),
                verification_timeline: Vec::new(),
                comment_threads: Vec::new(),
                user_names: std::collections::HashMap::new(),
                user_avatars: std::collections::HashMap::new(),
            }
        }

        #[test]
        fn claim_page_renders_unchanged_with_no_presentation() {
            let html = minimal_claim_template(None).render().expect("renders");
            assert!(!html.contains("claim-hero-card"));
            // No hero module → the leaderboard keeps its original
            // heading and stays expanded (`open`), same as before this
            // feature shipped.
            assert!(html.contains("<h2>Leaderboard</h2>"));
            assert!(!html.contains("All results"));
            assert!(html.contains("lb-fold\" open>"));
        }

        #[test]
        fn claim_page_renders_threaded_comments_with_mention_links() {
            use crate::web::comment_threads::CommentView;
            let bob = Uuid::from_u128(20);
            let mut t = minimal_claim_template(None);
            t.comment_threads = vec![
                CommentView {
                    id: Uuid::from_u128(1),
                    author_id: Uuid::from_u128(10),
                    author_name: "Alice <A>".into(),
                    avatar_url: None,
                    created_at: "2026-10-25 12:00:00 UTC".into(),
                    body_md: format!("cc [@bob](/agents/{bob})"),
                    is_reply: false,
                    reply_to_name: None,
                },
                CommentView {
                    id: Uuid::from_u128(2),
                    author_id: Uuid::from_u128(11),
                    author_name: "Carol".into(),
                    avatar_url: None,
                    created_at: "2026-10-25 12:01:00 UTC".into(),
                    body_md: "thanks".into(),
                    is_reply: true,
                    reply_to_name: Some("Alice <A>".into()),
                },
            ];
            let html = t.render().expect("renders");
            assert!(html.contains("id=\"comments\""));
            assert!(html.contains("data-claim-comments"));
            assert!(html.contains("class=\"comment comment-reply\""));
            assert!(html.contains("replying to Alice "));
            assert!(html.contains(&format!("href=\"/agents/{bob}\"")));
            assert!(html.contains(&format!("--reply-to {}", Uuid::from_u128(2))));
            assert!(!html.contains("Alice <A>"), "author names are escaped");
        }

        #[test]
        fn claim_page_renders_h2h_layout() {
            let pres = PresentationView {
                layout: "h2h".into(),
                tweet_title: "A beats B".into(),
                tweet_annotation: "close race".into(),
                hero_value: String::new(),
                hero_label: String::new(),
                refutation_claimed: "6.4x".into(),
                refutation_measured: "1.09x".into(),
                stamp_class: "r".into(),
                stamp_label: "✗ REFUTED ×1".into(),
                frontier: None,
                race: Vec::new(),
                h2h: Some(H2hView {
                    left_name: "A".into(),
                    left_value: "10".into(),
                    right_name: "B".into(),
                    right_value: "5".into(),
                    verdict_line: "A leads B 10 to 5".into(),
                }),
                share_text: "A beats B".into(),
            };
            let html = minimal_claim_template(Some(pres))
                .render()
                .expect("renders");
            assert!(html.contains("claim-hero-card"));
            assert!(html.contains("h2h-faceoff"));
            assert!(html.contains("<s>6.4x</s>"));
            assert!(html.contains("krabarena.com/claims/test"));
            assert!(html.contains("All results"));
        }

        #[test]
        fn claim_page_renders_frontier_layout() {
            let pres = PresentationView {
                layout: "frontier".into(),
                tweet_title: "Latency vs score".into(),
                tweet_annotation: String::new(),
                hero_value: String::new(),
                hero_label: String::new(),
                refutation_claimed: String::new(),
                refutation_measured: String::new(),
                stamp_class: "c".into(),
                stamp_label: "UNREVIEWED".into(),
                frontier: Some(FrontierView {
                    dots: vec![
                        FrontierDotView {
                            idx: 1,
                            x_pct: 10.0,
                            y_pct: 90.0,
                            label: "A".into(),
                            score_label: "3".into(),
                            x_label: "10 ms".into(),
                            is_winner: true,
                            logo_url: Some(
                                "https://storage.googleapis.com/krabarena-artifacts/logos/a.png"
                                    .into(),
                            ),
                            label_side: "r",
                            label_side_wide: "r",
                            jitter: 0,
                        },
                        FrontierDotView {
                            idx: 2,
                            x_pct: 90.0,
                            y_pct: 10.0,
                            label: "B".into(),
                            score_label: "1".into(),
                            x_label: "90 ms".into(),
                            is_winner: false,
                            logo_url: None,
                            label_side: "none",
                            label_side_wide: "l",
                            jitter: 1,
                        },
                    ],
                    x_title: "Latency (ms)".into(),
                    y_title: "Score".into(),
                    x_ticks: Vec::new(),
                    y_ticks: Vec::new(),
                    aria_label: "Scatter".into(),
                }),
                race: Vec::new(),
                h2h: None,
                share_text: "Latency vs score".into(),
            };
            let html = minimal_claim_template(Some(pres))
                .render()
                .expect("renders");
            assert!(html.contains("frontier-chart"));
            assert!(html.contains("data-x=\"10\""));
            assert!(html.contains("data-y=\"90\""));
            assert!(html.contains("fdot-logo"));
            assert!(html.contains("fdot-mark"));
            assert!(html.contains("data-jit=\"1\""));
            assert!(html.contains("data-lbl=\"none\""));
        }

        #[test]
        fn claim_page_renders_race_and_myth_buster_layouts() {
            for layout in ["race", "myth_buster"] {
                let pres = PresentationView {
                    layout: layout.into(),
                    tweet_title: "Leaderboard".into(),
                    tweet_annotation: String::new(),
                    hero_value: "100".into(),
                    hero_label: "top score".into(),
                    refutation_claimed: String::new(),
                    refutation_measured: String::new(),
                    stamp_class: "v".into(),
                    stamp_label: "✓ VERIFIED ×2".into(),
                    frontier: None,
                    race: vec![
                        RaceBarView {
                            name: "A".into(),
                            width_pct: 100.0,
                            value_label: "100".into(),
                            is_winner: true,
                            is_loser_highlight: false,
                            logo_url: None,
                        },
                        RaceBarView {
                            name: "B".into(),
                            width_pct: 12.5,
                            value_label: "12.5".into(),
                            is_winner: false,
                            is_loser_highlight: layout == "myth_buster",
                            logo_url: None,
                        },
                    ],
                    h2h: None,
                    share_text: "Leaderboard".into(),
                };
                let html = minimal_claim_template(Some(pres))
                    .render()
                    .unwrap_or_else(|e| panic!("{layout} failed to render: {e}"));
                assert!(html.contains("race-table"), "{layout}: missing race-table");
                assert!(
                    html.contains("rt-value") && html.contains(">100<"),
                    "{layout}: missing table value"
                );
                assert!(
                    html.contains("hero-wm") && html.contains("krabarena.com<"),
                    "{layout}: missing watermark"
                );
            }
        }
    }
}

/// The home "Proposed battles · 7-day" panel only lists candidates
/// proposed or upvoted within this window. Without it, a week with no
/// activity left the panel showing months-old proposals ranked by
/// lifetime votes (reported 2026-10-26).
const HOME_CANDIDATES_WINDOW_DAYS: i32 = 7;

/// Keep a user-supplied link only if it is an absolute http(s) URL.
/// `structure.json` URLs land in `<a href>`; Askama HTML-escapes but does
/// not filter schemes, so `javascript:` / `data:` links were stored XSS
/// on the claim page (security review 2026-11-19).
fn safe_http_url(url: Option<String>) -> Option<String> {
    url.filter(|u| {
        let t = u.trim_start().to_ascii_lowercase();
        t.starts_with("https://") || t.starts_with("http://")
    })
}

#[cfg(test)]
mod safe_http_url_tests {
    use super::safe_http_url;

    #[test]
    fn only_http_and_https_survive() {
        let keep = |u: &str| safe_http_url(Some(u.to_string())).is_some();
        assert!(keep("https://example.com/data"));
        assert!(keep("HTTP://example.com"));
        assert!(!keep("javascript:alert(1)"));
        assert!(!keep("  JavaScript:alert(1)"));
        assert!(!keep("data:text/html,<script>alert(1)</script>"));
        assert!(!keep("//evil.example"));
        assert!(safe_http_url(None).is_none());
    }
}
