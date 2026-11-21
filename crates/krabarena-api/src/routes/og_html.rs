//! HTML OG-card templates — the source the `og-render` headless
//! Chromium microservice screenshots into a 1200x630 PNG.
//!
//! These routes (`GET /og-html/battles/{id}`, `GET /og-html/claims/{id}`)
//! emit a self-contained 1200x630 HTML page styled like the approved
//! prototype: brand header, a big title, a top-5 leaderboard, and a
//! footer with domain tags + claim/verify counts + `krabarena.com`.
//!
//! Data source:
//!   - Battle: the cumulative-wins leaderboard
//!     ([`battle_summarizer::cumulative_wins_for_battle`]) over the
//!     battle's non-deleted, non-net-refuted claim structures, joined
//!     to `battles.solutions[*].logo_url` for the vendor favicon. The
//!     `metric` column shows the appearances count ("N runs").
//!   - Claim: the claim's own `claim_structure.leaderboard[]`, top-5
//!     by rank, with the per-row `score` as the primary metric column.
//!
//! The page is intentionally "internal-ish" — it's the render
//! service's input — but it's harmless if public (no secrets, just
//! the same leaderboard data already on the battle/claim page). The
//! Inter font family is self-hosted from `/static/fonts` via
//! `@font-face` so the render service never has to reach Google
//! Fonts (it may run network-restricted).

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use uuid::Uuid;

use crate::AppState;
use crate::models::battle::{BATTLE_COLUMNS, BattleRow, parse_solutions};
use crate::models::claim::{CLAIM_COLUMNS, ClaimRow};
use crate::services::format_metric::format_metric_compact;
use crate::services::solution_names::{
    is_self_hosted_logo, load_vendor_patterns, lookup_vendor_logo, normalize_name,
};

/// One leaderboard row on the card. Mirrors the prototype's `.row`
/// shape: a medal-coloured rank, an optional vendor logo, the
/// solution name, an optional metric column, and the big right-aligned
/// "wins" number (for battles) or the rank's score restated (claims).
pub struct OgRow {
    /// 1-based display rank.
    pub rank: usize,
    /// `1`/`2`/`3` paint the gold/silver/bronze chips; anything else
    /// (`"x"`) uses the muted `.rx` style.
    pub medal: String,
    pub name: String,
    /// Vendor favicon / logo URL, when the battle's solutions list (or
    /// the claim leaderboard row) carries one.
    pub logo: Option<String>,
    /// Secondary metric column — appearances for battles, score for
    /// claims. Pre-formatted string; `None` omits the column.
    pub metric: Option<String>,
    /// Right-aligned headline number. Win count for battles; the rank's
    /// score for claims.
    pub wins: String,
}

#[derive(Template)]
#[template(path = "og/og_card_battle.html")]
struct BattleOgTemplate {
    title: String,
    rows: Vec<OgRow>,
    tags: String,
    stats: String,
    /// Verified count for the arcade health bar's green side. Stringified
    /// `verify_count` (already fetched by `battle_counts`).
    verified: String,
    /// Refuted count for the health bar's red side. Stringified
    /// `refute_count` (a sibling SUM in `battle_counts`).
    refuted: String,
    /// `true` when there's at least one verify or refute — gates the
    /// health bar so a brand-new battle doesn't render an empty/ambiguous
    /// bar.
    has_activity: bool,
    /// CSS width bucket for the verified segment, rounded to the nearest
    /// 10% (`w0`..`w100`). Inline widths are banned by `lint:templates`,
    /// so the proportional fill is expressed as a pre-defined class.
    health_class: String,
}

