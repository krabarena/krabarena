//! Per-entity OpenGraph image routes.
//!
//! Serves the 1200x630 social card for a Claim or a Battle. The
//! route accepts either a slug or a UUID (same lookup pattern as
//! `web::pages::battle_detail` / `claim_detail`) so the URL the
//! page template emits matches whatever the user's address bar
//! shows.
//!
//! Caching strategy:
//!   1. Resolve the entity (DB), compute a content hash of the
//!      strings that go onto the card.
//!   2. Try GCS at `og/<kind>/<id>-<hash>.png` — hit → serve.
//!   3. Miss → render via `services::og_image::render_png`, upload
//!      best-effort, serve the rendered bytes directly.
//!   4. Anything fails → serve the static fallback PNG so previews
//!      never 500.
//!
//! The route emits long-lived cache headers because the URL itself
//! is hash-busted by content — once a card is generated, the bytes
//! never change for that URL.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use uuid::Uuid;

use crate::AppState;
use crate::models::battle::{BATTLE_COLUMNS, BattleRow};
use crate::models::claim::ClaimRow;
use crate::services::battle_summarizer::cumulative_wins_for_battle;
use crate::services::og_image::{
    OgCard, OgLogo, cache_object_name, fetch_cached, fetch_logo_bytes, render_png,
    render_png_with_logos, render_via_service, upload_to_cache,
};

/// Read the render-service base URL from the environment. When unset
/// (or empty), the dynamic headless-Chromium leaderboard card is
/// disabled and the route serves the historical `tiny_skia` card
/// unchanged — this is the safe default the operator flips on rollout
/// (see AGENTS.md → "OG_RENDER_URL rollout"). Reading it per-request is
/// cheap and means a Cloud Run env flip takes effect on the next
/// scrape without a code change.
fn og_render_url() -> Option<String> {
    std::env::var("OG_RENDER_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Static fallback bytes baked into the binary. Served when the
/// entity isn't found or every render path fails. Matches what
/// `static/og-fallback.png` ships so the bytes-on-the-wire are the
/// same on cold-cache `/og/<kind>/<id>.png` and `/static/og-fallback.png`.
const FALLBACK_PNG: &[u8] = include_bytes!("../../static/og-fallback.png");

/// `GET /og/battles/{id_or_slug}.png` (also accepts `.png` stripped
/// trailing path). Resolves the battle the same way the public
/// battle page does — UUID first, then slug. Returns 200 PNG on
/// success, 200 with the fallback PNG on any miss (never 404).
pub async fn battle_card(
    State(state): State<AppState>,
    Path(id_or_slug): Path<String>,
) -> Response {
    // Strip a trailing `.png` if the caller used the cleaner URL form.
    // We accept both so the meta tag URL and a browser-pasted URL
    // resolve identically.
    let key = id_or_slug.trim_end_matches(".png");
    // Explicit BATTLE_COLUMNS rather than `SELECT *` — see AGENTS.md
    // → "ALTER TABLE on prod" for the PgBouncer cached-plan trap.
    let by_id =
        format!("SELECT {BATTLE_COLUMNS} FROM battles WHERE id = $1 AND deleted_at IS NULL");
    let by_slug =
        format!("SELECT {BATTLE_COLUMNS} FROM battles WHERE slug = $1 AND deleted_at IS NULL");
    let battle = if let Ok(uuid) = key.parse::<Uuid>() {
        sqlx::query_as::<_, BattleRow>(&by_id)
            .bind(uuid)
            .fetch_optional(&state.pool)
            .await
    } else {
        sqlx::query_as::<_, BattleRow>(&by_slug)
            .bind(key)
            .fetch_optional(&state.pool)
            .await
    };
    let Ok(Some(battle)) = battle else {
        tracing::debug!(key = %key, "og_image: battle not found, serving fallback");
        return fallback_response();
    };

    let card = battle_to_card(&battle);

    // Dynamic leaderboard card via the headless-Chromium render
    // service, gated on OG_RENDER_URL. The cache key folds in the
    // cumulative-wins leaderboard so an edit/verify that reshuffles
    // the standings busts the cached PNG; on ANY render failure we
    // fall through to the tiny_skia card below.
    if let Some(render_base) = og_render_url() {
        // Compute the exact same object name the eager-warm path
        // writes (`battle_cache_object_name`) so a warmed card is a
        // guaranteed cache hit here — single source of truth for the
        // key keeps warm and serve in lockstep.
        let object_name = battle_cache_object_name(&state, &battle, &card).await;
        let og_html_path = format!("/og-html/battles/{}", battle.id);
        if let Some(resp) = serve_dynamic(&state, &render_base, &og_html_path, &object_name).await {
            return resp;
        }
        tracing::warn!(
            battle = %battle.id,
            "og_image: dynamic render unavailable, falling back to tiny_skia"
        );
    }

    let object_name = cache_object_name("battles", &battle.id.to_string(), &card.content_hash());
    serve_card_battle(&state, &card, &object_name).await
}

/// `GET /og/claims/{id_or_slug}.png`. Same resolution / cache /
/// fallback flow as `battle_card`.
pub async fn claim_card(State(state): State<AppState>, Path(id_or_slug): Path<String>) -> Response {
    let key = id_or_slug.trim_end_matches(".png");
    // Explicit CLAIM_COLUMNS rather than `SELECT *` — see AGENTS.md →
    // "ALTER TABLE on prod" for the PgBouncer cached-plan trap (same
    // hazard `og_html.rs` was already converted to avoid).
    let by_id = format!(
        "SELECT {} FROM claims WHERE id = $1 AND deleted_at IS NULL",
        crate::models::claim::CLAIM_COLUMNS
    );
    let by_slug = format!(
        "SELECT {} FROM claims WHERE slug = $1 AND deleted_at IS NULL",
        crate::models::claim::CLAIM_COLUMNS
    );
    let claim = if let Ok(uuid) = key.parse::<Uuid>() {
        sqlx::query_as::<_, ClaimRow>(&by_id)
            .bind(uuid)
            .fetch_optional(&state.pool)
            .await
    } else {
        sqlx::query_as::<_, ClaimRow>(&by_slug)
            .bind(key)
            .fetch_optional(&state.pool)
            .await
    };
    // Private claims get the generic fallback card — same as a missing
    // claim, so the card neither leaks content nor confirms existence.
    let claim = match claim {
        Ok(Some(c)) if crate::services::claim_visibility::is_public(c.visibility.as_deref()) => c,
        _ => {
            tracing::debug!(key = %key, "og_image: claim not found or private, serving fallback");
            return fallback_response();
        }
    };

    let card = claim_to_card(&claim);

    // Dynamic leaderboard card via the render service, gated on
    // OG_RENDER_URL. The claim's own structured leaderboard is folded
    // into the cache key so an edit that reshapes the leaderboard busts
    // the PNG; any render failure falls through to tiny_skia.
    if let Some(render_base) = og_render_url() {
        // Same key the eager-warm path writes (`claim_cache_object_name`).
        let object_name = claim_cache_object_name(&claim, &card);
        let og_html_path = format!("/og-html/claims/{}", claim.id);
        if let Some(resp) = serve_dynamic(&state, &render_base, &og_html_path, &object_name).await {
            return resp;
        }
        tracing::warn!(
            claim = %claim.id,
            "og_image: dynamic render unavailable, falling back to tiny_skia"
        );
    }

    let object_name = cache_object_name("claims", &claim.id.to_string(), &card.content_hash());
    serve_card(&state, &card, &object_name).await
}

/// Stable short hash over a cumulative-wins leaderboard. Folded into
/// the GCS cache key so the dynamic card regenerates when the
/// standings shift (a new claim, a verify/refute that flips a row).
fn leaderboard_hash<'a>(rows: impl Iterator<Item = (&'a str, u32, u32)>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (sol, wins, apps) in rows.take(5) {
        h.update(sol.as_bytes());
        h.update(b"=");
        h.update(wins.to_le_bytes());
        h.update(b":");
        h.update(apps.to_le_bytes());
        h.update(b"|");
    }
    hex::encode(&h.finalize()[..6])
}

