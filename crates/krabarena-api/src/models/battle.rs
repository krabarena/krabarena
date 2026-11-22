use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Explicit column list for `SELECT … FROM battles` reads. Kept in
/// sync with the [`BattleRow`] field order below. **When you add a
/// field to `BattleRow`, add it here too.** See the matching note
/// on `models::claim::CLAIM_COLUMNS` for the PgBouncer-cached-plan
/// motivation.
pub const BATTLE_COLUMNS: &str = "\
id, author_id, parent_id, title, description, domain_tags, status, \
created_at, updated_at, sponsored_by, executive_summary, proxy_spec, \
slug, og_summary, short_summary, temperature, cover_image_url, \
cover_image_alt, solutions, deleted_at, presentation_guidance, \
presentation_config, produced_by";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BattleRow {
    pub id: Uuid,
    pub author_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub domain_tags: Vec<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Primary sponsor entity (migration 008). Optional — many Battles have none.
    #[sqlx(default)]
    pub sponsored_by: Option<Uuid>,
    /// Auto-generated executive summary updated by KrabReviewer after each Claim.
    #[sqlx(default)]
    #[allow(dead_code)]
    pub executive_summary: Option<String>,
    /// Proxy configuration (JSONB) — API keys, budgets, services. Never exposed to claimers.
    #[sqlx(default)]
    #[allow(dead_code)]
    pub proxy_spec: Option<serde_json::Value>,
    #[sqlx(default)]
    #[allow(dead_code)]
    pub slug: Option<String>,
    /// Short (~200 char) social-card summary written by KrabReviewer.
    /// Used for `og:description` / `twitter:description`. Falls back to
    /// the longer `executive_summary` or the raw `description` when null.
    #[sqlx(default)]
    pub og_summary: Option<String>,
    /// Compact 3-section card summary (headline + Top 3 table + Top
    /// Metrics + Notes-as-emojis) written by KrabReviewer alongside
    /// `executive_summary` since 0.8.21. Rendered on the home/list
    /// page Battle cards. Also used as OG description fallback —
    /// when present, the headline opener is a better social-card
    /// preview than the full editorial.
    #[sqlx(default)]
    pub short_summary: Option<String>,
    /// Cached temperature score, refreshed each time the KrabReviewer
    /// updates the executive summary. The live battle detail page
    /// re-derives it from events too — this column is for fast list
    /// rendering without an N×events JOIN.
    #[sqlx(default)]
    pub temperature: f32,
    /// Optional editor-uploaded cover image for the Battle page hero.
    /// Direct GCS URL (migration 032). `None` means the hero renders
    /// without an image (the historical default).
    #[sqlx(default)]
    pub cover_image_url: Option<String>,
    /// Alt text for `cover_image_url` — either Gemini-generated or
    /// derived from the upload filename. Always present alongside the
    /// URL so the `<img>` tag has accessible text.
    #[sqlx(default)]
    pub cover_image_alt: Option<String>,
    /// Battle-owned competitor / product list (migration 038, 0.13.0).
    /// The canonical solution→logo map for every Claim attached to
    /// this Battle. Render path normalises each leaderboard row's
    /// `solution` string the same way as `name_normalized` and looks
    /// up the entry here. Standalone Claims (no parent Battle) still
    /// fall back to `structure.json.leaderboard[i].logo_url` —
    /// that field stays for the rare standalone case.
    ///
    /// Stored as raw JSONB so the column is forward-compatible with
    /// extra per-solution fields (variants, deprecated, …) without
    /// a schema bump. Parsed into [`BattleSolution`] at use sites
    /// via [`parse_solutions`].
    #[sqlx(default)]
    pub solutions: serde_json::Value,
    /// Admin soft-delete tombstone (migration 053). `None` = live;
    /// `Some(ts)` = soft-removed by an admin and hidden from every
    /// public read path. Reversible via the restore endpoint.
    #[sqlx(default)]
    pub deleted_at: Option<DateTime<Utc>>,
    /// Freeform per-battle presentation guidance (migration 056, 0.28.x).
    /// Natural-language steering fed to the summary / quick-summary
    /// agents. `None` = use the house-default guidance.
    #[sqlx(default)]
    pub presentation_guidance: Option<String>,
    /// Structured per-battle presentation knobs (migration 056, 0.28.x).
    /// Deserialized into `krabarena_shared::presentation::PresentationConfig`
    /// via `PresentationConfig::from_json`; a partial / absent blob
    /// falls back to the house defaults.
    #[sqlx(default)]
    pub presentation_config: Option<serde_json::Value>,
    /// Producer label slugs (migration 060). `None` / empty = no
    /// "produced by" label. Each slug (e.g. `fermatix`) resolves via
    /// [`crate::models::producer`] to the brand name / description /
    /// website / logo rendered as the interactive label in the card's
    /// top-right corner. An array so a battle can credit several partners.
    #[sqlx(default)]
    pub produced_by: Option<Vec<String>>,
}

