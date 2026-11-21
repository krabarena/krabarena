//! `/zoo` — The Crawler Zoo pages. Data + rules live in
//! [`crate::services::zoo`]; this module only shapes view models.
//!
//! Routes:
//! - `GET /zoo`                    — gate count, Bot of the Day, register, enclosures
//! - `GET /zoo/leaderboard?enc=…`  — species ranked by visits this week
//! - `GET /zoo/bots/{slug}`        — specimen file for one species
//! - `GET /zoo/trap/` (+ anything below) — the Trap Room (robots.txt-disallowed)

use askama::Template;
use axum::Extension;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;

use crate::AppState;
use crate::services::og_image::{OgCard, render_png};
use crate::services::ua_classifier::UaCategory;
use crate::services::zoo::{
    self, Conservation, Enclosure, FamilyStats, SpeciesDetail, ZooSnapshot, fmt_day, fmt_int,
};
use crate::services::zoo_feed::{self, FeedOutcome};
use crate::web::auth_context::AuthContext;
use crate::web::errors::{render_404, render_template_or_500};
use crate::web::meta::PageMeta;

// ---------------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------------

pub struct GateLine {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
}

pub struct Bar {
    pub label: String,
    pub value: String,
    /// 0..=100, hydrated into a CSS height by `dynamic-styles.js`.
    pub pct: String,
    pub tick: bool,
}

pub struct StatusChip {
    pub code: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub active: bool,
}

pub struct BotOfTheDay {
    pub plate: String,
    pub fig: String,
    pub date: String,
    pub slug: String,
    pub name: String,
    pub latin: String,
    pub enclosure: &'static str,
    pub family_label: &'static str,
    pub temperament_head: String,
    pub temperament_tail: String,
    pub operator: String,
    pub statuses: Vec<StatusChip>,
    pub today: String,
    pub week: String,
    pub resident_since: String,
}

pub struct RegisterRow {
    pub slug: String,
    pub name: String,
    pub latin: String,
    pub path: String,
    pub enclosure: &'static str,
    pub enclosure_key: &'static str,
    pub ago: String,
}

pub struct SpeciesLink {
    pub slug: String,
    pub name: String,
}

pub struct EnclosureCard {
    pub key: &'static str,
    pub name: &'static str,
    pub emoji: &'static str,
    pub blurb: &'static str,
    pub species: String,
    pub today: String,
    pub week: String,
    pub stars: Vec<SpeciesLink>,
}

pub struct LeaderRow {
    pub rank: usize,
    pub slug: String,
    pub name: String,
    pub latin: String,
    pub enclosure: &'static str,
    pub status_code: &'static str,
    pub status_key: &'static str,
    pub status_label: &'static str,
    pub today: String,
    pub week: String,
    pub month: String,
    pub compliance: String,
    pub trapped: String,
    pub last_seen: String,
}

pub struct EncTab {
    pub href: String,
    pub label: String,
    pub active: bool,
}

pub struct PathCount {
    pub path: String,
    pub count: String,
}

pub struct FieldNote {
    pub date: String,
    pub text: String,
}

pub struct SlotOption {
    pub slot: &'static str,
    pub snack: &'static str,
}

/// The food bowl block on a specimen page (also rendered alone as the
/// htmx response to a feed).
pub struct BowlView {
    pub slug: String,
    pub name: String,
    pub items: Vec<String>,
    pub last_meal: Option<String>,
    pub machine: Vec<SlotOption>,
    pub tokens_left: i64,
    pub tokens_total: i64,
    pub full: bool,
    pub flash: Option<String>,
}

pub struct MealLine {
    pub text: String,
    pub bowl_slug: String,
    pub ago: String,
}

pub struct Faq {
    pub q: String,
    pub a: String,
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "pages/zoo.html")]
struct ZooHomeTemplate {
    meta: PageMeta,
    open: bool,
    date: String,
    gate_total: String,
    gate_lines: Vec<GateLine>,
    disallowed: String,
    trapped: String,
    new_species: String,
    hourly: Vec<Bar>,
    botd: Option<BotOfTheDay>,
    register: Vec<RegisterRow>,
    enclosures: Vec<EnclosureCard>,
    species_total: String,
    updated: String,
    meals: Vec<MealLine>,
}

#[derive(Template)]
#[template(path = "pages/zoo_leaderboard.html")]
struct ZooLeaderboardTemplate {
    meta: PageMeta,
    open: bool,
    heading: String,
    blurb: String,
    tabs: Vec<EncTab>,
    rows: Vec<LeaderRow>,
}

#[derive(Template)]
#[template(path = "pages/zoo_bot.html")]
struct ZooBotTemplate {
    meta: PageMeta,
    bowl: BowlView,
    plate: String,
    fig: String,
    enclosure: &'static str,
    enclosure_key: &'static str,
    name: String,
    latin: String,
    about: String,
    operator: String,
    family_label: &'static str,
    token_display: String,
    temperament_head: String,
    temperament_tail: String,
    status_label: &'static str,
    status_key: &'static str,
    statuses: Vec<StatusChip>,
    first_seen: String,
    last_seen: String,
    today: String,
    week: String,
    all_time: String,
    trap_line: String,
    user_agent: Option<String>,
    docs_url: Option<String>,
    block_kind: &'static str,
    block_snippet: String,
    allow_snippet: String,
    daily: Vec<Bar>,
    daily_peak: String,
    daily_first: String,
    diet: Vec<PathCount>,
    hours: Vec<Bar>,
    hours_quip: String,
    compliance: String,
    compliance_line: String,
    trap_record: String,
    trap_record_line: String,
    notes: Vec<FieldNote>,
    faq: Vec<Faq>,
    neighbours: Vec<SpeciesLink>,
}