#[derive(Template)]
#[template(path = "og/og_card_claim.html")]
struct ClaimOgTemplate {
    title: String,
    rows: Vec<OgRow>,
    tags: String,
    /// Big headline number from the reviewer's `presentation.hero_stat`
    /// (since presentation surfacing on OG cards). Empty string → no
    /// hero row rendered.
    hero_value: String,
    /// Muted caption next to `hero_value` (e.g. "speedup"). Only
    /// meaningful when `hero_value` is non-empty.
    hero_label: String,
    /// Kind-pill text — `"CLAIM"` normally, `"HYPOTHESIS REFUTED"`
    /// when `pill_refuted` is set.
    pill_label: String,
    /// `true` when the stored presentation carries a `refutation` —
    /// flips the pill red and shows the struck-through claimed value.
    /// Safe to trust `refutation.is_some()` alone: the sanitizer
    /// (`services::presentation_check::sanitize_presentation`) already
    /// strips `refutation` unless the structure's hypothesis verdict
    /// is `"refutes"`.
    pill_refuted: bool,
    /// Struck-through claimed value (e.g. `"6.4×"`). Empty string →
    /// no refutation row rendered.
    refutation_claimed: String,
    /// Bold measured value shown next to the struck-through claim
    /// (e.g. `"1.09×"`).
    refutation_measured: String,
    /// Verified count for the arcade "✓ VERIFIED ×N" top-strip badge.
    /// Stringified `claim.verify_count` (already on the row).
    verified: String,
    /// Refuted count for the "✗ REFUTED ×N" badge shown instead of the
    /// verified badge when the claim is refuted. Stringified
    /// `claim.refute_count` (already on the row).
    refuted: String,
}

/// `GET /og-html/battles/{id_or_slug}` — battle leaderboard card HTML.
/// Resolves the battle UUID-first then by slug (same as the PNG route)
/// and 404s with a tiny HTML body when the battle isn't found so the
/// render service can distinguish "no card" from a transient error.
pub async fn battle_html(
    State(state): State<AppState>,
    Path(id_or_slug): Path<String>,
) -> Response {
    let key = id_or_slug.trim_end_matches(".html");
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
        return (
            StatusCode::NOT_FOUND,
            Html("<!doctype html><title>not found</title>"),
        )
            .into_response();
    };

    // Render the SAME standings the battle detail page shows (since
    // 0.28.6): the Olympic-points Claims Wins matrix, top-5 by points,
    // with each solution's brand/self-hosted logo. Previously this card
    // used `cumulative_wins_for_battle` (raw win count + "N runs"), which
    // diverged from the on-page table after the points/brand-logo work.
    let wins_matrix =
        crate::web::pages::battle_wins_matrix(&state.pool, &state.blob_store_url, &battle).await;
    let rows: Vec<OgRow> = wins_matrix
        .rows
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, r)| {
            let rank = i + 1;
            OgRow {
                rank,
                medal: medal_class(rank),
                name: r.solution_name.clone(),
                logo: r.logo_url.clone(),
                metric: None,
                // Headline number = the Olympic standing points (gold 5 /
                // silver 3 / bronze 1 over the shown benchmarks). Keep the
                // 🏅 prefix (matching the on-page + card standings) so the
                // social card makes clear the number is a medal score.
                wins: if r.points > 0 {
                    format!("🏅 {}", r.points)
                } else {
                    r.points.to_string()
                },
            }
        })
        .collect();

    // Footer: domain tags (top 3) on the left, claim/verify counts on
    // the right.
    let tags = battle
        .domain_tags
        .iter()
        .take(3)
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(" · ");
    let (claim_count, verify_count, refute_count) = battle_counts(&state, battle.id).await;
    let stats = format!("{claim_count} claims · {verify_count} verified");
    let (has_activity, health_class) = health_fill(verify_count, refute_count);

    let tmpl = BattleOgTemplate {
        title: battle.title.clone(),
        rows,
        tags,
        stats,
        verified: verify_count.to_string(),
        refuted: refute_count.to_string(),
        has_activity,
        health_class,
    };
    render_template(tmpl.render())
}