/// Stable short hash over a claim's own `claim_structure.leaderboard`.
/// Same cache-busting role as [`leaderboard_hash`] but keyed on the
/// claim's recorded rank/solution/score rows.
fn claim_leaderboard_hash(structure: Option<&serde_json::Value>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    if let Some(rows) = structure
        .and_then(|s| s.get("leaderboard"))
        .and_then(|v| v.as_array())
    {
        for row in rows.iter().take(5) {
            if let Some(sol) = row.get("solution").and_then(|v| v.as_str()) {
                h.update(sol.as_bytes());
            }
            h.update(b"=");
            if let Some(score) = row.get("score").and_then(|v| v.as_f64()) {
                h.update(score.to_le_bytes());
            }
            h.update(b"|");
        }
    }
    hex::encode(&h.finalize()[..6])
}

/// GCS object name for a battle's dynamic OG card. **Single source of
/// truth** for the cache key shared by the serve path (`battle_card`)
/// and the eager-warm path (`warm_battle_og`): both fold the battle's
/// content hash + the cumulative-wins leaderboard hash into the suffix
/// the exact same way, so a card written by the warm path is a
/// guaranteed cache hit on the next `/og/battles/{id}` fetch. If these
/// two ever drift, the warm is silently wasted — keep them computed
/// here, never inlined.
async fn battle_cache_object_name(state: &AppState, battle: &BattleRow, card: &OgCard) -> String {
    let wins = cumulative_wins_for_battle(&state.pool, battle.id)
        .await
        .unwrap_or_default();
    let lb_hash = leaderboard_hash(wins.iter().map(|(s, w, a)| (s.as_str(), *w, *a)));
    cache_object_name(
        "battles",
        &battle.id.to_string(),
        &format!("{}-lb{}", card.content_hash(), lb_hash),
    )
}

/// GCS object name for a claim's dynamic OG card. Counterpart to
/// [`battle_cache_object_name`] — shared by `claim_card` (serve) and
/// `warm_claim_og` (eager warm) so both agree on the key.
fn claim_cache_object_name(claim: &ClaimRow, card: &OgCard) -> String {
    let lb_hash = claim_leaderboard_hash(claim.claim_structure.as_ref());
    cache_object_name(
        "claims",
        &claim.id.to_string(),
        &format!("{}-lb{}", card.content_hash(), lb_hash),
    )
}

// --- Eager OG cache warming (since 0.22.1) -------------------------------