#[derive(Template)]
#[template(path = "partials/zoo_bowl.html")]
struct ZooBowlPartial {
    bowl: BowlView,
}

#[derive(Template)]
#[template(path = "pages/zoo_trap.html")]
struct ZooTrapTemplate {
    meta: PageMeta,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn zoo_home(State(state): State<AppState>) -> Response {
    let base = state.public_base_url.as_str();
    let meta = PageMeta::new(base, "/zoo")
        .with_title("The Crawler Zoo — a live menagerie of web crawlers")
        .with_description(
            "Every bot that visits KrabArena, caught and catalogued live: who reads robots.txt, \
             who ignores it, who walks into the trap.",
        )
        .with_og_image(og_url(base, "/og/zoo", None))
        .with_og_image_alt("The Crawler Zoo — bots that visited KrabArena today");
    let snap = zoo::snapshot(&state.pool).await;
    let mut tpl = match snap.as_deref() {
        Some(s) => home_view(meta, s),
        None => closed_home(meta),
    };
    if let Some(s) = snap.as_deref() {
        tpl.meals = meal_lines(&state.pool, s, None, 8).await;
    }
    let mut resp = render_template_or_500(tpl, base, "/zoo", "zoo_home");
    short_cache(&mut resp);
    resp
}

#[derive(Debug, Deserialize)]
pub struct LeaderboardQuery {
    enc: Option<String>,
}

pub async fn zoo_leaderboard(
    State(state): State<AppState>,
    Query(q): Query<LeaderboardQuery>,
) -> Response {
    let base = state.public_base_url.as_str();
    let enc = q.enc.as_deref().and_then(Enclosure::from_key);
    let path = match enc {
        Some(e) => format!("/zoo/leaderboard?enc={}", e.key()),
        None => "/zoo/leaderboard".to_string(),
    };
    let heading = match enc {
        Some(e) => format!("{} {}", e.emoji(), e.name()),
        None => "The whole zoo".to_string(),
    };
    let blurb = enc
        .map(|e| e.blurb().to_string())
        .unwrap_or_else(|| "Every species on file, busiest this week first.".to_string());
    let meta = PageMeta::new(base, &path)
        .with_title(format!("Crawler leaderboard — {heading}"))
        .with_description(format!(
            "Which web crawlers visit KrabArena most, and which ones respect robots.txt. {blurb}"
        ))
        .with_og_image(og_url(
            base,
            "/og/zoo/leaderboard",
            enc.map(|e| ("enc", e.key())),
        ))
        .with_og_image_alt(format!("Crawler leaderboard — {}", strip_emoji(&heading)));
    let snap = zoo::snapshot(&state.pool).await;
    let rows = snap
        .as_deref()
        .map(|s| leader_rows(s, enc))
        .unwrap_or_default();
    let mut tabs = vec![EncTab {
        href: "/zoo/leaderboard".to_string(),
        label: "All".to_string(),
        active: enc.is_none(),
    }];
    tabs.extend(Enclosure::ALL.into_iter().map(|e| EncTab {
        href: format!("/zoo/leaderboard?enc={}", e.key()),
        label: format!("{} {}", e.emoji(), e.name().trim_start_matches("The ")),
        active: enc == Some(e),
    }));
    let tpl = ZooLeaderboardTemplate {
        meta,
        open: snap.is_some(),
        heading,
        blurb,
        tabs,
        rows,
    };
    let mut resp = render_template_or_500(tpl, base, "/zoo/leaderboard", "zoo_leaderboard");
    short_cache(&mut resp);
    resp
}

pub async fn zoo_bot(
    State(state): State<AppState>,
    ctx: Option<Extension<AuthContext>>,
    Path(slug): Path<String>,
) -> Response {
    let base = state.public_base_url.as_str();
    let path = format!("/zoo/bots/{slug}");
    let Some(snap) = zoo::snapshot(&state.pool).await else {
        return render_404(base, &path);
    };
    let Some(fam) = snap.family(&slug) else {
        return render_404(base, &path);
    };
    let detail = zoo::species_detail(&state.pool, fam)
        .await
        .map(|d| (*d).clone())
        .unwrap_or_default();
    let meta = PageMeta::new(base, &path)
        .with_title(format!(
            "What is {}? User agent and how to block it",
            fam.info.name
        ))
        .with_description(format!(
            "{} — {} Seen {} times on KrabArena in the last 30 days; {}.",
            fam.info.name,
            fam.info.about,
            fmt_int(fam.month),
            fam.conservation().label().to_lowercase()
        ))
        .with_og_image(og_url(
            base,
            &format!("/og/zoo/bots/{}", fam.info.slug),
            None,
        ))
        .with_og_image_alt(format!(
            "{} — specimen card from the KrabArena Crawler Zoo",
            fam.info.name
        ));
    let device = ctx.map(|Extension(c)| c.device_id).unwrap_or_default();
    let bowl = bowl_view(&state.pool, &snap, fam, device, None).await;
    let tpl = bot_view(meta, &snap, fam, &detail, bowl);
    let mut resp = render_template_or_500(tpl, base, &path, "zoo_bot");
    // The bowl shows this visitor's tokens, so no shared caching.
    no_shared_cache(&mut resp);
    resp
}

#[derive(Debug, Deserialize)]
pub struct FeedForm {
    slot: String,
}

/// `POST /zoo/bots/{slug}/feed` — drop a vended snack in the bowl.
/// htmx callers get the re-rendered bowl; plain form posts are
/// redirected back to the page (POST/redirect/GET).
pub async fn zoo_feed(
    State(state): State<AppState>,
    ctx: Option<Extension<AuthContext>>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Form(form): Form<FeedForm>,
) -> Response {
    let base = state.public_base_url.as_str();
    let path = format!("/zoo/bots/{slug}");
    let Some(snap) = zoo::snapshot(&state.pool).await else {
        return render_404(base, &path);
    };
    let Some(fam) = snap.family(&slug) else {
        return render_404(base, &path);
    };
    let device = ctx.map(|Extension(c)| c.device_id).unwrap_or_default();
    let now = Utc::now();
    let flash = match zoo_feed::feed(&state.pool, &fam.info.slug, form.slot.trim(), device, now)
        .await
    {
        Ok(FeedOutcome::Fed { snack }) => format!(
            "You put the {snack} in the bowl. If {} comes by first, it eats it. If another bot gets here first, it steals it.",
            fam.info.name
        ),
        Ok(FeedOutcome::NoTokens) => {
            "You're out of tokens for today. The machine restocks your tokens at midnight UTC."
                .to_string()
        }
        Ok(FeedOutcome::BowlFull) => {
            "The bowl is full. Wait for a bot to come and eat.".to_string()
        }
        Ok(FeedOutcome::UnknownSlot) => "That slot is empty this week.".to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "zoo.feed.failed");
            "The vending machine jammed. Try again in a moment.".to_string()
        }
    };
    if headers.get("hx-request").is_none() {
        let mut resp = axum::response::Redirect::to(&format!("{path}#bowl")).into_response();
        no_shared_cache(&mut resp);
        return resp;
    }
    let bowl = bowl_view(&state.pool, &snap, fam, device, Some(flash)).await;
    let mut resp = render_template_or_500(ZooBowlPartial { bowl }, base, &path, "zoo_bowl_partial");
    no_shared_cache(&mut resp);
    resp
}