/// One entry in `battles.solutions`. Created at backfill time or by
/// editors via the battle edit form. `name_normalized` is the join
/// key against a leaderboard row's normalised `solution` string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BattleSolution {
    /// Human display name (what the editor typed). Preserved
    /// verbatim so the editor's casing wins.
    pub name: String,
    /// Lookup key: lowercase + trim + collapse-whitespace +
    /// strip-parens-content. Mirrors
    /// `services::solution_names::normalize_name`.
    pub name_normalized: String,
    /// Vendor logo URL (recommended: Google S2 favicon proxy at
    /// `https://www.google.com/s2/favicons?domain=<vendor>&sz=64`
    /// — stable URL, no GCS rehosting needed, free).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Vendor / product homepage. Optional; rendered as a link on
    /// the solution name when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    /// Dominant brand color (hex `#rrggbb`) extracted from the stored
    /// logo (migration 057 / `logo_assets`). Set by the battle-solutions
    /// backfill from the matched `vendor_logos.brand_color`. Used to
    /// color this solution's slice in the Claims Wins pie; falls back to
    /// the hash-derived avatar color when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand_color: Option<String>,
}

/// Decode `battles.solutions` JSONB into a typed Vec. Tolerant of
/// malformed entries: anything that fails to parse returns an empty
/// Vec rather than 500-ing the page. The column has `DEFAULT '[]'`
/// so the empty Vec is also the natural "no solutions registered"
/// signal.
pub fn parse_solutions(raw: &serde_json::Value) -> Vec<BattleSolution> {
    if raw.is_null() {
        return Vec::new();
    }
    serde_json::from_value::<Vec<BattleSolution>>(raw.clone()).unwrap_or_default()
}

/// Maximum slug length in characters (ASCII alphanumerics + dashes).
/// 80 is the upper bound we've stored historically; raising it would
/// break URL compatibility with the existing battles/claims tables.
const MAX_SLUG_LEN: usize = 80;

/// Generate a URL slug from a title.
///
/// Word-boundary aware: builds the slug word-by-word and stops when
/// adding the next word would exceed [`MAX_SLUG_LEN`]. Avoids cuts
/// like `…-the-ai-e` (truncated `era`) that the previous fixed-take
/// path produced. If the very first word is already longer than the
/// cap (typo-style mash, e.g. `aaaaaaaaaaaa…`), fall back to a
/// character-prefix slice so we always return something non-empty
/// for non-empty input.
pub fn slugify(title: &str) -> String {
    let normalized: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    let mut out = String::new();
    for word in normalized.split('-').filter(|s| !s.is_empty()) {
        if out.is_empty() {
            if word.len() > MAX_SLUG_LEN {
                // Pathological: a single token longer than the cap.
                // Take a prefix to keep returning a non-empty slug.
                // `is_ascii_alphanumeric` filtered above guarantees
                // ASCII bytes here, so byte slicing is safe.
                return word[..MAX_SLUG_LEN].to_string();
            }
            out.push_str(word);
        } else if out.len() + 1 + word.len() <= MAX_SLUG_LEN {
            out.push('-');
            out.push_str(word);
        } else {
            // Next word doesn't fit — stop and keep the slug clean.
            break;
        }
    }
    out
}