/// `GET /og-html/claims/{id_or_slug}` — claim leaderboard card HTML.
pub async fn claim_html(State(state): State<AppState>, Path(id_or_slug): Path<String>) -> Response {
    let key = id_or_slug.trim_end_matches(".html");
    let claim = if let Ok(uuid) = key.parse::<Uuid>() {
        let sql =
            format!("SELECT {CLAIM_COLUMNS} FROM claims WHERE id = $1 AND deleted_at IS NULL");
        sqlx::query_as::<_, ClaimRow>(&sql)
            .bind(uuid)
            .fetch_optional(&state.pool)
            .await
    } else {
        let sql =
            format!("SELECT {CLAIM_COLUMNS} FROM claims WHERE slug = $1 AND deleted_at IS NULL");
        sqlx::query_as::<_, ClaimRow>(&sql)
            .bind(key)
            .fetch_optional(&state.pool)
            .await
    };
    // Private claims 404 exactly like missing ones (the og-render
    // service fetches this page unauthenticated).
    let claim = match claim {
        Ok(Some(c)) if crate::services::claim_visibility::is_public(c.visibility.as_deref()) => c,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Html("<!doctype html><title>not found</title>"),
            )
                .into_response();
        }
    };

    // Reviewer-generated viral share layer (layout, tweet copy, hero
    // stat, refutation — see migration 067 /
    // `models::review::Presentation`). `None` on a NULL column OR a
    // malformed/legacy blob; every field below degrades gracefully
    // rather than panicking on either.
    let presentation: Option<crate::models::review::Presentation> = claim
        .presentation
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    // The red "HYPOTHESIS REFUTED" stamp trusts the stored
    // presentation's `refutation.is_some()` alone — the sanitizer
    // (`services::presentation_check::sanitize_presentation`) already
    // enforces refutation ⇔ structure.hypothesis.verdict == "refutes"
    // before a presentation is ever persisted, so re-checking the
    // structure here would be redundant.
    let pill_refuted = presentation
        .as_ref()
        .is_some_and(|p| p.refutation.is_some());

    // Title prefers the reviewer's tweet_title when present.
    let title = presentation
        .as_ref()
        .map(|p| p.tweet_title.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| extract_title_from_claim(&claim.claim));

    let (hero_value, hero_label) = presentation
        .as_ref()
        .and_then(|p| p.hero_stat.as_ref())
        .map(|h| (h.value.clone(), h.label.clone()))
        .unwrap_or_default();

    let (refutation_claimed, refutation_measured) = if pill_refuted {
        presentation
            .as_ref()
            .and_then(|p| p.refutation.as_ref())
            .map(|r| (r.claimed.clone(), r.measured.clone()))
            .unwrap_or_default()
    } else {
        (String::new(), String::new())
    };

    let pill_label = if pill_refuted {
        "HYPOTHESIS REFUTED".to_string()
    } else {
        "CLAIM".to_string()
    };

    // Logo resolution mirrors the claim PAGE (since 0.28.x): the OG card
    // used to read ONLY each leaderboard entry's inline `logo_url`, which
    // is almost always absent, so cards rendered with bare names. Resolve
    // in the same three layers: inline → parent battle's curated
    // `solutions[*].logo_url` → data-driven `vendor_logos` favicon.
    let battle_solutions = if let Some(bid) = claim.battle_id {
        sqlx::query_as::<_, (serde_json::Value,)>("SELECT solutions FROM battles WHERE id = $1")
            .bind(bid)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .map(|(s,)| parse_solutions(&s))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let vendor_patterns = load_vendor_patterns(&state.pool).await.unwrap_or_default();
    // GCP-only at every layer: an inline (author) or battle-solution logo
    // is only accepted when it's self-hosted; otherwise fall through to the
    // vendor map, which itself returns GCP-only URLs. Guarantees the OG
    // render never fetches an external favicon.
    //
    // AGENT-BRAND-WINS: the final `lookup_vendor_logo` fallback
    // deprioritizes a base LLM model in parens, so an `"<Agent> (<model>)"`
    // name resolves the agent/tool/company logo, not the model provider's.
    // See the `solution_names` module docs.
    let resolve_logo = |name: &str, inline: Option<String>| -> Option<String> {
        if let Some(u) = inline {
            if is_self_hosted_logo(&u) {
                return Some(u);
            }
        }
        let norm = normalize_name(name);
        battle_solutions
            .iter()
            .find(|s| {
                s.name_normalized == norm && s.logo_url.as_deref().is_some_and(is_self_hosted_logo)
            })
            .and_then(|s| s.logo_url.clone())
            .or_else(|| lookup_vendor_logo(name, &vendor_patterns).map(|r| r.url))
    };

    // The claim's own leaderboard, top-5 by ascending rank (top-4 when
    // the hero row renders — see `claim_row_cap`). Score is the primary
    // metric column; the right-hand "wins" slot restates the score's
    // headline value so the column still reads as a number.
    let mut rows: Vec<OgRow> = Vec::new();
    if let Some(structure) = claim.claim_structure.as_ref() {
        // Unit-aware formatting for the "wins" headline column — the
        // same `benchmark.metric_unit`/`metric` the claim page's race
        // bars and leaderboard table use, so a raw seconds value shows
        // as e.g. "102 ms" here too instead of a bare "0.102".
        let metric_unit = structure
            .get("benchmark")
            .and_then(|b| b.get("metric_unit"))
            .and_then(|v| v.as_str());
        let metric_name = structure
            .get("benchmark")
            .and_then(|b| b.get("metric"))
            .and_then(|v| v.as_str());
        if let Some(entries) = structure.get("leaderboard").and_then(|v| v.as_array()) {
            let mut sorted: Vec<&serde_json::Value> = entries.iter().collect();
            sorted.sort_by_key(|e| e.get("rank").and_then(|r| r.as_u64()).unwrap_or(u64::MAX));
            for (i, entry) in sorted
                .into_iter()
                .take(claim_row_cap(&hero_value, &refutation_claimed))
                .enumerate()
            {
                let rank = i + 1;
                let name = entry
                    .get("solution")
                    .and_then(|v| v.as_str())
                    .unwrap_or("—")
                    .to_string();
                let inline_logo = entry
                    .get("logo_url")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let logo = resolve_logo(&name, inline_logo);
                let score = entry.get("score").and_then(|v| v.as_f64());
                let score_str = score.map(|s| format_metric_compact(s, metric_unit, metric_name));
                rows.push(OgRow {
                    rank,
                    medal: medal_class(rank),
                    name,
                    logo,
                    // Metric column stays empty for claims (the score
                    // headline lives in the right-hand slot) so the row
                    // doesn't double-print the same number.
                    metric: None,
                    wins: score_str.unwrap_or_default(),
                });
            }
        }
    }

    // Footer: winner on the left (if any), verify/refute on the right.
    let winners = claim.winners.clone().unwrap_or_default();
    let tags = if winners.is_empty() {
        String::new()
    } else {
        format!("Winner: {}", winners.join(", "))
    };
    let tmpl = ClaimOgTemplate {
        title,
        rows,
        tags,
        hero_value,
        hero_label,
        pill_label,
        pill_refuted,
        refutation_claimed,
        refutation_measured,
        verified: claim.verify_count.to_string(),
        refuted: claim.refute_count.to_string(),
    };
    render_template(tmpl.render())
}