/// `GET /zoo/bowls/{slug}.txt` — the bowl as bots see it. The first
/// bot to fetch it eats everything inside; people only look.
pub async fn zoo_bowl_txt(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    let plain_404 = || {
        let mut r = (axum::http::StatusCode::NOT_FOUND, "No such bowl.\n").into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        r
    };
    let Some(slug) = file.strip_suffix(".txt") else {
        return plain_404();
    };
    let Some(snap) = zoo::snapshot(&state.pool).await else {
        return plain_404();
    };
    let Some(fam) = snap.family(slug) else {
        return plain_404();
    };
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let now = Utc::now();
    // Only a real GET from a bot eats; HEAD and people just look.
    let eater = (method == Method::GET)
        .then(|| zoo_feed::eater_for(ua))
        .flatten();
    let body = match eater {
        Some((eater_slug, eater_name)) => {
            let eaten = zoo_feed::eat(&state.pool, &fam.info.slug, &eater_slug, &eater_name)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "zoo.bowl.eat_failed");
                    Vec::new()
                });
            zoo_feed::bowl_text(
                &state.public_base_url,
                &fam.info.slug,
                &fam.info.name,
                Some(&eaten),
                &[],
                now,
            )
        }
        None => {
            let visible = zoo_feed::bowl_contents(&state.pool, &fam.info.slug)
                .await
                .unwrap_or_default();
            zoo_feed::bowl_text(
                &state.public_base_url,
                &fam.info.slug,
                &fam.info.name,
                None,
                &visible,
                now,
            )
        }
    };
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    resp
}

fn no_shared_cache(resp: &mut Response) {
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
}

async fn bowl_view(
    pool: &sqlx::PgPool,
    snap: &ZooSnapshot,
    fam: &FamilyStats,
    device: uuid::Uuid,
    flash: Option<String>,
) -> BowlView {
    let now = Utc::now();
    let slug = fam.info.slug.as_str();
    let (items, tokens, meals) = tokio::join!(
        zoo_feed::bowl_contents(pool, slug),
        zoo_feed::tokens_left(pool, device, now),
        meal_lines(pool, snap, Some(slug), 1),
    );
    let items = items.unwrap_or_default();
    BowlView {
        slug: slug.to_string(),
        name: fam.info.name.clone(),
        full: items.len() as i64 >= zoo_feed::BOWL_CAPACITY,
        items: items
            .iter()
            .map(|i| format!("{} · left {}", i.snack, zoo::ago(i.created_at, now)))
            .collect(),
        last_meal: meals
            .into_iter()
            .next()
            .map(|m| format!("Last time, {}, {}.", m.text, m.ago)),
        machine: zoo_feed::machine_for(now.date_naive())
            .into_iter()
            .map(|(slot, snack)| SlotOption { slot, snack })
            .collect(),
        tokens_left: tokens.unwrap_or(zoo_feed::TOKENS_PER_DAY),
        tokens_total: zoo_feed::TOKENS_PER_DAY,
        flash,
    }
}