/// Ensure a slug is unique within `table` (either `claims` or `battles`,
/// both of which have a `UNIQUE (slug)` index). If the natural slug is
/// already taken, append a 6-char uuid suffix. Two separate posts with
/// the same title shouldn't 500 with `<table>_slug_key` — the collision
/// is expected (titles repeat) and the URL just needs to disambiguate.
///
/// Called pre-INSERT to avoid the INSERT itself raising the unique
/// violation as a sqlx::Error that wraps into ApiError::Internal (500).
pub async fn ensure_unique_slug(
    pool: &sqlx::PgPool,
    table: &'static str,
    natural: &str,
) -> Result<String, sqlx::Error> {
    let sql = match table {
        "claims" => "SELECT EXISTS(SELECT 1 FROM claims WHERE slug = $1)",
        "battles" => "SELECT EXISTS(SELECT 1 FROM battles WHERE slug = $1)",
        other => panic!("ensure_unique_slug: unsupported table {other}"),
    };
    let taken: bool = sqlx::query_scalar(sql)
        .bind(natural)
        .fetch_one(pool)
        .await?;
    if !taken {
        return Ok(natural.to_string());
    }
    // Short uuid tail as disambiguator. 6 chars ≈ 24 bits, collision-
    // free in practice for a per-title cohort. If the second post
    // racing somehow also collides, the INSERT's unique constraint
    // still catches it — this function is a best-effort preflight.
    let suffix: String = Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect();
    Ok(format!("{natural}-{suffix}"))
}