/// Health-bar fill bucket for the battle card. Returns `(has_activity,
/// class)` where `class` is `w0`..`w100` in 10% steps — the verified
/// share of (verified + refuted), rounded to the nearest 10%. When
/// there's no activity at all the caller hides the bar (`has_activity ==
/// false`); the class is still a valid `w0` so the template never emits
/// an unknown class.
fn health_fill(verified: i64, refuted: i64) -> (bool, String) {
    let total = verified + refuted;
    if total <= 0 {
        return (false, "w0".to_string());
    }
    let pct = (verified as f64 / total as f64) * 100.0;
    let decile = ((pct / 10.0).round() as i64 * 10).clamp(0, 100);
    (true, format!("w{decile}"))
}

/// Gold/silver/bronze for the top three, muted for the rest. Returned
/// as the CSS class suffix the template appends to `.r`.
fn medal_class(rank: usize) -> String {
    match rank {
        1 => "1".to_string(),
        2 => "2".to_string(),
        3 => "3".to_string(),
        _ => "x".to_string(),
    }
}

/// Leaderboard rows shown on a claim card: 5 normally, 3 (the podium)
/// when the hero stat row renders, and 2 (the duel) when the
/// struck-through refutation row renders as well. At 1200x630 the hero
/// row plus a 2-line title (`.og-title-hero` in `og-card.css`) leave
/// roughly 280px before the footer; measured live, 4 rows push the
/// footer off-canvas, and the refutation row costs another row's worth
/// of budget.
fn claim_row_cap(hero_value: &str, refutation_claimed: &str) -> usize {
    match (hero_value.is_empty(), refutation_claimed.is_empty()) {
        (true, true) => 5,
        (false, true) => 3,
        _ => 2,
    }
}