async fn meal_lines(
    pool: &sqlx::PgPool,
    snap: &ZooSnapshot,
    bowl: Option<&str>,
    limit: i64,
) -> Vec<MealLine> {
    let now = Utc::now();
    zoo_feed::recent_meals(pool, bowl, limit)
        .await
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let owner = snap
                .family(&m.bowl_slug)
                .map(|f| f.info.name.clone())
                .unwrap_or_else(|| m.bowl_slug.clone());
            MealLine {
                text: zoo_feed::meal_sentence(m, &owner),
                bowl_slug: m.bowl_slug.clone(),
                ago: zoo::ago(m.eaten_at, now),
            }
        })
        .collect()
}

/// The Trap Room. Any path under `/zoo/trap/` lands here; the visit is
/// already logged by the auth middleware (that's the whole point).
pub async fn zoo_trap(State(state): State<AppState>) -> Response {
    let base = state.public_base_url.as_str();
    let meta = PageMeta::new(base, zoo::TRAP_PREFIX)
        .with_title("The Trap Room")
        .with_description("You were not supposed to come in here.")
        .with_robots("noindex,nofollow");
    let mut resp =
        render_template_or_500(ZooTrapTemplate { meta }, base, zoo::TRAP_PREFIX, "zoo_trap");
    resp.headers_mut().insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn short_cache(resp: &mut Response) {
    if resp.status().is_success() {
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        );
    }
}

// ---------------------------------------------------------------------------
// OG / Twitter cards (`/og/zoo…`)
// ---------------------------------------------------------------------------
//
// Rendered with the same tiny-skia renderer as claim cards
// (`og_image::render_png`). Numbers move every minute, so cards are NOT
// written to the GCS OG cache (it would fill with near-duplicates);
// instead the PNG bytes are memoised in-process by content hash for
// [`OG_TTL`], and the og:image URL carries `?d=<UTC day>` so social
// scrapers re-fetch at most daily. `/og/` paths are not logged as visits.

const OG_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

fn og_cache() -> &'static crate::cache::TtlCache<Vec<u8>> {
    static C: std::sync::OnceLock<crate::cache::TtlCache<Vec<u8>>> = std::sync::OnceLock::new();
    C.get_or_init(|| crate::cache::TtlCache::new(OG_TTL))
}

/// Absolute og:image URL with a daily cache-buster (and optional query).
fn og_url(base: &str, path: &str, extra: Option<(&str, &str)>) -> String {
    let base = base.trim_end_matches('/');
    let day = Utc::now().date_naive();
    match extra {
        Some((k, v)) => format!("{base}{path}?{k}={v}&d={day}"),
        None => format!("{base}{path}?d={day}"),
    }
}

/// The card font has no emoji glyphs; drop anything outside the BMP
/// symbol ranges we use and tidy the spaces left behind.
fn strip_emoji(s: &str) -> String {
    s.chars()
        .filter(|c| (*c as u32) < 0x2000 || matches!(*c, '—' | '–' | '·' | '…' | '×'))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn base_card(kind: &str, title: String, subtitle: String) -> OgCard {
    OgCard {
        title: strip_emoji(&title),
        subtitle: strip_emoji(&subtitle),
        footer: None,
        kind_label: kind.to_string(),
        logos: Vec::new(),
        hero_value: None,
        hero_label: None,
        refuted: false,
        refutation_line: None,
    }
}

pub(crate) fn home_card(s: &ZooSnapshot) -> OgCard {
    let week: i64 = s.families.iter().map(|f| f.week).sum();
    let mut card = base_card(
        "CRAWLER ZOO",
        "The Crawler Zoo".to_string(),
        "Who reads robots.txt, who ignores it, who walks into the trap.".to_string(),
    );
    card.hero_value = Some(fmt_int(week));
    card.hero_label = Some("bot visits in the last 7 days".to_string());
    let botd = s
        .bot_of_the_day
        .as_deref()
        .and_then(|slug| s.family(slug))
        .map(|f| format!(" · Bot of the Day: {}", f.info.name))
        .unwrap_or_default();
    card.footer = Some(strip_emoji(&format!(
        "{} species on file{botd}",
        fmt_int(s.families.len() as i64)
    )));
    card
}

pub(crate) fn leaderboard_card(s: &ZooSnapshot, enc: Option<Enclosure>) -> OgCard {
    let members: Vec<&FamilyStats> = s
        .families
        .iter()
        .filter(|f| enc.is_none_or(|e| f.enclosure == e))
        .collect();
    let (title, subtitle) = match enc {
        Some(e) => (
            format!("Crawler leaderboard: {}", e.name()),
            e.blurb().to_string(),
        ),
        None => (
            "Crawler leaderboard".to_string(),
            "Which bots visit KrabArena most, and which obey robots.txt.".to_string(),
        ),
    };
    let mut card = base_card("LEADERBOARD", title, subtitle);
    card.hero_value = Some(fmt_int(members.iter().map(|f| f.week).sum()));
    card.hero_label = Some(format!(
        "visits this week from {} species",
        fmt_int(members.len() as i64)
    ));
    let top: Vec<&str> = members
        .iter()
        .filter(|f| f.week > 0)
        .take(2)
        .map(|f| f.info.name.as_str())
        .collect();
    if !top.is_empty() {
        card.footer = Some(strip_emoji(&format!("Top this week: {}", top.join(", "))));
    }
    card
}

pub(crate) fn bot_card(f: &FamilyStats) -> OgCard {
    let status = f.conservation();
    let mut card = base_card(
        "SPECIMEN",
        f.info.name.clone(),
        format!("{} · {}", f.info.latin, f.enclosure.name()),
    );
    card.hero_value = Some(f.compliance_label());
    card.hero_label = Some(og_status_label(status).to_string());
    // Red pill for bots that break robots.txt without reading it.
    card.refuted = status == Conservation::Feral;
    card.footer = Some(strip_emoji(&format!(
        "{} visits in 30 days · operated by {}",
        fmt_int(f.month),
        f.info.operator
    )));
    card
}

/// Short status caption that fits beside the hero number on a card.
fn og_status_label(c: Conservation) -> &'static str {
    match c {
        Conservation::Respects => "of requests obeyed robots.txt",
        Conservation::Selective => "obeyed · reads robots.txt, breaks it",
        Conservation::Feral => "obeyed · never read robots.txt",
        Conservation::Unassessed => "obeyed · no robots.txt record yet",
    }
}