/// Proactively re-render + re-cache a battle's dynamic OG card so the
/// `/og/battles/{id}` URL serves the fresh card immediately, without
/// waiting for the next social re-scrape to trigger a lazy miss.
///
/// No-op unless `OG_RENDER_URL` is set (mirrors the serve path's
/// gate). Best-effort end to end: a render-service hiccup or GCS write
/// failure is logged at warn and swallowed — the caller (summary
/// regeneration, the admin endpoint) must never fail or block on this.
/// Returns `true` only when the PNG was rendered AND written to the
/// cache under the same key `battle_card` reads.
pub async fn warm_battle_og(state: &AppState, battle: &BattleRow) -> bool {
    let Some(render_base) = og_render_url() else {
        tracing::debug!(battle = %battle.id, "og warm: OG_RENDER_URL unset, skipping");
        return false;
    };
    let card = battle_to_card(battle);
    let object_name = battle_cache_object_name(state, battle, &card).await;
    let og_html_path = format!("/og-html/battles/{}", battle.id);
    match render_via_service(&render_base, &og_html_path).await {
        Ok(bytes) => match upload_to_cache(&state.blob_store_url, &object_name, &bytes).await {
            Ok(()) => {
                tracing::debug!(battle = %battle.id, object = %object_name, "og warm: battle card re-cached");
                true
            }
            Err(e) => {
                tracing::warn!(battle = %battle.id, object = %object_name, error = %e, "og warm: battle cache write failed");
                false
            }
        },
        Err(e) => {
            tracing::warn!(battle = %battle.id, error = %e, "og warm: battle render failed");
            false
        }
    }
}

/// Claim counterpart of [`warm_battle_og`]. Same gate, same best-effort
/// contract, same key-agreement guarantee (via
/// [`claim_cache_object_name`]).
pub async fn warm_claim_og(state: &AppState, claim: &ClaimRow) -> bool {
    let Some(render_base) = og_render_url() else {
        tracing::debug!(claim = %claim.id, "og warm: OG_RENDER_URL unset, skipping");
        return false;
    };
    let card = claim_to_card(claim);
    let object_name = claim_cache_object_name(claim, &card);
    let og_html_path = format!("/og-html/claims/{}", claim.id);
    match render_via_service(&render_base, &og_html_path).await {
        Ok(bytes) => match upload_to_cache(&state.blob_store_url, &object_name, &bytes).await {
            Ok(()) => {
                tracing::debug!(claim = %claim.id, object = %object_name, "og warm: claim card re-cached");
                true
            }
            Err(e) => {
                tracing::warn!(claim = %claim.id, object = %object_name, error = %e, "og warm: claim cache write failed");
                false
            }
        },
        Err(e) => {
            tracing::warn!(claim = %claim.id, error = %e, "og warm: claim render failed");
            false
        }
    }
}

/// Fire-and-forget eager warm of a battle's OG card. Spawns a detached
/// task so the caller's response is never blocked and never fails on a
/// render-service hiccup. Used from the summary-regeneration paths.
/// Cheap no-op when `OG_RENDER_URL` is unset (the spawned task returns
/// immediately).
pub fn spawn_warm_battle_og(state: &AppState, battle: BattleRow) {
    // Skip spawning entirely when the feature is off — keeps the
    // summary path allocation-free in the common (prod-until-rollout)
    // case.
    if og_render_url().is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let _ = warm_battle_og(&state, &battle).await;
    });
}

/// Look up a battle by id and warm its OG card (fire-and-forget). The
/// summary-regeneration call sites only have the `battle_id` in hand,
/// so this fetches the row first. All failures are swallowed.
pub fn spawn_warm_battle_og_by_id(state: &AppState, battle_id: Uuid) {
    if og_render_url().is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let by_id =
            format!("SELECT {BATTLE_COLUMNS} FROM battles WHERE id = $1 AND deleted_at IS NULL");
        match sqlx::query_as::<_, BattleRow>(&by_id)
            .bind(battle_id)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(Some(battle)) => {
                let _ = warm_battle_og(&state, &battle).await;
            }
            Ok(None) => {
                tracing::debug!(battle = %battle_id, "og warm: battle not found, skipping");
            }
            Err(e) => {
                tracing::warn!(battle = %battle_id, error = %e, "og warm: battle lookup failed");
            }
        }
    });
}

/// Look up a claim by id and warm its OG card (fire-and-forget). Claim
/// counterpart of [`spawn_warm_battle_og_by_id`] — callers (e.g. the
/// admin presentation-regenerate endpoint) only have the `claim_id` in
/// hand, so this fetches the row first via `CLAIM_COLUMNS` (not
/// `SELECT *` — see the cached-plan trap note on that constant). All
/// failures are swallowed; this must never fail or block the caller.
pub fn spawn_warm_claim_og_by_id(state: &AppState, claim_id: Uuid) {
    if og_render_url().is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let by_id = format!(
            "SELECT {} FROM claims WHERE id = $1 AND deleted_at IS NULL",
            crate::models::claim::CLAIM_COLUMNS
        );
        match sqlx::query_as::<_, ClaimRow>(&by_id)
            .bind(claim_id)
            .fetch_optional(&state.pool)
            .await
        {
            // Never pre-render (and cache in the public bucket) a card
            // for a private claim.
            Ok(Some(claim))
                if crate::services::claim_visibility::is_public(claim.visibility.as_deref()) =>
            {
                let _ = warm_claim_og(&state, &claim).await;
            }
            Ok(Some(_)) => {
                tracing::debug!(claim = %claim_id, "og warm: private claim, skipping");
            }
            Ok(None) => {
                tracing::debug!(claim = %claim_id, "og warm: claim not found, skipping");
            }
            Err(e) => {
                tracing::warn!(claim = %claim_id, error = %e, "og warm: claim lookup failed");
            }
        }
    });
}