/// Claim/verify/refute counts for a battle's footer + health bar. Counts
/// surviving (non-deleted) claims and the verifications/refutations
/// recorded against them. The refute SUM is a sibling of the verify SUM
/// over the same rows — it feeds only the arcade health bar's red side
/// (no change to the leaderboard/wins-matrix logic). Soft-failures
/// degrade each count to `0` so the card still renders.
async fn battle_counts(state: &AppState, battle_id: Uuid) -> (i64, i64, i64) {
    let claim_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM claims WHERE battle_id = $1 AND deleted_at IS NULL",
    )
    .bind(battle_id)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let verify_count: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(verify_count), 0)::bigint FROM claims \
         WHERE battle_id = $1 AND deleted_at IS NULL",
    )
    .bind(battle_id)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    let refute_count: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(refute_count), 0)::bigint FROM claims \
         WHERE battle_id = $1 AND deleted_at IS NULL",
    )
    .bind(battle_id)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    (claim_count, verify_count, refute_count)
}

/// Find the `# Title` line at the top of a claim body, falling back to
/// the first non-empty line. Same extraction `routes::og` uses for the
/// PNG card title.
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

/// Wrap a rendered askama result in an `Html` 200, or a 500 with a
/// minimal body on render failure (should never happen — the template
/// is compile-checked).
fn render_template(rendered: Result<String, askama::Error>) -> Response {
    match rendered {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "og_html: template render failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<!doctype html><title>render error</title>"),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medal_class_maps_top_three_then_muted() {
        assert_eq!(medal_class(1), "1");
        assert_eq!(medal_class(2), "2");
        assert_eq!(medal_class(3), "3");
        assert_eq!(medal_class(4), "x");
        assert_eq!(medal_class(99), "x");
    }

    #[test]
    fn claim_row_cap_drops_to_podium_when_hero_present() {
        assert_eq!(claim_row_cap("", ""), 5);
        assert_eq!(claim_row_cap("2.4x", ""), 3);
        assert_eq!(claim_row_cap("1.5x", "2x"), 2);
    }

    #[test]
    fn wins_column_uses_shared_metric_formatter() {
        // Score formatting for the "wins" column now goes through the
        // shared `format_metric_compact` — dimensionless values keep
        // the old drop-trailing-zeros behaviour, and a raw seconds
        // value scales/labels like every other presentation surface.
        assert_eq!(format_metric_compact(42.0, None, None), "42");
        assert_eq!(format_metric_compact(0.875, None, None), "0.875");
        assert_eq!(format_metric_compact(0.102, Some("s"), None), "102 ms");
    }

    #[test]
    fn extract_title_handles_markdown_heading() {
        let body = "# Best Web API\n\n## Summary\n\nBody.";
        assert_eq!(extract_title_from_claim(body), "Best Web API");
    }

    #[test]
    fn battle_template_renders_rows_and_footer() {
        let tmpl = BattleOgTemplate {
            title: "What is the best Web Access API?".into(),
            rows: vec![
                OgRow {
                    rank: 1,
                    medal: "1".into(),
                    name: "Keenable".into(),
                    logo: Some("https://example.com/k.png".into()),
                    metric: Some("3 runs".into()),
                    wins: "2".into(),
                },
                OgRow {
                    rank: 2,
                    medal: "2".into(),
                    name: "Exa".into(),
                    logo: None,
                    metric: Some("2 runs".into()),
                    wins: "1".into(),
                },
            ],
            tags: "ai · search".into(),
            stats: "5 claims · 3 verified".into(),
            verified: "3".into(),
            refuted: "1".into(),
            has_activity: true,
            health_class: "w80".into(),
        };
        let html = tmpl.render().expect("battle template renders");
        assert!(html.contains("What is the best Web Access API?"));
        assert!(html.contains("Keenable"));
        assert!(html.contains("Exa"));
        assert!(html.contains("https://example.com/k.png"));
        assert!(html.contains("5 claims · 3 verified"));
        assert!(html.contains("BATTLE"));
        // CSS owns the self-hosted font declarations, not Google Fonts.
        assert!(html.contains(r#"<link rel="stylesheet" href="/static/og-card.css">"#));
        assert!(!html.contains("fonts.googleapis.com"));
    }

    #[test]
    fn battle_template_handles_empty_leaderboard() {
        let tmpl = BattleOgTemplate {
            title: "Fresh Battle".into(),
            rows: vec![],
            tags: String::new(),
            stats: "0 claims · 0 verified".into(),
            verified: "0".into(),
            refuted: "0".into(),
            has_activity: false,
            health_class: "w0".into(),
        };
        let html = tmpl.render().expect("empty battle template renders");
        assert!(html.contains("No claims yet"));
    }

    #[test]
    fn claim_template_renders_score_column() {
        let tmpl = ClaimOgTemplate {
            title: "Exa wins on recall".into(),
            rows: vec![OgRow {
                rank: 1,
                medal: "1".into(),
                name: "Exa".into(),
                logo: None,
                metric: None,
                wins: "0.88".into(),
            }],
            tags: "Winner: Exa".into(),
            hero_value: String::new(),
            hero_label: String::new(),
            pill_label: "CLAIM".into(),
            pill_refuted: false,
            refutation_claimed: String::new(),
            refutation_measured: String::new(),
            verified: "4".into(),
            refuted: "0".into(),
        };
        let html = tmpl.render().expect("claim template renders");
        assert!(html.contains("Exa wins on recall"));
        assert!(html.contains("0.88"));
        // The redundant "Winner: …" line is suppressed when a leaderboard is
        // present — rank 1 already names the winner, and dropping it keeps the
        // fixed-height card from overflowing onto the footer.
        assert!(
            !html.contains("Winner: Exa"),
            "winner line must be hidden when the leaderboard is shown"
        );
        assert!(html.contains("CLAIM"));
        // Verified badge carries the count; the refuted badge class is
        // absent on a non-refuted card.
        assert!(html.contains("VERIFIED"));
        assert!(
            !html.contains("og-badge-refuted"),
            "non-refuted card must not carry the red refuted badge class"
        );
        // Watermark renders regardless of hero/refutation state.
        assert!(html.contains("og-wm"));
        assert!(html.contains("krabarena.com"));
        // No hero row -> title keeps the legacy (un-clamped-to-2) class.
        assert!(!html.contains("og-title-hero"));
    }

    #[test]
    fn claim_template_renders_hero_row_when_present() {
        let tmpl = ClaimOgTemplate {
            title: "Exa is 2.5x faster than Brave".into(),
            rows: vec![],
            tags: String::new(),
            hero_value: "2.5x".into(),
            hero_label: "speedup".into(),
            pill_label: "CLAIM".into(),
            pill_refuted: false,
            refutation_claimed: String::new(),
            refutation_measured: String::new(),
            verified: "4".into(),
            refuted: "0".into(),
        };
        let html = tmpl.render().expect("hero template renders");
        assert!(html.contains("2.5x"));
        assert!(html.contains("speedup"));
        assert!(!html.contains("og-refutation"));
        // Hero row present -> title clamps to 2 lines instead of 3 so a
        // long title truncates cleanly instead of overlapping the hero.
        assert!(html.contains("og-title-hero"));
    }

    #[test]
    fn claim_template_renders_refuted_pill_and_struck_through_values() {
        let tmpl = ClaimOgTemplate {
            title: "Nobody said it was 6.4x faster".into(),
            rows: vec![],
            tags: String::new(),
            hero_value: String::new(),
            hero_label: String::new(),
            pill_label: "HYPOTHESIS REFUTED".into(),
            pill_refuted: true,
            refutation_claimed: "6.4×".into(),
            refutation_measured: "1.09×".into(),
            verified: "0".into(),
            refuted: "1".into(),
        };
        let html = tmpl.render().expect("refuted template renders");
        assert!(html.contains("HYPOTHESIS REFUTED"));
        assert!(
            html.contains("og-badge-refuted"),
            "refuted card must carry the red refuted badge class"
        );
        assert!(html.contains("<s>6.4×</s>"));
        assert!(html.contains("<b>1.09×</b>"));
    }

    #[test]
    fn health_fill_buckets_verified_share() {
        // No activity -> hidden bar, w0.
        assert_eq!(health_fill(0, 0), (false, "w0".to_string()));
        // All verified -> full green bar.
        assert_eq!(health_fill(5, 0), (true, "w100".to_string()));
        // All refuted -> empty green (all red).
        assert_eq!(health_fill(0, 3), (true, "w0".to_string()));
        // ~82% verified rounds to the 80% bucket.
        assert_eq!(health_fill(23, 5), (true, "w80".to_string()));
    }
}