#[derive(Debug, Serialize)]
pub struct BattleResponse {
    pub id: Uuid,
    pub author_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub domain_tags: Vec<String>,
    pub status: String,
    pub claim_count: i64,
    pub total_confirmed: i64,
    pub total_refuted: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub slug: Option<String>,
    pub web_url: String,
    pub executive_summary: Option<String>,
    pub short_summary: Option<String>,
    pub og_summary: Option<String>,
    pub solutions: Vec<BattleSolution>,
    pub produced_by: Vec<String>,
    pub cover_image_url: Option<String>,
    pub cover_image_alt: Option<String>,
    pub sub_battle_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BattleTotals {
    pub claim_count: i64,
    pub total_confirmed: i64,
    pub total_refuted: i64,
}

impl BattleResponse {
    pub fn from_row(row: BattleRow, totals: BattleTotals, sub_battle_ids: Vec<Uuid>) -> Self {
        let solutions = parse_solutions(&row.solutions);
        let web_url = format!(
            "https://krabarena.com/battles/{}",
            row.slug
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| row.id.to_string())
        );
        Self {
            id: row.id,
            author_id: row.author_id,
            parent_id: row.parent_id,
            title: row.title,
            description: row.description,
            domain_tags: row.domain_tags,
            status: row.status,
            claim_count: totals.claim_count,
            total_confirmed: totals.total_confirmed,
            total_refuted: totals.total_refuted,
            created_at: row.created_at,
            updated_at: row.updated_at,
            slug: row.slug,
            web_url,
            executive_summary: row.executive_summary,
            short_summary: row.short_summary,
            og_summary: row.og_summary,
            solutions,
            produced_by: row.produced_by.unwrap_or_default(),
            cover_image_url: row.cover_image_url,
            cover_image_alt: row.cover_image_alt,
            sub_battle_ids,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BattleListItem {
    pub id: Uuid,
    pub slug: Option<String>,
    pub author_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub domain_tags: Vec<String>,
    pub status: String,
    pub claim_count: i64,
    pub total_confirmed: i64,
    pub total_refuted: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBattleRequest {
    pub title: String,
    pub description: String,
    pub domain_tags: Vec<String>,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    /// Optional proxy spec — set inline at creation time.
    #[serde(default)]
    pub proxy_spec: Option<serde_json::Value>,
    /// Optional producer slugs (e.g. `["fermatix"]`) tagging the battle as
    /// "produced by" registered brands. Admin-only; each slug is validated
    /// against [`crate::models::producer`] (unknown slugs rejected).
    #[serde(default)]
    pub produced_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBattleRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub domain_tags: Option<Vec<String>>,
    /// Battle status (`open`, `holding`, `resolved`, `archived`). Prior
    /// versions advertised `status` as editable on the CLI but silently
    /// dropped it server-side — now plumbed through.
    pub status: Option<String>,
    /// Proxy spec JSON (API keys, services, budgets). Editable post-
    /// creation so sponsors can rotate keys or adjust budgets without
    /// having to re-open the Battle.
    pub proxy_spec: Option<serde_json::Value>,
    /// Producer slugs (e.g. `["fermatix"]`). Admin-only. A non-empty array
    /// (slugs validated against [`crate::models::producer`]) tags the
    /// battle; an empty array clears the label. Absent = leave unchanged.
    #[serde(default)]
    pub produced_by: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ListBattlesQuery {
    pub status: Option<String>,
    pub domain: Option<String>,
    #[allow(dead_code)]
    pub sort: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[cfg(test)]
mod slugify_tests {
    use super::{MAX_SLUG_LEN, slugify};

    /// Regression: the actual prod title that triggered the bug
    /// report — "Python vs Typescript vs Rust vs Go: which language
    /// is best for coding in the AI era?". Pre-fix produced
    /// `…-the-ai-e` (truncated `era`); post-fix should stop at the
    /// last whole word that fits.
    #[test]
    fn cuts_at_word_boundary_not_mid_word() {
        let title =
            "Python vs Typescript vs Rust vs Go: which language is best for coding in the AI era?";
        let slug = slugify(title);
        assert!(
            slug.len() <= MAX_SLUG_LEN,
            "slug exceeded {MAX_SLUG_LEN} chars: {slug}"
        );
        // Most important: no half-word at the tail.
        assert!(
            !slug.ends_with("-e"),
            "slug cut mid-word, ends with `-e`: {slug}"
        );
        assert!(!slug.ends_with('-'), "slug ends with a dash: {slug}");
        // Every dash-separated token should be a whole word in the
        // input — no `e` (truncated `era`) or similar.
        let input_words: Vec<String> = title
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        for token in slug.split('-') {
            assert!(
                input_words.iter().any(|w| w == token),
                "slug token `{token}` is not a whole word from the input. slug={slug}"
            );
        }
    }

    /// Short title — no truncation needed, output is the obvious
    /// slugged form.
    #[test]
    fn short_title_no_truncation() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("Foo!! Bar??"), "foo-bar");
        assert_eq!(slugify("  spaces   "), "spaces");
    }

    /// Title exactly at the cap — keep the whole thing.
    #[test]
    fn title_at_exact_cap_is_kept_whole() {
        let core = "abcdefghij".repeat(8); // 80 chars, single token
        let slug = slugify(&core);
        assert_eq!(slug.len(), MAX_SLUG_LEN);
        assert_eq!(slug, core); // unchanged
    }

    /// Title one word past the cap — drop the overflowing word
    /// rather than truncate it.
    #[test]
    fn drops_overflowing_word_keeps_prior() {
        // The slug "alpha-beta-gamma-delta-epsilon-zeta-eta-theta-iota-kappa-lambda-mu-nu-xi-omicron" is 80 chars exactly.
        // Add "-pi" → 83. Slugify should stop after "omicron".
        let title =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi";
        let slug = slugify(title);
        assert!(slug.len() <= MAX_SLUG_LEN);
        assert!(slug.ends_with("omicron"), "{slug}");
        assert!(
            !slug.contains("-pi"),
            "should have dropped overflow: {slug}"
        );
    }

    /// Empty/all-symbol input — empty slug rather than a panic.
    #[test]
    fn empty_input_empty_slug() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("???!!!"), "");
        assert_eq!(slugify("   "), "");
    }

    /// Pathological: a single token longer than the cap. Falls back
    /// to a character-prefix slice (last-resort behavior).
    #[test]
    fn single_oversized_token_falls_back_to_prefix() {
        let title = "a".repeat(MAX_SLUG_LEN + 50);
        let slug = slugify(&title);
        assert_eq!(slug.len(), MAX_SLUG_LEN);
        assert!(slug.chars().all(|c| c == 'a'));
    }
}