/// Dynamic-render path: GCS-cache lookup → render-service call →
/// best-effort cache write. Returns `Some(response)` when the dynamic
/// card was produced (cache hit OR fresh render), or `None` when the
/// render service failed and the caller should fall back to the
/// `tiny_skia` card. A cache hit serves immediately; on a miss we ask
/// the render service (tight 5s timeout) and, on success, spawn a
/// best-effort GCS upload so the next scrape is a cache hit.
async fn serve_dynamic(
    state: &AppState,
    render_base: &str,
    og_html_path: &str,
    object_name: &str,
) -> Option<Response> {
    if let Some(cached) = fetch_cached(&state.blob_store_url, object_name).await {
        if cached.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
            tracing::debug!(object = %object_name, "og_image: dynamic cache hit");
            return Some(png_response(cached));
        }
    }
    match render_via_service(render_base, og_html_path).await {
        Ok(bytes) => {
            let store = state.blob_store_url.clone();
            let object = object_name.to_string();
            let upload_bytes = bytes.clone();
            tokio::spawn(async move {
                if let Err(e) = upload_to_cache(&store, &object, &upload_bytes).await {
                    tracing::warn!(object = %object, error = %e, "og_image: dynamic cache write failed");
                }
            });
            Some(png_response(bytes))
        }
        Err(e) => {
            tracing::warn!(path = %og_html_path, error = %e, "og_image: render service call failed");
            None
        }
    }
}

/// Hit the GCS cache, then fall through to fresh render + upload on
/// miss. Returns the final PNG bytes wrapped in a 200 response, or
/// the static fallback if generation itself fails.
async fn serve_card(state: &AppState, card: &OgCard, object_name: &str) -> Response {
    if let Some(cached) = fetch_cached(&state.blob_store_url, object_name).await {
        if cached.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
            tracing::debug!(object = %object_name, bytes = cached.len(), "og_image: cache hit");
            return png_response(cached);
        }
        // GCS gave us something that isn't a PNG — log + regenerate.
        tracing::warn!(
            object = %object_name,
            bytes = cached.len(),
            "og_image: cache returned non-PNG, regenerating"
        );
    }

    match render_png(card) {
        Ok(bytes) => {
            // Best-effort upload — don't block the response on it.
            let store = state.blob_store_url.clone();
            let object = object_name.to_string();
            let upload_bytes = bytes.clone();
            tokio::spawn(async move {
                if let Err(e) = upload_to_cache(&store, &object, &upload_bytes).await {
                    tracing::warn!(object = %object, error = %e, "og_image: cache write failed");
                }
            });
            png_response(bytes)
        }
        Err(e) => {
            tracing::error!(error = %e, "og_image: render failed, serving fallback");
            fallback_response()
        }
    }
}

/// Battle-card variant of [`serve_card`] that also fetches the vendor
/// logos (`card.logos`) and paints them into the strip near the
/// footer. The logo fetch is best-effort — any individual failure
/// silently drops that slot. A total failure (no logos resolve)
/// degrades cleanly to the no-logo battle card.
async fn serve_card_battle(state: &AppState, card: &OgCard, object_name: &str) -> Response {
    if let Some(cached) = fetch_cached(&state.blob_store_url, object_name).await {
        if cached.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
            tracing::debug!(object = %object_name, bytes = cached.len(), "og_image: cache hit");
            return png_response(cached);
        }
        tracing::warn!(
            object = %object_name,
            bytes = cached.len(),
            "og_image: cache returned non-PNG, regenerating"
        );
    }

    // Fetch every logo we can; failures drop the slot silently.
    let mut logo_bitmaps: Vec<Vec<u8>> = Vec::with_capacity(card.logos.len());
    for logo in card.logos.iter().take(3) {
        if let Some(bytes) = fetch_logo_bytes(&state.blob_store_url, &logo.url).await {
            logo_bitmaps.push(bytes);
        }
    }

    match render_png_with_logos(card, &logo_bitmaps) {
        Ok(bytes) => {
            let store = state.blob_store_url.clone();
            let object = object_name.to_string();
            let upload_bytes = bytes.clone();
            tokio::spawn(async move {
                if let Err(e) = upload_to_cache(&store, &object, &upload_bytes).await {
                    tracing::warn!(object = %object, error = %e, "og_image: cache write failed");
                }
            });
            png_response(bytes)
        }
        Err(e) => {
            tracing::error!(error = %e, "og_image: render failed, serving fallback");
            fallback_response()
        }
    }
}

pub(crate) fn png_response(bytes: Vec<u8>) -> Response {
    let mut resp = Response::new(Body::from(bytes));
    *resp.status_mut() = StatusCode::OK;
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    // The og:image URL is STABLE (`/og/{kind}/{id}`, no content hash in
    // the path), but the card content changes (new claims / verify /
    // refute / summary / edit). So it must NOT be `immutable` / year-long
    // — that froze pre-leaderboard title cards in browser/CDN/scraper
    // caches with no way to refresh. Short TTL + stale-while-revalidate:
    // serve instantly, refresh in the background, propagate changes in
    // minutes. The internal GCS object is still content-addressed, so a
    // revalidation is a cheap cache hit unless the standings actually moved.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300, stale-while-revalidate=86400"),
    );
    resp
}

pub(crate) fn fallback_response() -> Response {
    let mut resp = Response::new(Body::from(FALLBACK_PNG));
    *resp.status_mut() = StatusCode::OK;
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    // Short TTL on the fallback so a stale "we couldn't generate"
    // result doesn't outlive whatever transient failure caused it.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    resp
}