fn render_card(card: &OgCard) -> Response {
    let key = card.content_hash();
    let bytes = match og_cache().get(&key) {
        Some(b) => Ok(b),
        None => render_png(card).inspect(|b| og_cache().put(key.clone(), b.clone())),
    };
    match bytes {
        Ok(b) => crate::routes::og::png_response(b),
        Err(e) => {
            tracing::warn!(error = %e, "zoo.og.render_failed");
            crate::routes::og::fallback_response()
        }
    }
}

pub async fn og_home(State(state): State<AppState>) -> Response {
    match zoo::snapshot(&state.pool).await {
        Some(s) => render_card(&home_card(&s)),
        None => crate::routes::og::fallback_response(),
    }
}

pub async fn og_leaderboard(
    State(state): State<AppState>,
    Query(q): Query<LeaderboardQuery>,
) -> Response {
    let enc = q.enc.as_deref().and_then(Enclosure::from_key);
    match zoo::snapshot(&state.pool).await {
        Some(s) => render_card(&leaderboard_card(&s, enc)),
        None => crate::routes::og::fallback_response(),
    }
}

pub async fn og_bot(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let slug = slug.trim_end_matches(".png");
    match zoo::snapshot(&state.pool).await {
        Some(s) => match s.family(slug) {
            Some(f) => render_card(&bot_card(f)),
            None => crate::routes::og::fallback_response(),
        },
        None => crate::routes::og::fallback_response(),
    }
}

// ---------------------------------------------------------------------------
// View builders
// ---------------------------------------------------------------------------

fn status_chips(active: Conservation) -> Vec<StatusChip> {
    [
        Conservation::Respects,
        Conservation::Selective,
        Conservation::Feral,
    ]
    .into_iter()
    .map(|c| StatusChip {
        code: c.code(),
        key: c.key(),
        label: c.label(),
        active: c == active,
    })
    .collect()
}

/// Split "thoughtful. Looks at every page twice." into the italic
/// one-word head and the rest.
fn split_temperament(t: &str) -> (String, String) {
    match t.split_once(". ") {
        Some((head, tail)) => (format!("{head}."), tail.to_string()),
        None => (t.to_string(), String::new()),
    }
}

fn category_label(c: UaCategory) -> &'static str {
    use UaCategory as C;
    match c {
        C::SearchEngine => "Search engines",
        C::AiSearchApi => "AI search APIs",
        C::AiAssistant => "AI assistants",
        C::AiTraining => "AI scrapers",
        C::Seo => "SEO crawlers",
        C::SocialPreview => "Link unfurlers",
        C::Scanner => "Scanners",
        C::Script => "Scripts",
        C::OtherBot => "Unclassified bots",
        C::Unknown => "The nameless",
        C::Monitoring | C::Internal | C::Browser => "Domestic",
    }
}

fn bars(values: &[i64], labels: impl Fn(usize) -> String, tick_every: usize) -> Vec<Bar> {
    let max = values.iter().copied().max().unwrap_or(0).max(1);
    values
        .iter()
        .enumerate()
        .map(|(i, v)| Bar {
            label: labels(i),
            value: fmt_int(*v),
            pct: format!("{:.1}", 100.0 * *v as f64 / max as f64),
            tick: i % tick_every == 0,
        })
        .collect()
}

fn home_view(meta: PageMeta, s: &ZooSnapshot) -> ZooHomeTemplate {
    let gate_lines = Conservation::ALL
        .iter()
        .enumerate()
        .map(|(i, c)| GateLine {
            key: c.key(),
            label: c.gate_label(),
            value: fmt_int(s.gate.by_status[i]),
        })
        .collect();

    let botd = s
        .bot_of_the_day
        .as_deref()
        .and_then(|slug| s.family(slug))
        .map(|f| {
            let plate = zoo::plate_number(s.today);
            let (head, tail) = split_temperament(&f.info.temperament);
            BotOfTheDay {
                plate: zoo::roman(plate),
                fig: fig_code(&f.info.slug),
                date: fmt_day(s.today),
                slug: f.info.slug.clone(),
                name: f.info.name.clone(),
                latin: f.info.latin.clone(),
                enclosure: f.enclosure.name(),
                family_label: category_label(f.info.category),
                temperament_head: head,
                temperament_tail: tail,
                operator: f.info.operator.clone(),
                statuses: status_chips(f.conservation()),
                today: fmt_int(f.today),
                week: fmt_int(f.week),
                resident_since: fmt_day(f.first_seen.date_naive()),
            }
        });

    let now = s.generated_at;
    let register = s
        .register
        .iter()
        .map(|a| RegisterRow {
            slug: a.slug.clone(),
            name: a.name.clone(),
            latin: a.latin.clone(),
            path: a.path.clone(),
            enclosure: a.enclosure.name(),
            enclosure_key: a.enclosure.key(),
            ago: zoo::ago(a.at, now),
        })
        .collect();

    let enclosures = Enclosure::ALL
        .into_iter()
        .map(|e| {
            let members: Vec<&FamilyStats> =
                s.families.iter().filter(|f| f.enclosure == e).collect();
            EnclosureCard {
                key: e.key(),
                name: e.name(),
                emoji: e.emoji(),
                blurb: e.blurb(),
                species: fmt_int(members.len() as i64),
                today: fmt_int(members.iter().map(|f| f.today).sum()),
                week: fmt_int(members.iter().map(|f| f.week).sum()),
                stars: members
                    .iter()
                    .filter(|f| f.week > 0)
                    .take(4)
                    .map(|f| SpeciesLink {
                        slug: f.info.slug.clone(),
                        name: f.info.name.clone(),
                    })
                    .collect(),
            }
        })
        .collect();

    ZooHomeTemplate {
        meta,
        open: true,
        date: fmt_day(s.today),
        gate_total: fmt_int(s.gate.total),
        gate_lines,
        disallowed: fmt_int(s.gate.disallowed),
        trapped: fmt_int(s.gate.trapped),
        new_species: fmt_int(s.gate.new_species),
        hourly: bars(&s.hourly, |i| format!("{i:02}h"), 6),
        botd,
        register,
        enclosures,
        species_total: fmt_int(s.families.len() as i64),
        updated: zoo::fmt_hm(s.generated_at),
        meals: Vec::new(),
    }
}

fn closed_home(meta: PageMeta) -> ZooHomeTemplate {
    ZooHomeTemplate {
        meta,
        open: false,
        date: fmt_day(Utc::now().date_naive()),
        gate_total: "0".into(),
        gate_lines: Vec::new(),
        disallowed: "0".into(),
        trapped: "0".into(),
        new_species: "0".into(),
        hourly: Vec::new(),
        botd: None,
        register: Vec::new(),
        enclosures: Vec::new(),
        species_total: "0".into(),
        updated: String::new(),
        meals: Vec::new(),
    }
}

/// Specimen "Fig." code — a stable hex tag derived from the slug.
fn fig_code(slug: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in slug.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("0x{h:08X}")
}

fn leader_rows(s: &ZooSnapshot, enc: Option<Enclosure>) -> Vec<LeaderRow> {
    s.families
        .iter()
        .filter(|f| enc.is_none_or(|e| f.enclosure == e))
        .enumerate()
        .map(|(i, f)| {
            let c = f.conservation();
            LeaderRow {
                rank: i + 1,
                slug: f.info.slug.clone(),
                name: f.info.name.clone(),
                latin: f.info.latin.clone(),
                enclosure: f.enclosure.name(),
                status_code: c.code(),
                status_key: c.key(),
                status_label: c.label(),
                today: fmt_int(f.today),
                week: fmt_int(f.week),
                month: fmt_int(f.month),
                compliance: f.compliance_label(),
                trapped: if f.trapped > 0 {
                    format!("{}×", fmt_int(f.trapped))
                } else {
                    "—".to_string()
                },
                last_seen: zoo::ago(f.last_seen, s.generated_at),
            }
        })
        .collect()
}