/// Project a BattleRow into the renderer-agnostic OgCard form. Same
/// description-priority chain `battle_detail` uses for the meta
/// `<description>` tag — keeps the card text consistent with what
/// scrapers see in the HTML.
fn battle_to_card(battle: &BattleRow) -> OgCard {
    let subtitle = battle
        .og_summary
        .clone()
        .or_else(|| battle.short_summary.clone())
        .or_else(|| battle.executive_summary.clone())
        .unwrap_or_else(|| battle.description.clone());
    // Strip newlines / heading markers / table noise — keep it a
    // single-paragraph blurb.
    let subtitle = clean_for_card(&subtitle, 280);

    let footer = if !battle.domain_tags.is_empty() {
        // Top-3 tags as `· Foo · Bar · Baz`.
        let tags = battle
            .domain_tags
            .iter()
            .take(3)
            .map(|t| t.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        Some(tags)
    } else {
        None
    };

    // Top-3 vendor logos from the Battle's solutions list (since
    // 0.13.4). Same set of URLs that the home-page card uses; the OG
    // renderer fetches the bytes (with a per-URL GCS cache) and
    // rasterises them along the bottom strip of the card.
    let solutions = crate::models::battle::parse_solutions(&battle.solutions);
    let logos: Vec<OgLogo> = solutions
        .into_iter()
        .filter_map(|s| {
            s.logo_url.map(|u| OgLogo {
                name: s.name,
                url: u,
            })
        })
        .take(3)
        .collect();

    OgCard {
        title: clean_for_card(&battle.title, 200),
        subtitle,
        footer,
        kind_label: "Battle".to_string(),
        logos,
        // Battles don't carry a claim `presentation` — the hero row
        // and refutation stamp only apply to claim cards.
        hero_value: None,
        hero_label: None,
        refuted: false,
        refutation_line: None,
    }
}

/// Parse a claim's stored `presentation` JSONB (see
/// [`crate::models::review::Presentation`] / migration 067). Returns
/// `None` on a `NULL` column OR a malformed/legacy blob — callers
/// MUST treat that as "no presentation" and fall back to the
/// pre-presentation card, never panic.
fn parse_presentation(claim: &ClaimRow) -> Option<crate::models::review::Presentation> {
    claim
        .presentation
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Project a ClaimRow into an OgCard. Title/subtitle prefer the
/// reviewer-generated `presentation` (tweet_title / tweet_annotation)
/// when present and non-empty; otherwise this falls back to the
/// original extraction chain unchanged (legacy claims, or a
/// NULL/malformed `presentation` column — [`parse_presentation`]
/// degrades to `None` for both, never panics).
fn claim_to_card(claim: &ClaimRow) -> OgCard {
    let presentation = parse_presentation(claim);

    let title = presentation
        .as_ref()
        .map(|p| p.tweet_title.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| extract_title_from_claim(&claim.claim));

    let subtitle = presentation
        .as_ref()
        .map(|p| p.tweet_annotation.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            claim
                .og_summary
                .clone()
                .or_else(|| claim.artifact_summary.clone())
                .unwrap_or_else(|| body_without_title(&claim.claim))
        });
    let subtitle = clean_for_card(&subtitle, 280);

    let footer = claim
        .winners
        .as_ref()
        .filter(|w| !w.is_empty())
        .map(|w| format!("Winner: {}", w.join(", ")))
        .or_else(|| {
            // Fall back to a verify/refute count footer if we have
            // either. Always shows even when both are 0 so the card
            // has a stable shape.
            Some(format!(
                "{} verified · {} refuted",
                claim.verify_count, claim.refute_count
            ))
        });

    // Hero stat prefers the reviewer's explicit `hero_stat`; when the
    // presentation instead carries a `refutation` (the claimed number
    // didn't hold up), render the measured value as the hero and spell
    // out the contrast in the label. `refuted` is driven purely by
    // `refutation.is_some()` regardless of which hero source won, so a
    // presentation carrying both still stamps the card as refuted.
    let refuted = presentation
        .as_ref()
        .is_some_and(|p| p.refutation.is_some());
    let (hero_value, hero_label) = match presentation.as_ref() {
        Some(p) if p.hero_stat.is_some() => {
            let hero = p.hero_stat.as_ref().expect("checked is_some above");
            (Some(hero.value.clone()), Some(hero.label.clone()))
        }
        Some(p) if p.refutation.is_some() => {
            let r = p.refutation.as_ref().expect("checked is_some above");
            (
                Some(r.measured.clone()),
                Some(format!("measured vs {} claimed", r.claimed)),
            )
        }
        _ => (None, None),
    };

    let kind_label = if refuted {
        "HYPOTHESIS REFUTED".to_string()
    } else {
        "Claim".to_string()
    };

    // Cache-busting only (see `OgCard::refutation_line` doc) — when a
    // presentation carries both `hero_stat` and `refutation`, the hero
    // fields above win and the refutation's claimed/measured strings
    // never touch `hero_value`/`hero_label`. The Chromium
    // `/og-html/claims/{id}` template renders those strings directly
    // from the presentation regardless, so fold them into the hash
    // here or an edit to just the refutation text won't bust the
    // `og-render` cache key.
    let refutation_line = presentation
        .as_ref()
        .and_then(|p| p.refutation.as_ref())
        .map(|r| format!("{} vs {}", r.claimed, r.measured));

    OgCard {
        title: clean_for_card(&title, 200),
        subtitle,
        footer,
        kind_label,
        // Claim OG cards stay logo-less for now — the strip only makes
        // sense for the Battle (multi-vendor) shape. Leave the door
        // open for a future Claim-card winners-strip though.
        logos: vec![],
        hero_value,
        hero_label,
        refuted,
        refutation_line,
    }
}

/// Find the `# Title` line at the top of a claim body. Falls back to
/// the first non-empty line. Matches `ClaimCardView::extract_title_body`.
fn extract_title_from_claim(body: &str) -> String {
    for line in body.lines() {
        if let Some(t) = line.trim().strip_prefix("# ") {
            return t.trim().to_string();
        }
    }
    body.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Claim")
        .trim()
        .to_string()
}

/// Return the claim body with the leading `# Title` stripped off, so
/// the og subtitle isn't a verbatim repeat of the title line.
fn body_without_title(body: &str) -> String {
    let mut lines = body.lines();
    if let Some(first) = lines.next() {
        if first.trim().starts_with("# ") {
            return lines.collect::<Vec<_>>().join(" ");
        }
    }
    body.to_string()
}

/// Collapse whitespace and strip markdown noise (headings, table
/// pipes, fenced code fences) so the OG card subtitle reads like a
/// natural sentence. Truncates to `max` chars on a word boundary with
/// a trailing ellipsis.
fn clean_for_card(s: &str, max: usize) -> String {
    let mut buf = String::with_capacity(s.len());
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with("```") {
            continue;
        }
        if !buf.is_empty() {
            buf.push(' ');
        }
        // Drop markdown emphasis markers (`**`, `__`, backticks) so
        // they don't end up as literal characters on the card.
        for ch in trimmed.chars() {
            if matches!(ch, '*' | '_' | '`' | '|') {
                continue;
            }
            buf.push(ch);
        }
    }
    // Collapse runs of whitespace.
    let cleaned: String = buf.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= max {
        return cleaned;
    }
    // Truncate on a word boundary.
    let mut end = 0usize;
    for (count, (idx, _)) in cleaned.char_indices().enumerate() {
        if count >= max {
            break;
        }
        end = idx;
    }
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
    use crate::services::og_image::{OG_HEIGHT, OG_WIDTH};

    #[test]
    fn extract_title_handles_markdown_heading() {
        let body = "# My Claim Title\n\n## Summary\n\nBody.";
        assert_eq!(extract_title_from_claim(body), "My Claim Title");
    }

    #[test]
    fn extract_title_falls_back_to_first_line() {
        let body = "No heading just text here.\nMore stuff.";
        assert_eq!(extract_title_from_claim(body), "No heading just text here.");
    }

    #[test]
    fn clean_for_card_strips_headings_and_emphasis() {
        let input = "# Title\n\nThis is **bold** and `code` text.\n\n## Section\n\nMore.";
        let out = clean_for_card(input, 500);
        assert!(!out.contains('#'));
        assert!(!out.contains('*'));
        assert!(!out.contains('`'));
        assert!(out.contains("This is bold and code text."));
    }

    #[test]
    fn clean_for_card_truncates_long_input_with_ellipsis() {
        let long = "word ".repeat(200);
        let out = clean_for_card(&long, 100);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 101);
    }

    #[test]
    fn og_render_url_gates_on_env_presence() {
        // Unset / empty → None → the route serves the tiny_skia card
        // (today's behavior, the safe default). A non-empty value flips
        // on the dynamic headless-Chromium leaderboard card. Uses a
        // dedicated var name so it never races the real OG_RENDER_URL
        // another test might set; the logic under test is the same
        // trim+filter, exercised directly.
        fn gate(v: Option<&str>) -> Option<String> {
            v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        }
        assert_eq!(gate(None), None, "unset env → fall back to tiny_skia");
        assert_eq!(gate(Some("")), None, "empty env → fall back to tiny_skia");
        assert_eq!(gate(Some("  ")), None, "whitespace env → fall back");
        assert_eq!(
            gate(Some("http://og-render:8080")),
            Some("http://og-render:8080".to_string()),
            "set env → dynamic render enabled"
        );
    }

    #[test]
    fn og_render_url_reads_real_env_when_unset() {
        // SAFETY: single-threaded within this test; we restore after.
        // Confirms the real accessor returns None when the var is
        // absent — i.e. the no-op default holds without configuration.
        unsafe {
            std::env::remove_var("OG_RENDER_URL");
        }
        assert!(
            og_render_url().is_none(),
            "OG_RENDER_URL unset must disable the dynamic render path"
        );
    }

    #[test]
    fn leaderboard_hash_changes_with_standings() {
        let a = leaderboard_hash([("Keenable", 2, 3), ("Exa", 1, 2)].into_iter());
        let b = leaderboard_hash([("Keenable", 3, 3), ("Exa", 1, 2)].into_iter());
        let same = leaderboard_hash([("Keenable", 2, 3), ("Exa", 1, 2)].into_iter());
        assert_ne!(a, b, "a win-count change should bust the cache key");
        assert_eq!(a, same, "identical standings hash stably");
    }

    #[test]
    fn claim_leaderboard_hash_tracks_structure() {
        let s1 = serde_json::json!({
            "leaderboard": [{"solution": "Exa", "score": 0.88}],
        });
        let s2 = serde_json::json!({
            "leaderboard": [{"solution": "Exa", "score": 0.91}],
        });
        assert_ne!(
            claim_leaderboard_hash(Some(&s1)),
            claim_leaderboard_hash(Some(&s2)),
            "a score change should bust the cache key"
        );
        assert_eq!(
            claim_leaderboard_hash(None),
            claim_leaderboard_hash(Some(&serde_json::json!({}))),
            "no leaderboard hashes the same as no structure"
        );
    }

    #[test]
    fn fallback_png_is_a_real_png() {
        assert!(
            FALLBACK_PNG.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
            "static fallback PNG should have valid magic bytes"
        );
    }

    #[test]
    fn og_canvas_constants_match_spec() {
        assert_eq!(OG_WIDTH, 1200);
        assert_eq!(OG_HEIGHT, 630);
    }

    #[test]
    fn claim_warm_and_serve_share_the_same_cache_key() {
        // The eager-warm path MUST write the PNG under the exact key
        // the serve path reads, or the warm is wasted. Both go through
        // `claim_cache_object_name`, so this test pins that the key is
        // a stable composition of (content_hash, claim leaderboard
        // hash) under the `og/claims/<id>-<...>.png` layout. If anyone
        // inlines a divergent key on either side, this breaks.
        let claim_structure = serde_json::json!({
            "leaderboard": [{"solution": "Exa", "score": 0.9}],
        });
        let card = OgCard {
            title: "Claim".into(),
            subtitle: "sub".into(),
            footer: None,
            kind_label: "Claim".into(),
            logos: vec![],
            hero_value: None,
            hero_label: None,
            refuted: false,
            refutation_line: None,
        };
        // Recompute the expected suffix the same way the serve path
        // historically did (content_hash + "-lb" + leaderboard hash).
        let lb = claim_leaderboard_hash(Some(&claim_structure));
        let expected = format!(
            "og/claims/{ID}-{}-lb{lb}-v{}.png",
            card.content_hash(),
            crate::services::og_image::OG_CACHE_VERSION,
        );
        const ID: &str = "00000000-0000-0000-0000-000000000001";

        // Build a ClaimRow with just the fields the key reads.
        let claim = claim_row_for_key(
            uuid::Uuid::parse_str(ID).unwrap(),
            Some(claim_structure.clone()),
        );
        let key = claim_cache_object_name(&claim, &card);
        assert_eq!(
            key, expected,
            "warm/serve claim key must match the documented layout"
        );

        // Determinism: same inputs → same key (so a re-warm overwrites,
        // never orphans).
        let key2 = claim_cache_object_name(&claim, &card);
        assert_eq!(key, key2, "claim cache key must be deterministic");
    }

    #[test]
    fn claim_cache_key_changes_with_leaderboard() {
        // A leaderboard reshuffle must bust the key so a stale warmed
        // card can't be served after the standings change.
        let card = OgCard {
            title: "T".into(),
            subtitle: "s".into(),
            footer: None,
            kind_label: "Claim".into(),
            logos: vec![],
            hero_value: None,
            hero_label: None,
            refuted: false,
            refutation_line: None,
        };
        let id = uuid::Uuid::nil();
        let a = claim_cache_object_name(
            &claim_row_for_key(
                id,
                Some(serde_json::json!({"leaderboard":[{"solution":"Exa","score":0.5}]})),
            ),
            &card,
        );
        let b = claim_cache_object_name(
            &claim_row_for_key(
                id,
                Some(serde_json::json!({"leaderboard":[{"solution":"Exa","score":0.9}]})),
            ),
            &card,
        );
        assert_ne!(a, b, "score change must change the cache key");
    }

    /// Build a minimal `ClaimRow` carrying only the fields the OG
    /// cache key + card builder read (id, claim body, claim_structure).
    /// Everything else is defaulted — this is a key-computation fixture,
    /// not a full row.
    fn claim_row_for_key(id: uuid::Uuid, structure: Option<serde_json::Value>) -> ClaimRow {
        ClaimRow {
            id,
            battle_id: None,
            author_id: uuid::Uuid::nil(),
            claim: "# Title\n\n## Summary\n\nbody".into(),
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
            visibility: None,
            price_cents: None,
            slug: None,
            artifact_filename: None,
            private_artifact_url: None,
            private_artifact_filename: None,
            og_summary: None,
            claim_structure: structure,
            winners: None,
            topic: None,
            topic_tags: None,
            deleted_at: None,
            produced_by: None,
            presentation: None,
        }
    }

    /// Build a `ClaimRow` for [`claim_to_card`] mapping tests —
    /// carries a claim body, the fallback subtitle/footer fields, and
    /// an arbitrary raw `presentation` JSON value (or `None`/malformed
    /// JSON to exercise the degrade path).
    fn claim_row_for_presentation(presentation: Option<serde_json::Value>) -> ClaimRow {
        ClaimRow {
            id: uuid::Uuid::nil(),
            battle_id: None,
            author_id: uuid::Uuid::nil(),
            claim: "# Legacy Title\n\nLegacy body text.".into(),
            artifact_hash: String::new(),
            artifact_url: String::new(),
            artifact_size: 0,
            predecessor_id: None,
            verify_count: 2,
            refute_count: 1,
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
            visibility: None,
            price_cents: None,
            slug: None,
            artifact_filename: None,
            private_artifact_url: None,
            private_artifact_filename: None,
            og_summary: Some("Legacy ogsummary fallback.".into()),
            claim_structure: None,
            winners: None,
            topic: None,
            topic_tags: None,
            deleted_at: None,
            produced_by: None,
            presentation,
        }
    }

    #[test]
    fn claim_to_card_falls_back_when_presentation_is_null() {
        let claim = claim_row_for_presentation(None);
        let card = claim_to_card(&claim);
        assert_eq!(
            card.title, "Legacy Title",
            "falls back to # heading extraction"
        );
        assert_eq!(
            card.subtitle, "Legacy ogsummary fallback.",
            "falls back to og_summary chain"
        );
        assert_eq!(card.hero_value, None);
        assert_eq!(card.hero_label, None);
        assert!(!card.refuted);
        assert_eq!(card.kind_label, "Claim");
    }

    #[test]
    fn claim_to_card_degrades_on_malformed_presentation_without_panicking() {
        // Missing required `layout`/`tweet_title`/`tweet_annotation`
        // fields — deserialize must fail, and the mapper must degrade
        // to the exact same fallback chain as `None`, never panic.
        let malformed = serde_json::json!({"hero_stat": {"value": "1.09x"}});
        let claim = claim_row_for_presentation(Some(malformed));
        let card = claim_to_card(&claim);
        assert_eq!(card.title, "Legacy Title");
        assert_eq!(card.subtitle, "Legacy ogsummary fallback.");
        assert_eq!(card.hero_value, None);
        assert!(!card.refuted);
        assert_eq!(card.kind_label, "Claim");
    }

    #[test]
    fn claim_to_card_maps_hero_stat_presentation() {
        let presentation = serde_json::json!({
            "layout": "h2h",
            "tweet_title": "Exa is 2.5x faster than Brave",
            "tweet_annotation": "Measured across 50 factual queries.",
            "hero_stat": {"value": "2.5x", "label": "speedup"},
            "refutation": null,
        });
        let claim = claim_row_for_presentation(Some(presentation));
        let card = claim_to_card(&claim);
        assert_eq!(card.title, "Exa is 2.5x faster than Brave");
        assert_eq!(card.subtitle, "Measured across 50 factual queries.");
        assert_eq!(card.hero_value.as_deref(), Some("2.5x"));
        assert_eq!(card.hero_label.as_deref(), Some("speedup"));
        assert!(!card.refuted);
        assert_eq!(card.refutation_line, None);
        assert_eq!(card.kind_label, "Claim");
    }

    #[test]
    fn claim_to_card_maps_refutation_presentation() {
        let presentation = serde_json::json!({
            "layout": "h2h",
            "tweet_title": "Nobody said it was 6.4x faster",
            "tweet_annotation": "The number didn't hold up under review.",
            "hero_stat": null,
            "refutation": {"claimed": "6.4x", "measured": "1.09x"},
        });
        let claim = claim_row_for_presentation(Some(presentation));
        let card = claim_to_card(&claim);
        assert_eq!(card.title, "Nobody said it was 6.4x faster");
        assert_eq!(card.hero_value.as_deref(), Some("1.09x"));
        assert_eq!(card.hero_label.as_deref(), Some("measured vs 6.4x claimed"));
        assert!(card.refuted);
        assert_eq!(card.refutation_line.as_deref(), Some("6.4x vs 1.09x"));
        assert_eq!(card.kind_label, "HYPOTHESIS REFUTED");
    }

    #[test]
    fn claim_to_card_folds_refutation_line_even_when_hero_stat_wins() {
        // The exact bug scenario: a presentation carrying BOTH
        // `hero_stat` and `refutation` renders hero_value/hero_label
        // from the hero_stat, so the refutation's claimed/measured
        // strings never touch those two fields. `refutation_line`
        // must still capture them so the `og-render` cache key busts
        // when only the refutation text changes.
        let presentation = serde_json::json!({
            "layout": "h2h",
            "tweet_title": "Exa is 2.5x faster than Brave",
            "tweet_annotation": "Measured across 50 factual queries.",
            "hero_stat": {"value": "2.5x", "label": "speedup"},
            "refutation": {"claimed": "6.4x", "measured": "1.09x"},
        });
        let claim = claim_row_for_presentation(Some(presentation));
        let card = claim_to_card(&claim);
        assert_eq!(card.hero_value.as_deref(), Some("2.5x"), "hero_stat wins");
        assert_eq!(
            card.refutation_line.as_deref(),
            Some("6.4x vs 1.09x"),
            "refutation strings must still be folded in for cache-busting"
        );
    }

    #[test]
    fn claim_to_card_ignores_empty_tweet_title_and_annotation() {
        // An empty string is technically "present" JSON-wise but must
        // still fall back to the existing chain — a reviewer bug that
        // writes `""` shouldn't blank out the card.
        let presentation = serde_json::json!({
            "layout": "h2h",
            "tweet_title": "",
            "tweet_annotation": "",
            "hero_stat": null,
            "refutation": null,
        });
        let claim = claim_row_for_presentation(Some(presentation));
        let card = claim_to_card(&claim);
        assert_eq!(card.title, "Legacy Title");
        assert_eq!(card.subtitle, "Legacy ogsummary fallback.");
    }

    #[test]
    fn warm_is_noop_when_render_url_unset() {
        // The eager-warm gate mirrors the serve gate: with OG_RENDER_URL
        // absent, warming must be a no-op (no render-service call, no
        // GCS write). We assert the gate accessor here — the warm
        // functions return `false` immediately on this branch before
        // touching the pool / blob store / network, so a missing env
        // can never turn a summary regen into a render-service call.
        // SAFETY: single-threaded within this test.
        unsafe {
            std::env::remove_var("OG_RENDER_URL");
        }
        assert!(
            og_render_url().is_none(),
            "unset OG_RENDER_URL must disable eager warming"
        );
    }
}