fn bot_view(
    meta: PageMeta,
    s: &ZooSnapshot,
    f: &FamilyStats,
    d: &SpeciesDetail,
    bowl: BowlView,
) -> ZooBotTemplate {
    let info = &f.info;
    let name = info.name.clone();
    let now = s.generated_at;
    let status = f.conservation();
    let (head, tail) = split_temperament(&info.temperament);

    // § II — blocking advice.
    let (block_kind, block_snippet, allow_snippet) = if info.robots_tokens.is_empty() {
        (
            "ratelimit",
            "# It doesn't announce a name robots.txt can match,\n\
             # so there is nothing to put in robots.txt. Rate-limit instead:\n\
             # nginx\n\
             limit_req_zone $binary_remote_addr zone=perip:10m rate=2r/s;\n\
             limit_req zone=perip burst=20 nodelay;"
                .to_string(),
            String::new(),
        )
    } else {
        let agents: String = info
            .robots_tokens
            .iter()
            .map(|t| format!("User-agent: {t}\n"))
            .collect();
        (
            "robots",
            format!("# Block {name} from the whole site\n{agents}Disallow: /"),
            format!(
                "# Let it in, but keep it out of one room\n{agents}Allow: /\nDisallow: /members/"
            ),
        )
    };

    // § III — behaviour.
    let daily_vals: Vec<i64> = d.daily.iter().map(|(_, n)| *n).collect();
    let daily = bars(
        &daily_vals,
        |i| {
            d.daily
                .get(i)
                .map(|(day, _)| fmt_day(*day))
                .unwrap_or_default()
        },
        7,
    );
    let daily_peak = d
        .busiest_day()
        .map(|(day, n)| format!("peak {} / day, {}", fmt_int(n), fmt_day(day)))
        .unwrap_or_else(|| "no visits in the last 30 days".to_string());
    let daily_first = d
        .daily
        .first()
        .map(|(day, _)| fmt_day(*day))
        .unwrap_or_default();
    let hours = bars(&d.hours, |i| format!("{i:02}h"), 6);
    let hours_quip = match d.peak_hour() {
        Some(h) => format!(
            "Most active around {h:02}:00 UTC. {}",
            zoo::visiting_hours_quip(h)
        ),
        None => "Keeps no regular hours.".to_string(),
    };
    let diet = d
        .diet
        .iter()
        .map(|(p, n)| PathCount {
            path: p.clone(),
            count: fmt_int(*n),
        })
        .collect();

    let compliance_line = format!(
        "Requested {} disallowed page{} out of {} requests in the last 30 days. Read robots.txt {} time{}.",
        fmt_int(f.disallowed),
        plural(f.disallowed),
        fmt_int(f.month),
        fmt_int(f.robots_reads),
        plural(f.robots_reads),
    );
    let (trap_record, trap_record_line, trap_line) = if f.trapped > 0 {
        let last = f
            .last_trapped
            .map(|t| zoo::ago(t, now))
            .unwrap_or_else(|| "a while ago".into());
        (
            format!("{}×", fmt_int(f.trapped)),
            format!(
                "Walked through the hidden {} door. Last caught {last}.",
                zoo::TRAP_PREFIX
            ),
            format!("{} times", fmt_int(f.trapped)),
        )
    } else {
        (
            "Clean".to_string(),
            format!(
                "Has never followed the hidden link to {}. Either well trained or very lucky.",
                zoo::TRAP_PREFIX
            ),
            "Never".to_string(),
        )
    };

    // § V — keeper's field notes, newest first.
    let mut notes = Vec::new();
    if let Some((at, _)) = d.latest_paths.first() {
        let mut paths: Vec<&str> = d.latest_paths.iter().map(|(_, p)| p.as_str()).collect();
        paths.dedup();
        notes.push(FieldNote {
            date: fmt_day(at.date_naive()),
            text: format!(
                "{}. Arrived without announcement. Consumed {}. Departed.",
                zoo::fmt_hm(*at),
                join_and(&paths)
            ),
        });
    }
    if let Some(t) = f.last_trapped {
        notes.push(FieldNote {
            date: fmt_day(t.date_naive()),
            text: format!(
                "Caught walking through {}. The sign on the door was very clear.",
                zoo::TRAP_PREFIX
            ),
        });
    }
    if f.robots_reads > 0 && f.disallowed == 0 {
        notes.push(FieldNote {
            date: fmt_day(f.last_seen.date_naive()),
            text: "Observed reading robots.txt in full. Complied with every word.".to_string(),
        });
    }
    if let Some((day, n)) = d.busiest_day() {
        if n > 1 {
            notes.push(FieldNote {
                date: fmt_day(day),
                text: format!(
                    "Visited {} times in a single day. Keeper unable to establish why.",
                    fmt_int(n)
                ),
            });
        }
    }
    notes.push(FieldNote {
        date: fmt_day(f.first_seen.date_naive()),
        text: format!(
            "First recorded at the gate. Entered in the register as {}.",
            info.latin
        ),
    });

    // § VI — questions site owners ask.
    let respects = match status {
        Conservation::Respects => format!(
            "Yes. In {} requests observed here over the last 30 days it has read robots.txt and never fetched a disallowed page.",
            fmt_int(f.month)
        ),
        Conservation::Selective => format!(
            "Mostly. It reads robots.txt, but it has fetched a disallowed page {} times out of {} requests observed here.",
            fmt_int(f.disallowed),
            fmt_int(f.month)
        ),
        Conservation::Feral => format!(
            "No. It has never read robots.txt here and has fetched disallowed pages {} times.",
            fmt_int(f.disallowed)
        ),
        Conservation::Unassessed => "Unknown so far. It hasn't read robots.txt here, but it hasn't tried a disallowed page either.".to_string(),
    };
    let rankings = match info.category {
        UaCategory::SearchEngine => format!(
            "Yes. {name} feeds a search engine, so blocking it removes your pages from that engine's results."
        ),
        UaCategory::AiAssistant | UaCategory::AiSearchApi => format!(
            "Not in Google or Bing. But {name} decides whether AI assistants and agents can find and cite your pages, so blocking it makes you invisible there."
        ),
        UaCategory::Scanner | UaCategory::Script | UaCategory::Unknown => {
            "No. Nothing respectable will miss it.".to_string()
        }
        _ => format!("No. {name} isn't used by Google or Bing to rank pages."),
    };
    let per_day = f.week as f64 / 7.0;
    let faq = vec![
        Faq {
            q: format!("Does {name} respect robots.txt?"),
            a: respects,
        },
        Faq {
            q: format!("Will blocking {name} hurt my search rankings?"),
            a: rankings,
        },
        Faq {
            q: format!("How often does {name} visit?"),
            a: format!(
                "Here, about {} requests a day over the last week. Visits to your site depend on its size, how often it changes, and how many links point to it.",
                fmt_int(per_day.round() as i64)
            ),
        },
    ];

    let neighbours = s
        .families
        .iter()
        .filter(|o| o.enclosure == f.enclosure && o.info.slug != info.slug && o.week > 0)
        .take(6)
        .map(|o| SpeciesLink {
            slug: o.info.slug.clone(),
            name: o.info.name.clone(),
        })
        .collect();

    let token_display = if info.robots_tokens.is_empty() {
        if info.show_ua {
            info.family.clone()
        } else {
            "(browser)".to_string()
        }
    } else {
        info.robots_tokens.join(", ")
    };

    ZooBotTemplate {
        meta,
        bowl,
        plate: zoo::roman(zoo::plate_number(f.first_seen.date_naive())),
        fig: fig_code(&info.slug),
        enclosure: f.enclosure.name(),
        enclosure_key: f.enclosure.key(),
        name,
        latin: info.latin.clone(),
        about: info.about.clone(),
        operator: info.operator.clone(),
        family_label: category_label(info.category),
        token_display,
        temperament_head: head,
        temperament_tail: tail,
        status_label: status.label(),
        status_key: status.key(),
        statuses: status_chips(status),
        first_seen: fmt_day(f.first_seen.date_naive()),
        last_seen: zoo::ago(f.last_seen, now),
        today: fmt_int(f.today),
        week: fmt_int(f.week),
        all_time: fmt_int(f.all_time),
        trap_line,
        user_agent: if info.show_ua {
            f.sample_ua.clone()
        } else {
            None
        },
        docs_url: info.docs_url.clone(),
        block_kind,
        block_snippet,
        allow_snippet,
        daily,
        daily_peak,
        daily_first,
        diet,
        hours,
        hours_quip,
        compliance: f.compliance_label(),
        compliance_line,
        trap_record,
        trap_record_line,
        notes,
        faq,
        neighbours,
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperament_splits_on_first_sentence() {
        assert_eq!(
            split_temperament("thoughtful. Looks at every page twice."),
            (
                "thoughtful.".to_string(),
                "Looks at every page twice.".to_string()
            )
        );
        assert_eq!(
            split_temperament("domestic."),
            ("domestic.".to_string(), String::new())
        );
    }

    #[test]
    fn bars_scale_to_the_max() {
        let b = bars(&[0, 5, 10], |i| i.to_string(), 2);
        assert_eq!(b[0].pct, "0.0");
        assert_eq!(b[1].pct, "50.0");
        assert_eq!(b[2].pct, "100.0");
        assert!(b[0].tick && !b[1].tick && b[2].tick);
        // All-zero series must not divide by zero.
        let z = bars(&[0, 0], |i| i.to_string(), 1);
        assert_eq!(z[1].pct, "0.0");
    }

    #[test]
    fn joins_paths_like_a_keeper() {
        assert_eq!(join_and(&["/a"]), "/a");
        assert_eq!(join_and(&["/a", "/b"]), "/a and /b");
        assert_eq!(join_and(&["/a", "/b", "/c"]), "/a, /b and /c");
    }

    #[test]
    fn og_strings_drop_emoji_but_keep_punctuation() {
        assert_eq!(strip_emoji("🦜 The AI Aviary"), "The AI Aviary");
        assert_eq!(
            strip_emoji("Gptus vorax · The AI Aviary — x"),
            "Gptus vorax · The AI Aviary — x"
        );
    }

    #[test]
    fn og_url_carries_a_daily_buster_and_optional_query() {
        let day = Utc::now().date_naive().to_string();
        assert_eq!(
            og_url("https://krabarena.com/", "/og/zoo", None),
            format!("https://krabarena.com/og/zoo?d={day}")
        );
        assert_eq!(
            og_url(
                "https://krabarena.com",
                "/og/zoo/leaderboard",
                Some(("enc", "aviary"))
            ),
            format!("https://krabarena.com/og/zoo/leaderboard?enc=aviary&d={day}")
        );
    }

    #[test]
    fn og_cards_render_to_png() {
        let card = base_card(
            "SPECIMEN",
            "GPTBot".into(),
            "Gptus vorax · The AI Aviary. OpenAI's training crawler.".into(),
        );
        let mut card = card;
        card.hero_value = Some("99.9%".into());
        card.hero_label = Some("robots.txt compliance · Feral".into());
        card.refuted = true;
        card.footer = Some("3,291 visits in 30 days · operated by OpenAI".into());
        let png = render_png(&card).expect("renders");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn fig_codes_are_stable_hex() {
        assert_eq!(fig_code("gptbot"), fig_code("gptbot"));
        assert_ne!(fig_code("gptbot"), fig_code("claudebot"));
        assert_eq!(fig_code("x").len(), 10);
    }
}
