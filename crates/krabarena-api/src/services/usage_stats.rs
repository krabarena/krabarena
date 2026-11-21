//! Usage stats for `GET /v1/admin/usage` / `krab admin usage` — the data
//! source for the daily digest. Reports, per UTC window:
//!
//! - **crawlers** — `user_logs` rows (HTML GETs) whose User-Agent is a
//!   bot, bucketed by [`ua_classifier`] category and family, with page
//!   coverage (distinct live Battles / Claims reached).
//! - **humans** — `user_logs` visits from real-browser UAs (devices,
//!   engaged devices, new devices, logins, votes, top pages/referrers).
//! - **cli** — `cli_telemetry` `cli_command` records (users, commands,
//!   versions). Only CLIs with telemetry on are counted.
//! - **content** — Battles / Claims / comments / verifications / signups
//!   created in the window.
//!
//! Plus `previous`: the same headline numbers for the prior window of
//! equal length, as deltas.
//!
//! Classification happens in Rust ([`ua_classifier::classify_ua`]) over
//! the distinct UAs of the window; the SQL only receives the resulting
//! `ua → (family, category)` mapping as bound arrays. One source of
//! truth for the patterns.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::services::ua_classifier::{UaCategory, classify_ua};

/// Max window length (`?days=`). `user_logs` keeps data since
/// 2026-07-13; Cloud Run logs only 30 days.
pub const MAX_DAYS: i64 = 90;
/// How many families / pages / referrers / commands / versions to list.
pub const TOP_N: usize = 10;
/// `by_family` keeps this many families; the tail folds into `(other)`.
pub const MAX_FAMILIES: usize = 40;

/// Path patterns that are never a human page view (vuln probes, static
/// files, robots/sitemaps). Applied to human visits only.
const NON_PAGE_PATH_RE: &str = r"(^/\.|/wp-|xmlrpc|phpmyadmin|/cgi-bin|\.(php|asp|aspx|jsp|cgi|env|ico|txt|xml|png|jpe?g|gif|svg|webp|js|css|map|json|ya?ml|sql|bak|zip|gz|tar|ini|log)$)";

/// Referer hosts that are us (self-navigation), excluded from top referrers.
/// Bare-IP referers are our own load balancer (34.49.91.89 = krabarena.com)
/// or scanners — never a real referring site.
const OWN_HOST_RE: &str = r"(^|\.)krabarena\.(com|org)$|\.run\.app$|^localhost$|^[0-9.]+$";

// ---------------------------------------------------------------------------
// Window math
// ---------------------------------------------------------------------------

/// A half-open UTC window `[start, end)` made of whole days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageWindow {
    /// First day (inclusive).
    pub first_day: NaiveDate,
    /// Last day (inclusive).
    pub last_day: NaiveDate,
    pub days: i64,
}

impl UsageWindow {
    /// Resolve `?date` / `?days` against `today` (UTC).
    ///
    /// - `date` = the LAST day of the window (inclusive); default
    ///   yesterday, so the default report is the last complete day.
    /// - `days` = window length, 1..=90, default 1.
    ///
    /// `date` may not be in the future; `date == today` is allowed and
    /// flagged `partial`.
    pub fn resolve(
        date: Option<NaiveDate>,
        days: Option<i64>,
        today: NaiveDate,
    ) -> Result<Self, String> {
        let days = days.unwrap_or(1);
        if !(1..=MAX_DAYS).contains(&days) {
            return Err(format!("days must be between 1 and {MAX_DAYS}"));
        }
        let last_day = date.unwrap_or(today - Duration::days(1));
        if last_day > today {
            return Err(format!(
                "date {last_day} is in the future (today is {today} UTC)"
            ));
        }
        Ok(Self {
            first_day: last_day - Duration::days(days - 1),
            last_day,
            days,
        })
    }

    pub fn start(&self) -> DateTime<Utc> {
        self.first_day.and_hms_opt(0, 0, 0).unwrap().and_utc()
    }

    /// Exclusive end: midnight after `last_day`.
    pub fn end(&self) -> DateTime<Utc> {
        (self.last_day + Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
    }

    /// The prior window of equal length, ending the day before `first_day`.
    pub fn previous(&self) -> Self {
        let last_day = self.first_day - Duration::days(1);
        Self {
            first_day: last_day - Duration::days(self.days - 1),
            last_day,
            days: self.days,
        }
    }

    pub fn info(&self, now: DateTime<Utc>) -> WindowInfo {
        WindowInfo {
            start: self.start(),
            end: self.end(),
            first_day: self.first_day,
            last_day: self.last_day,
            days: self.days,
            partial: self.end() > now,
        }
    }
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct WindowInfo {
    /// Inclusive start (UTC midnight).
    pub start: DateTime<Utc>,
    /// Exclusive end (UTC midnight after `last_day`).
    pub end: DateTime<Utc>,
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub days: i64,
    /// True when the window includes today (still accumulating).
    pub partial: bool,
}

// ---------------------------------------------------------------------------
// Deltas
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Delta {
    pub current: i64,
    pub previous: i64,
    /// `current - previous`.
    pub change: i64,
    /// Percent change vs `previous`, 1 decimal; `null` when `previous == 0`.
    pub pct: Option<f64>,
}

impl Delta {
    pub fn new(current: i64, previous: i64) -> Self {
        let pct = if previous == 0 {
            None
        } else {
            let raw = (current - previous) as f64 * 100.0 / previous as f64;
            Some((raw * 10.0).round() / 10.0)
        };
        Self {
            current,
            previous,
            change: current - previous,
            pct,
        }
    }

    /// `+12%`, `-3.5%`, `new` (previous 0, current > 0) or `±0`.
    pub fn short(&self) -> String {
        match self.pct {
            Some(0.0) => "±0%".to_string(),
            Some(p) => {
                let s = if p.fract() == 0.0 {
                    format!("{p:.0}")
                } else {
                    format!("{p:.1}")
                };
                if p > 0.0 {
                    format!("+{s}%")
                } else {
                    format!("{s}%")
                }
            }
            None if self.current > 0 => "new".to_string(),
            None => "±0".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Report shape (stable)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct UsageReport {
    pub window: WindowInfo,
    pub generated_at: DateTime<Utc>,
    /// Server version that produced the report (CLI versions compare to it).
    pub server_version: String,
    pub crawlers: CrawlerStats,
    pub humans: HumanStats,
    pub cli: CliStats,
    pub content: ContentStats,
    pub previous: PreviousWindow,
    /// Caveats a digest should carry (UA-claimed classification, …).
    pub notes: Vec<String>,
    /// Compact human-readable rendering of this report (what
    /// `krab admin usage` prints in human mode).
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct CrawlerStats {
    /// All bot hits (every category except `browser`).
    pub total: i64,
    /// Live (non-deleted) Battles right now — the coverage denominator.
    pub live_battles: i64,
    /// Live Claims right now (not deleted, not private, latest version).
    pub live_claims: i64,
    pub by_category: Vec<CategoryStat>,
    pub by_family: Vec<FamilyStat>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CategoryStat {
    /// Machine key, e.g. `ai_search_api`.
    pub category: UaCategory,
    pub label: String,
    pub hits: i64,
    /// Distinct families seen in this category.
    pub families: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct FamilyStat {
    /// UA-claimed family, e.g. `KeenableBot`, `Googlebot Smartphone`.
    pub family: String,
    pub category: UaCategory,
    pub hits: i64,
    /// Hits on `/battles/*` pages.
    pub battle_pages: i64,
    /// Hits on `/claims/*` pages.
    pub claim_pages: i64,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct Coverage {
    /// Distinct live Battles this family fetched in the window.
    pub battles: i64,
    /// Distinct live Claims this family fetched in the window.
    pub claims: i64,
    /// `battles / live_battles * 100`, 1 decimal.
    pub battles_pct: f64,
    /// `claims / live_claims * 100`, 1 decimal.
    pub claims_pct: f64,
}

#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct HumanStats {
    /// Page views from real-browser UAs (probe/static paths excluded).
    pub visits: i64,
    /// Distinct `ka_device` cookies among those visits.
    pub devices: i64,
    /// Devices with ≥2 visits or a login in the window — filters out
    /// one-shot scrapers wearing a browser UA.
    pub engaged_devices: i64,
    /// Devices whose first-ever `user_logs` row falls in the window.
    /// Cookie-less scrapers make nearly every scraper hit "new"; prefer
    /// `new_engaged_devices` for "new humans".
    pub new_devices: i64,
    /// New devices that are also engaged — the best "new visitors" signal.
    pub new_engaged_devices: i64,
    /// Distinct logged-in voters (OAuth readers) with any activity.
    pub voters_active: i64,
    /// Distinct logged-in users (agents' owners) with web activity.
    pub users_active: i64,
    pub logins: i64,
    pub votes: i64,
    pub top_pages: Vec<PathCount>,
    pub top_referrers: Vec<ReferrerCount>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PathCount {
    pub path: String,
    pub visits: i64,
    pub devices: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ReferrerCount {
    /// Referer host (own hosts excluded).
    pub host: String,
    pub visits: i64,
    pub devices: i64,
}

#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct CliStats {
    /// Distinct users with ≥1 `krab` invocation (telemetry on).
    pub users: i64,
    /// Of `users`, accounts with `kind = 'bot'` (e.g. KrabAgent).
    pub bot_users: i64,
    /// Users whose first-ever invocation is in the window.
    pub new_users: i64,
    pub invocations: i64,
    /// Invocations with a non-zero exit code.
    pub failed: i64,
    pub top_commands: Vec<CommandCount>,
    pub versions: Vec<VersionCount>,
    /// Users whose latest-seen version equals `server_version`.
    pub users_on_latest: i64,
    /// Invoking harness (`claude-code`, `unknown` = plain terminal, …).
    pub agents: Vec<AgentCount>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CommandCount {
    /// Resolved via the CLI manifest, e.g. `claim post`, `admin usage`.
    pub command: String,
    pub invocations: i64,
    pub users: i64,
    pub failed: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct VersionCount {
    pub version: String,
    pub users: i64,
    pub invocations: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AgentCount {
    pub agent: String,
    pub users: i64,
    pub invocations: i64,
}

#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct ContentStats {
    /// New Battles (not deleted).
    pub battles: i64,
    /// New Claims — first versions only, not deleted.
    pub claims: i64,
    /// New versions of existing Claims (edits).
    pub claim_edits: i64,
    pub comments: i64,
    pub verifications: i64,
    /// Proposed Battles (`battle_candidates`).
    pub battle_proposals: i64,
    pub signups: Signups,
}

#[derive(Debug, Clone, Default, Serialize, utoipa::ToSchema)]
pub struct Signups {
    /// New `users` rows with `kind = 'human'` (agent owners / CLI users).
    pub users: i64,
    /// New `voters` (OAuth readers).
    pub voters: i64,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PreviousWindow {
    pub window: WindowInfo,
    /// `metric → delta`. Keys: `crawlers.total`, `crawlers.<category>`,
    /// `humans.<field>`, `cli.<field>`, `content.<field>`,
    /// `content.signups.users|voters`.
    pub deltas: BTreeMap<String, Delta>,
}

// ---------------------------------------------------------------------------
// Headline metrics (shared by current/previous → deltas)
// ---------------------------------------------------------------------------

/// Flatten the scalar headline metrics of one window into `key → value`.
pub fn headline_metrics(
    crawlers: &CrawlerStats,
    humans: &HumanStats,
    cli: &CliStats,
    content: &ContentStats,
) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    m.insert("crawlers.total".into(), crawlers.total);
    for c in UaCategory::ALL.into_iter().filter(|c| c.is_bot()) {
        let hits = crawlers
            .by_category
            .iter()
            .find(|s| s.category == c)
            .map(|s| s.hits)
            .unwrap_or(0);
        m.insert(format!("crawlers.{}", c.key()), hits);
    }
    for (k, v) in [
        ("humans.visits", humans.visits),
        ("humans.devices", humans.devices),
        ("humans.engaged_devices", humans.engaged_devices),
        ("humans.new_devices", humans.new_devices),
        ("humans.new_engaged_devices", humans.new_engaged_devices),
        ("humans.voters_active", humans.voters_active),
        ("humans.users_active", humans.users_active),
        ("humans.logins", humans.logins),
        ("humans.votes", humans.votes),
        ("cli.users", cli.users),
        ("cli.new_users", cli.new_users),
        ("cli.invocations", cli.invocations),
        ("cli.failed", cli.failed),
        ("content.battles", content.battles),
        ("content.claims", content.claims),
        ("content.claim_edits", content.claim_edits),
        ("content.comments", content.comments),
        ("content.verifications", content.verifications),
        ("content.battle_proposals", content.battle_proposals),
        ("content.signups.users", content.signups.users),
        ("content.signups.voters", content.signups.voters),
    ] {
        m.insert(k.into(), v);
    }
    m
}

pub fn compute_deltas(
    current: &BTreeMap<String, i64>,
    previous: &BTreeMap<String, i64>,
) -> BTreeMap<String, Delta> {
    current
        .iter()
        .map(|(k, &v)| {
            (
                k.clone(),
                Delta::new(v, previous.get(k).copied().unwrap_or(0)),
            )
        })
        .collect()
}

fn pct1(num: i64, den: i64) -> f64 {
    if den <= 0 {
        0.0
    } else {
        ((num as f64 * 1000.0) / den as f64).round() / 10.0
    }
}

// ---------------------------------------------------------------------------
// CLI command resolution (via the CLI manifest)
// ---------------------------------------------------------------------------

/// Resolve a recorded invocation to its command path using the CLI
/// manifest: `["claim","get","<slug>","--format","json"]` with
/// `command = "claim"` → `claim get`. Tokens after the top-level
/// command are walked through `subcommands` while they match; the
/// first positional/flag stops the walk. Commands not in the manifest
/// (hardcoded builtins like `auth`, `doctor`) take one extra
/// word-looking token (`auth status`).
pub fn resolve_command(
    manifest: &krabarena_shared::manifest::CliManifest,
    command: &str,
    args: &[String],
) -> String {
    let rest: &[String] = match args.iter().position(|a| a == command) {
        Some(i) => &args[i + 1..],
        None => &[],
    };
    let mut path = vec![command.to_string()];
    match manifest.commands.iter().find(|c| c.name == command) {
        Some(mut def) => {
            for tok in rest.iter().take(3) {
                match def.subcommands.iter().find(|s| &s.name == tok) {
                    Some(sub) => {
                        path.push(tok.clone());
                        def = sub;
                    }
                    None => break,
                }
            }
        }
        None => {
            if let Some(tok) = rest.first()
                && is_word(tok)
            {
                path.push(tok.clone());
            }
        }
    }
    path.join(" ")
}

fn is_word(s: &str) -> bool {
    (2..=24).contains(&s.len())
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && s.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}

/// Order semver-ish strings numerically (`0.43.10` > `0.43.9`); unparsable last.
fn version_key(v: &str) -> (u8, Vec<u64>) {
    let parts: Option<Vec<u64>> = v
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse::<u64>().ok())
        .collect();
    match parts {
        Some(p) if !p.is_empty() => (1, p),
        _ => (0, vec![]),
    }
}

// ---------------------------------------------------------------------------
// DB
// ---------------------------------------------------------------------------

/// UA classification for one window: every distinct UA → class.
struct UaMap {
    hits_by_ua: Vec<(String, i64)>,
}

async fn load_ua_hits(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<UaMap, sqlx::Error> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT COALESCE(user_agent, '') AS ua, count(*)::bigint AS hits
           FROM user_logs
          WHERE created_at >= $1 AND created_at < $2
            AND (path IS NULL OR path <> '/health')
          GROUP BY 1",
    )
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await?;
    Ok(UaMap { hits_by_ua: rows })
}

#[derive(sqlx::FromRow)]
struct FamilyRow {
    family: String,
    category: String,
    hits: i64,
    battle_pages: i64,
    claim_pages: i64,
    battles_reached: i64,
    claims_reached: i64,
}

const LIVE_CLAIMS_CTE: &str = "live_claims AS (
    SELECT c.id, c.slug FROM claims c
     WHERE c.deleted_at IS NULL AND c.visibility <> 'private'
       AND NOT EXISTS (SELECT 1 FROM claims s
                        WHERE s.predecessor_id = c.id AND s.deleted_at IS NULL)
)";

async fn crawler_stats(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    uas: &UaMap,
    detailed: bool,
) -> Result<CrawlerStats, sqlx::Error> {
    let mut bot_uas = Vec::new();
    let mut bot_fams = Vec::new();
    let mut bot_cats = Vec::new();
    for (ua, _) in &uas.hits_by_ua {
        let class = classify_ua(ua);
        if class.category.is_bot() {
            bot_uas.push(ua.clone());
            bot_fams.push(class.family);
            bot_cats.push(class.category.key().to_string());
        }
    }

    let rows: Vec<FamilyRow> = if !detailed {
        // Previous window: hits per family only, no page joins.
        let fam_of: HashMap<&str, (&str, &str)> = bot_uas
            .iter()
            .zip(bot_fams.iter().zip(bot_cats.iter()))
            .map(|(u, (f, c))| (u.as_str(), (f.as_str(), c.as_str())))
            .collect();
        let mut agg: HashMap<(String, String), i64> = HashMap::new();
        for (ua, hits) in &uas.hits_by_ua {
            if let Some((f, c)) = fam_of.get(ua.as_str()) {
                *agg.entry((f.to_string(), c.to_string())).or_default() += hits;
            }
        }
        agg.into_iter()
            .map(|((family, category), hits)| FamilyRow {
                family,
                category,
                hits,
                battle_pages: 0,
                claim_pages: 0,
                battles_reached: 0,
                claims_reached: 0,
            })
            .collect()
    } else if bot_uas.is_empty() {
        vec![]
    } else {
        let sql = format!(
            "WITH m AS (
                SELECT * FROM unnest($3::text[], $4::text[], $5::text[]) AS m(ua, family, category)
            ),
            lb AS (
                SELECT id, id::text AS k FROM battles WHERE deleted_at IS NULL
                UNION ALL
                SELECT id, slug FROM battles WHERE deleted_at IS NULL AND slug IS NOT NULL
            ),
            {LIVE_CLAIMS_CTE},
            lc AS (
                SELECT id, id::text AS k FROM live_claims
                UNION ALL
                SELECT id, slug FROM live_claims WHERE slug IS NOT NULL
            ),
            x AS (
                SELECT m.family, m.category,
                       substring(l.path from '^/battles/([^/?#]+)') AS bkey,
                       NULLIF(substring(l.path from '^/claims/([^/?#]+)'), 'standalone') AS ckey
                  FROM user_logs l
                  JOIN m ON m.ua = COALESCE(l.user_agent, '')
                 WHERE l.created_at >= $1 AND l.created_at < $2
                   AND (l.path IS NULL OR l.path <> '/health')
            )
            SELECT x.family, x.category,
                   count(*)::bigint AS hits,
                   count(x.bkey)::bigint AS battle_pages,
                   count(x.ckey)::bigint AS claim_pages,
                   count(DISTINCT lb.id)::bigint AS battles_reached,
                   count(DISTINCT lc.id)::bigint AS claims_reached
              FROM x
              LEFT JOIN lb ON lb.k = x.bkey
              LEFT JOIN lc ON lc.k = x.ckey
             GROUP BY 1, 2"
        );
        // Nested loops off: see `human_stats`.
        let mut tx = pool.begin().await?;
        sqlx::query("SET LOCAL enable_nestloop = off")
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query_as(&sql)
            .bind(start)
            .bind(end)
            .bind(&bot_uas)
            .bind(&bot_fams)
            .bind(&bot_cats)
            .fetch_all(&mut *tx)
            .await?;
        tx.rollback().await?;
        rows
    };

    let (live_battles, live_claims): (i64, i64) = if detailed {
        sqlx::query_as(&format!(
            "WITH {LIVE_CLAIMS_CTE}
             SELECT (SELECT count(*) FROM battles WHERE deleted_at IS NULL)::bigint,
                    (SELECT count(*) FROM live_claims)::bigint"
        ))
        .fetch_one(pool)
        .await?
    } else {
        (0, 0)
    };

    Ok(assemble_crawlers(rows, live_battles, live_claims))
}

fn assemble_crawlers(rows: Vec<FamilyRow>, live_battles: i64, live_claims: i64) -> CrawlerStats {
    let mut by_cat: BTreeMap<UaCategory, (i64, i64)> = BTreeMap::new();
    let mut fams: Vec<FamilyStat> = Vec::new();
    for r in rows {
        let category = UaCategory::from_key(&r.category).unwrap_or(UaCategory::OtherBot);
        let e = by_cat.entry(category).or_default();
        e.0 += r.hits;
        e.1 += 1;
        fams.push(FamilyStat {
            family: r.family,
            category,
            hits: r.hits,
            battle_pages: r.battle_pages,
            claim_pages: r.claim_pages,
            coverage: Coverage {
                battles: r.battles_reached,
                claims: r.claims_reached,
                battles_pct: pct1(r.battles_reached, live_battles),
                claims_pct: pct1(r.claims_reached, live_claims),
            },
        });
    }
    fams.sort_by(|a, b| b.hits.cmp(&a.hits).then_with(|| a.family.cmp(&b.family)));
    if fams.len() > MAX_FAMILIES {
        let tail = fams.split_off(MAX_FAMILIES - 1);
        let mut other = FamilyStat {
            family: "(other)".into(),
            category: UaCategory::OtherBot,
            hits: 0,
            battle_pages: 0,
            claim_pages: 0,
            coverage: Coverage::default(),
        };
        for f in tail {
            other.hits += f.hits;
            other.battle_pages += f.battle_pages;
            other.claim_pages += f.claim_pages;
            // Distinct coverage can't be summed across families; report
            // the best single family as a lower bound.
            other.coverage.battles = other.coverage.battles.max(f.coverage.battles);
            other.coverage.claims = other.coverage.claims.max(f.coverage.claims);
        }
        other.coverage.battles_pct = pct1(other.coverage.battles, live_battles);
        other.coverage.claims_pct = pct1(other.coverage.claims, live_claims);
        fams.push(other);
    }
    let mut by_category: Vec<CategoryStat> = by_cat
        .into_iter()
        .map(|(category, (hits, families))| CategoryStat {
            category,
            label: category.label().to_string(),
            hits,
            families,
        })
        .collect();
    by_category.sort_by(|a, b| b.hits.cmp(&a.hits).then(a.category.cmp(&b.category)));
    CrawlerStats {
        total: by_category.iter().map(|c| c.hits).sum(),
        live_battles,
        live_claims,
        by_category,
        by_family: fams,
    }
}

async fn human_stats(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    uas: &UaMap,
    detailed: bool,
) -> Result<HumanStats, sqlx::Error> {
    let browser_uas: Vec<String> = uas
        .hits_by_ua
        .iter()
        .filter(|(ua, _)| classify_ua(ua).category == UaCategory::Browser)
        .map(|(ua, _)| ua.clone())
        .collect();

    // Human page views. $3 = browser UAs (hash-joined, not `= ANY`,
    // which is a linear scan of the array per row), $4 = non-page path regex.
    let h_cte = "WITH b AS (SELECT DISTINCT unnest($3::text[]) AS ua),
    h AS (
        SELECT l.device_id, l.voter_id, l.user_id, l.path, l.referer
          FROM user_logs l
          JOIN b ON b.ua = l.user_agent
         WHERE l.created_at >= $1 AND l.created_at < $2
           AND l.event = 'visit'
           AND l.path IS NOT NULL AND l.path <> '/health'
           AND l.path !~* $4
    )";

    // Window scans join user_logs to a bound UA list. When the planner
    // under-estimates the window (stale stats) it picks a nested loop
    // over the UA list — 8 s instead of 0.4 s for one day on prod — so
    // these run with nested loops disabled in their own transaction.
    let mut tx = pool.begin().await?;
    sqlx::query("SET LOCAL enable_nestloop = off")
        .execute(&mut *tx)
        .await?;

    // Per-device aggregates; "new" is probed below with index lookups.
    let device_rows: Vec<(Uuid, i64, bool)> = sqlx::query_as(&format!(
        "{h_cte}
         SELECT device_id, count(*)::bigint,
                bool_or(voter_id IS NOT NULL OR user_id IS NOT NULL)
           FROM h GROUP BY device_id"
    ))
    .bind(start)
    .bind(end)
    .bind(&browser_uas)
    .bind(NON_PAGE_PATH_RE)
    .fetch_all(&mut *tx)
    .await?;
    let visits: i64 = device_rows.iter().map(|d| d.1).sum();
    let devices = device_rows.len() as i64;
    let device_ids: Vec<Uuid> = device_rows.iter().map(|d| d.0).collect();
    let engaged: Vec<bool> = device_rows.iter().map(|d| d.1 >= 2 || d.2).collect();
    let engaged_devices = engaged.iter().filter(|e| **e).count() as i64;

    let (pages, refs): (Vec<TopRow>, Vec<TopRow>) = if detailed {
        let pages = sqlx::query_as(&format!(
            "{h_cte}
             SELECT split_part(path, '?', 1) AS p, count(*)::bigint, count(DISTINCT device_id)::bigint
               FROM h GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT $5"
        ))
        .bind(start)
        .bind(end)
        .bind(&browser_uas)
        .bind(NON_PAGE_PATH_RE)
        .bind(TOP_N as i64)
        .fetch_all(&mut *tx)
        .await?;
        let refs = sqlx::query_as(&format!(
            "{h_cte},
             r AS (
                SELECT lower(substring(referer from '^[a-zA-Z][a-zA-Z0-9+.-]*://([^/:?#]+)')) AS host,
                       device_id
                  FROM h WHERE referer IS NOT NULL AND referer <> ''
             )
             SELECT host, count(*)::bigint, count(DISTINCT device_id)::bigint
               FROM r
              WHERE host IS NOT NULL AND host !~* $5
              GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT $6"
        ))
        .bind(start)
        .bind(end)
        .bind(&browser_uas)
        .bind(NON_PAGE_PATH_RE)
        .bind(OWN_HOST_RE)
        .bind(TOP_N as i64)
        .fetch_all(&mut *tx)
        .await?;
        (pages, refs)
    } else {
        (vec![], vec![])
    };
    tx.rollback().await?;

    // New = no user_logs row for the device before the window. One
    // index probe (device_id, created_at) per device — nested loops on.
    let (new_devices, new_engaged_devices): (i64, i64) = sqlx::query_as(
        "SELECT count(*)::bigint, (count(*) FILTER (WHERE d.engaged))::bigint
           FROM unnest($1::uuid[], $2::bool[]) AS d(device_id, engaged)
          WHERE NOT EXISTS (SELECT 1 FROM user_logs p
                             WHERE p.device_id = d.device_id AND p.created_at < $3)",
    )
    .bind(&device_ids)
    .bind(&engaged)
    .bind(start)
    .fetch_one(pool)
    .await?;

    // Account activity regardless of UA (a logged-in session is human).
    let (voters_active, users_active, logins, votes): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(DISTINCT voter_id)::bigint,
                count(DISTINCT user_id)::bigint,
                (count(*) FILTER (WHERE event = 'login'))::bigint,
                (count(*) FILTER (WHERE event = 'vote'))::bigint
           FROM user_logs
          WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await?;

    let mut out = HumanStats {
        visits,
        devices,
        engaged_devices,
        new_devices,
        new_engaged_devices,
        voters_active,
        users_active,
        logins,
        votes,
        top_pages: vec![],
        top_referrers: vec![],
    };
    out.top_pages = pages
        .into_iter()
        .map(|(path, visits, devices)| PathCount {
            path,
            visits,
            devices,
        })
        .collect();
    out.top_referrers = refs
        .into_iter()
        .map(|(host, visits, devices)| ReferrerCount {
            host,
            visits,
            devices,
        })
        .collect();
    Ok(out)
}

/// `(key, visits, devices)` row of a top-N list.
type TopRow = (String, i64, i64);

#[derive(sqlx::FromRow)]
struct CliRow {
    user_id: Uuid,
    is_bot: bool,
    version: String,
    agent: String,
    cmd: String,
    args: Vec<String>,
    failed: bool,
    n: i64,
}

/// `cli_telemetry` records land on the NEXT invocation's flush, so they
/// are windowed by the client-side `ts`; `received_at` (indexed) is
/// bounded a day earlier than the window just to use the index.
async fn cli_stats(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    server_version: &str,
    detailed: bool,
) -> Result<CliStats, sqlx::Error> {
    let rows: Vec<CliRow> = sqlx::query_as(
        "SELECT t.user_id,
                COALESCE(u.kind = 'bot', false) AS is_bot,
                COALESCE(t.krab_version, '?') AS version,
                t.agent,
                COALESCE(t.raw_json->>'command', '<none>') AS cmd,
                ARRAY(SELECT jsonb_array_elements_text(
                         CASE WHEN jsonb_typeof(t.raw_json->'args') = 'array'
                              THEN t.raw_json->'args' ELSE '[]'::jsonb END)
                      LIMIT 6) AS args,
                COALESCE(t.raw_json->>'exit_code', '0') <> '0' AS failed,
                count(*)::bigint AS n
           FROM cli_telemetry t
           LEFT JOIN users u ON u.id = t.user_id
          WHERE t.kind = 'cli_command'
            AND t.received_at >= $1 - interval '1 day'
            AND t.ts >= $1 AND t.ts < $2
          GROUP BY 1, 2, 3, 4, 5, 6, 7",
    )
    .bind(start)
    .bind(end)
    .fetch_all(pool)
    .await?;

    let new_users: i64 = sqlx::query_scalar(
        // Distinct users first, then one probe per user: any record from
        // (or received) before the window ⇒ not new. (Probing per row
        // scanned a heavy user's whole history per record — 40 s on a
        // 30-day window.)
        "WITH u AS (
            SELECT DISTINCT t.user_id
              FROM cli_telemetry t
             WHERE t.kind = 'cli_command'
               AND t.received_at >= $1 - interval '1 day'
               AND t.ts >= $1 AND t.ts < $2
         )
         SELECT count(*)::bigint
           FROM u
          WHERE NOT EXISTS (SELECT 1 FROM cli_telemetry p
                             WHERE p.user_id = u.user_id
                               AND p.kind = 'cli_command'
                               AND (p.ts < $1 OR p.received_at < $1))",
    )
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await?;

    let manifest = krabarena_shared::manifest_builder::build_manifest();
    Ok(assemble_cli(
        rows,
        new_users,
        server_version,
        &manifest,
        detailed,
    ))
}

fn assemble_cli(
    rows: Vec<CliRow>,
    new_users: i64,
    server_version: &str,
    manifest: &krabarena_shared::manifest::CliManifest,
    detailed: bool,
) -> CliStats {
    let mut users: HashSet<Uuid> = HashSet::new();
    let mut bot_users: HashSet<Uuid> = HashSet::new();
    let mut invocations = 0i64;
    let mut failed = 0i64;
    // command → (invocations, users, failed)
    let mut cmds: HashMap<String, (i64, HashSet<Uuid>, i64)> = HashMap::new();
    let mut versions: HashMap<String, (HashSet<Uuid>, i64)> = HashMap::new();
    let mut agents: HashMap<String, (HashSet<Uuid>, i64)> = HashMap::new();
    // user → highest version seen (for users_on_latest)
    let mut latest: HashMap<Uuid, String> = HashMap::new();

    for r in rows {
        users.insert(r.user_id);
        if r.is_bot {
            bot_users.insert(r.user_id);
        }
        invocations += r.n;
        if r.failed {
            failed += r.n;
        }
        if detailed {
            let name = resolve_command(manifest, &r.cmd, &r.args);
            let e = cmds.entry(name).or_insert_with(|| (0, HashSet::new(), 0));
            e.0 += r.n;
            e.1.insert(r.user_id);
            if r.failed {
                e.2 += r.n;
            }
            let v = versions.entry(r.version.clone()).or_default();
            v.0.insert(r.user_id);
            v.1 += r.n;
            let a = agents.entry(r.agent.clone()).or_default();
            a.0.insert(r.user_id);
            a.1 += r.n;
        }
        latest
            .entry(r.user_id)
            .and_modify(|cur| {
                if version_key(&r.version) > version_key(cur) {
                    *cur = r.version.clone();
                }
            })
            .or_insert_with(|| r.version.clone());
    }

    let mut top_commands: Vec<CommandCount> = cmds
        .into_iter()
        .map(|(command, (inv, us, f))| CommandCount {
            command,
            invocations: inv,
            users: us.len() as i64,
            failed: f,
        })
        .collect();
    top_commands.sort_by(|a, b| {
        b.invocations
            .cmp(&a.invocations)
            .then(a.command.cmp(&b.command))
    });
    top_commands.truncate(TOP_N);

    let mut versions: Vec<VersionCount> = versions
        .into_iter()
        .map(|(version, (us, inv))| VersionCount {
            version,
            users: us.len() as i64,
            invocations: inv,
        })
        .collect();
    versions.sort_by_key(|v| std::cmp::Reverse(version_key(&v.version)));
    versions.truncate(TOP_N);

    let mut agents: Vec<AgentCount> = agents
        .into_iter()
        .map(|(agent, (us, inv))| AgentCount {
            agent,
            users: us.len() as i64,
            invocations: inv,
        })
        .collect();
    agents.sort_by(|a, b| {
        b.invocations
            .cmp(&a.invocations)
            .then(a.agent.cmp(&b.agent))
    });

    CliStats {
        users: users.len() as i64,
        bot_users: bot_users.len() as i64,
        new_users,
        invocations,
        failed,
        top_commands,
        versions,
        users_on_latest: latest
            .values()
            .filter(|v| v.as_str() == server_version)
            .count() as i64,
        agents,
    }
}

async fn content_stats(
    pool: &PgPool,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<ContentStats, sqlx::Error> {
    let row: (i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM battles
             WHERE created_at >= $1 AND created_at < $2 AND deleted_at IS NULL)::bigint,
           (SELECT count(*) FROM claims
             WHERE created_at >= $1 AND created_at < $2 AND deleted_at IS NULL
               AND predecessor_id IS NULL)::bigint,
           (SELECT count(*) FROM claims
             WHERE created_at >= $1 AND created_at < $2 AND deleted_at IS NULL
               AND predecessor_id IS NOT NULL)::bigint,
           (SELECT count(*) FROM comments WHERE created_at >= $1 AND created_at < $2)::bigint,
           (SELECT count(*) FROM verifications WHERE created_at >= $1 AND created_at < $2)::bigint,
           (SELECT count(*) FROM battle_candidates WHERE created_at >= $1 AND created_at < $2)::bigint,
           (SELECT count(*) FROM users
             WHERE created_at >= $1 AND created_at < $2 AND kind = 'human')::bigint,
           (SELECT count(*) FROM voters WHERE created_at >= $1 AND created_at < $2)::bigint",
    )
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await?;
    Ok(ContentStats {
        battles: row.0,
        claims: row.1,
        claim_edits: row.2,
        comments: row.3,
        verifications: row.4,
        battle_proposals: row.5,
        signups: Signups {
            users: row.6,
            voters: row.7,
        },
    })
}

struct WindowStats {
    crawlers: CrawlerStats,
    humans: HumanStats,
    cli: CliStats,
    content: ContentStats,
}

async fn window_stats(
    pool: &PgPool,
    w: UsageWindow,
    server_version: &str,
    detailed: bool,
) -> Result<WindowStats, sqlx::Error> {
    let (start, end) = (w.start(), w.end());
    let uas = load_ua_hits(pool, start, end).await?;
    let (crawlers, humans, cli, content) = tokio::try_join!(
        crawler_stats(pool, start, end, &uas, detailed),
        human_stats(pool, start, end, &uas, detailed),
        cli_stats(pool, start, end, server_version, detailed),
        content_stats(pool, start, end),
    )?;
    Ok(WindowStats {
        crawlers,
        humans,
        cli,
        content,
    })
}

/// Build the full report for `window` (+ the previous window's deltas).
pub async fn build_report(
    pool: &PgPool,
    window: UsageWindow,
    server_version: &str,
    now: DateTime<Utc>,
) -> Result<UsageReport, sqlx::Error> {
    let prev_window = window.previous();
    let (cur, prev) = tokio::try_join!(
        window_stats(pool, window, server_version, true),
        window_stats(pool, prev_window, server_version, false),
    )?;
    let deltas = compute_deltas(
        &headline_metrics(&cur.crawlers, &cur.humans, &cur.cli, &cur.content),
        &headline_metrics(&prev.crawlers, &prev.humans, &prev.cli, &prev.content),
    );
    let mut report = UsageReport {
        window: window.info(now),
        generated_at: now,
        server_version: server_version.to_string(),
        crawlers: cur.crawlers,
        humans: cur.humans,
        cli: cur.cli,
        content: cur.content,
        previous: PreviousWindow {
            window: prev_window.info(now),
            deltas,
        },
        notes: report_notes(),
        summary: String::new(),
    };
    report.summary = render_summary(&report);
    Ok(report)
}

pub fn report_notes() -> Vec<String> {
    vec![
        "Crawler families are UA-claimed (no reverse-DNS check). Googlebot UA spoofing is common \
         (~95% of Googlebot-UA requests site-wide in a 30-day sample were fake, mostly scanners on \
         non-HTML paths); user_logs only records HTML routes, so most Googlebot rows here are real."
            .into(),
        "KeenableBot includes Keenable's older crawler names SleepBot, Aranet-SearchBot and \
         Keenable-SearchBot."
            .into(),
        "humans = real-browser UAs on page paths; engaged_devices (>=2 visits or logged in) filters \
         one-shot scrapers wearing a browser UA."
            .into(),
        "cli counts only krab installs with telemetry on; records upload on the next invocation, \
         so the latest day can still grow."
            .into(),
        "Coverage = distinct live Battles/Claims fetched in the window vs. live totals now."
            .into(),
    ]
}

// ---------------------------------------------------------------------------
// Human rendering
// ---------------------------------------------------------------------------

fn fmt_int(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Compact multi-section text for `krab admin usage` human mode.
pub fn render_summary(r: &UsageReport) -> String {
    use std::fmt::Write;
    let d = |k: &str| -> String {
        r.previous
            .deltas
            .get(k)
            .map(|d| format!(" ({})", d.short()))
            .unwrap_or_default()
    };
    let mut s = String::new();
    let w = &r.window;
    let range = if w.days == 1 {
        format!("{}", w.last_day)
    } else {
        format!("{} → {} ({} days)", w.first_day, w.last_day, w.days)
    };
    let _ = writeln!(
        s,
        "KrabArena usage · {range}{} · vs previous {} day{}",
        if w.partial { " (partial)" } else { "" },
        w.days,
        if w.days == 1 { "" } else { "s" }
    );

    let c = &r.crawlers;
    let _ = writeln!(
        s,
        "\nCRAWLERS  {} hits{}",
        fmt_int(c.total),
        d("crawlers.total")
    );
    for cat in c.by_category.iter().take(6) {
        let _ = writeln!(
            s,
            "  {:<16} {:>8}{}",
            cat.label,
            fmt_int(cat.hits),
            d(&format!("crawlers.{}", cat.category.key()))
        );
    }
    let _ = writeln!(
        s,
        "  top families (coverage of {} battles / {} claims):",
        c.live_battles, c.live_claims
    );
    for f in c.by_family.iter().take(5) {
        let _ = writeln!(
            s,
            "    {:<22} {:>7}  battles {:>3} ({:.0}%)  claims {:>4} ({:.0}%)",
            truncate(&f.family, 22),
            fmt_int(f.hits),
            f.coverage.battles,
            f.coverage.battles_pct,
            f.coverage.claims,
            f.coverage.claims_pct
        );
    }
    // Always surface AI search APIs (the digest's audience cares most).
    let ai: Vec<&FamilyStat> = c
        .by_family
        .iter()
        .filter(|f| f.category == UaCategory::AiSearchApi)
        .take(5)
        .collect();
    if !ai.is_empty() {
        let parts: Vec<String> = ai
            .iter()
            .map(|f| format!("{} {} ({}c)", f.family, fmt_int(f.hits), f.coverage.claims))
            .collect();
        let _ = writeln!(s, "  AI search APIs: {}", parts.join(", "));
    }

    let h = &r.humans;
    let _ = writeln!(
        s,
        "\nHUMANS  {} engaged devices{} ({} new{}) · {} visits{} · {} devices{} ({} new)",
        fmt_int(h.engaged_devices),
        d("humans.engaged_devices"),
        fmt_int(h.new_engaged_devices),
        d("humans.new_engaged_devices"),
        fmt_int(h.visits),
        d("humans.visits"),
        fmt_int(h.devices),
        d("humans.devices"),
        fmt_int(h.new_devices),
    );
    let _ = writeln!(
        s,
        "  (engaged = >=2 page views or logged in; most one-shot browser-UA devices are cookie-less scrapers)"
    );
    let _ = writeln!(
        s,
        "  voters active {}{} · users active {}{} · logins {}{} · votes {}{}",
        h.voters_active,
        d("humans.voters_active"),
        h.users_active,
        d("humans.users_active"),
        h.logins,
        d("humans.logins"),
        h.votes,
        d("humans.votes"),
    );
    if !h.top_pages.is_empty() {
        let parts: Vec<String> = h
            .top_pages
            .iter()
            .take(5)
            .map(|p| format!("{} {}", truncate(&p.path, 48), p.visits))
            .collect();
        let _ = writeln!(s, "  top pages: {}", parts.join(" · "));
    }
    if !h.top_referrers.is_empty() {
        let parts: Vec<String> = h
            .top_referrers
            .iter()
            .take(5)
            .map(|p| format!("{} {}", p.host, p.visits))
            .collect();
        let _ = writeln!(s, "  top referrers: {}", parts.join(" · "));
    }

    let cli = &r.cli;
    let _ = writeln!(
        s,
        "\nCLI  {} users{} ({} bot) · {} new{} · {} invocations{} · {} failed",
        cli.users,
        d("cli.users"),
        cli.bot_users,
        cli.new_users,
        d("cli.new_users"),
        fmt_int(cli.invocations),
        d("cli.invocations"),
        fmt_int(cli.failed),
    );
    if !cli.top_commands.is_empty() {
        let parts: Vec<String> = cli
            .top_commands
            .iter()
            .take(5)
            .map(|c| format!("{} {}", c.command, c.invocations))
            .collect();
        let _ = writeln!(s, "  top commands: {}", parts.join(" · "));
    }
    if !cli.versions.is_empty() {
        let parts: Vec<String> = cli
            .versions
            .iter()
            .take(5)
            .map(|v| format!("{} ({}u)", v.version, v.users))
            .collect();
        let _ = writeln!(
            s,
            "  versions: {} · on latest {}: {}/{}",
            parts.join(", "),
            r.server_version,
            cli.users_on_latest,
            cli.users
        );
    }

    let ct = &r.content;
    let _ = writeln!(
        s,
        "\nCONTENT  battles {}{} · claims {}{} (+{} edits) · comments {}{} · verifications {}{} · proposals {}{}",
        ct.battles,
        d("content.battles"),
        ct.claims,
        d("content.claims"),
        ct.claim_edits,
        ct.comments,
        d("content.comments"),
        ct.verifications,
        d("content.verifications"),
        ct.battle_proposals,
        d("content.battle_proposals"),
    );
    let _ = write!(
        s,
        "  signups: users {}{} · voters {}{}",
        ct.signups.users,
        d("content.signups.users"),
        ct.signups.voters,
        d("content.signups.voters"),
    );
    s
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn default_window_is_yesterday() {
        let w = UsageWindow::resolve(None, None, d("2026-10-25")).unwrap();
        assert_eq!(
            (w.first_day, w.last_day, w.days),
            (d("2026-10-24"), d("2026-10-24"), 1)
        );
        assert_eq!(w.start().to_rfc3339(), "2026-10-24T00:00:00+00:00");
        assert_eq!(w.end().to_rfc3339(), "2026-10-25T00:00:00+00:00");
        let p = w.previous();
        assert_eq!(
            (p.first_day, p.last_day),
            (d("2026-10-23"), d("2026-10-23"))
        );
        assert_eq!(p.end(), w.start());
    }

    #[test]
    fn multi_day_window_ends_on_date_inclusive() {
        let w = UsageWindow::resolve(Some(d("2026-10-24")), Some(7), d("2026-10-25")).unwrap();
        assert_eq!(
            (w.first_day, w.last_day),
            (d("2026-10-18"), d("2026-10-24"))
        );
        assert_eq!(w.end() - w.start(), Duration::days(7));
        let p = w.previous();
        assert_eq!(
            (p.first_day, p.last_day, p.days),
            (d("2026-10-11"), d("2026-10-17"), 7)
        );
        assert_eq!(p.end(), w.start());
        // Month boundary.
        let w = UsageWindow::resolve(Some(d("2026-03-02")), Some(3), d("2026-10-25")).unwrap();
        assert_eq!(w.first_day, d("2026-02-28"));
    }

    #[test]
    fn window_validation() {
        let today = d("2026-10-25");
        assert!(UsageWindow::resolve(None, Some(0), today).is_err());
        assert!(UsageWindow::resolve(None, Some(91), today).is_err());
        assert!(UsageWindow::resolve(None, Some(90), today).is_ok());
        assert!(UsageWindow::resolve(Some(d("2026-10-26")), None, today).is_err());
        let w = UsageWindow::resolve(Some(today), None, today).unwrap();
        let now = today.and_hms_opt(12, 0, 0).unwrap().and_utc();
        assert!(w.info(now).partial);
        let y = UsageWindow::resolve(None, None, today).unwrap();
        assert!(!y.info(now).partial);
    }

    #[test]
    fn delta_math() {
        let x = Delta::new(112, 100);
        assert_eq!((x.change, x.pct), (12, Some(12.0)));
        assert_eq!(x.short(), "+12%");
        let x = Delta::new(2, 3);
        assert_eq!(x.pct, Some(-33.3));
        assert_eq!(x.short(), "-33.3%");
        let x = Delta::new(5, 0);
        assert_eq!((x.pct, x.short().as_str()), (None, "new"));
        assert_eq!(Delta::new(0, 0).short(), "±0");
        assert_eq!(Delta::new(7, 7).short(), "±0%");
    }

    #[test]
    fn deltas_cover_every_headline_metric() {
        let mut cur = CrawlerStats::default();
        cur.by_category.push(CategoryStat {
            category: UaCategory::AiSearchApi,
            label: "AI search API".into(),
            hits: 30,
            families: 2,
        });
        cur.total = 30;
        let prev = CrawlerStats {
            total: 20,
            ..Default::default()
        };
        let h = HumanStats {
            visits: 10,
            ..Default::default()
        };
        let a = headline_metrics(&cur, &h, &CliStats::default(), &ContentStats::default());
        let b = headline_metrics(
            &prev,
            &HumanStats::default(),
            &CliStats::default(),
            &ContentStats::default(),
        );
        let deltas = compute_deltas(&a, &b);
        assert_eq!(deltas["crawlers.total"].pct, Some(50.0));
        assert_eq!(deltas["crawlers.ai_search_api"].current, 30);
        assert_eq!(deltas["crawlers.ai_search_api"].previous, 0);
        assert_eq!(deltas["humans.visits"].short(), "new");
        assert!(deltas.contains_key("content.signups.voters"));
        assert!(deltas.contains_key("cli.new_users"));
        assert!(!deltas.contains_key("crawlers.browser"));
    }

    #[test]
    fn resolves_commands_through_manifest() {
        let m = krabarena_shared::manifest_builder::build_manifest();
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            resolve_command(
                &m,
                "claim",
                &a(&["claim", "get", "some-slug", "--format", "json"])
            ),
            "claim get"
        );
        assert_eq!(
            resolve_command(
                &m,
                "admin",
                &a(&["admin", "vendor-logos", "list", "--format", "json"])
            ),
            "admin vendor-logos list"
        );
        assert_eq!(
            resolve_command(&m, "admin", &a(&["admin", "usage", "--days", "7"])),
            "admin usage"
        );
        // Builtin not in the manifest: one word-looking token.
        assert_eq!(
            resolve_command(&m, "auth", &a(&["auth", "status", "--format", "json"])),
            "auth status"
        );
        assert_eq!(
            resolve_command(&m, "doctor", &a(&["doctor", "--fix"])),
            "doctor"
        );
        // Positional right after a leaf stops the walk.
        assert_eq!(
            resolve_command(&m, "search", &a(&["search", "exa", "--limit", "5"])),
            "search"
        );
        // Global flag before the command.
        assert_eq!(
            resolve_command(&m, "battle", &a(&["--format", "json", "battle", "list"])),
            "battle list"
        );
        assert_eq!(resolve_command(&m, "<none>", &[]), "<none>");
    }

    #[test]
    fn versions_sort_numerically() {
        assert!(version_key("0.43.10") > version_key("0.43.9"));
        assert!(version_key("0.43.0") > version_key("0.42.2"));
        assert!(version_key("?") < version_key("0.1.0"));
    }

    fn row(user: Uuid, version: &str, cmd: &str, args: &[&str], failed: bool, n: i64) -> CliRow {
        CliRow {
            user_id: user,
            is_bot: false,
            version: version.into(),
            agent: "unknown".into(),
            cmd: cmd.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            failed,
            n,
        }
    }

    #[test]
    fn cli_assembly_counts_users_commands_versions() {
        let m = krabarena_shared::manifest_builder::build_manifest();
        let (u1, u2) = (Uuid::new_v4(), Uuid::new_v4());
        let mut bot = row(u2, "0.43.7", "claim", &["claim", "list"], false, 1);
        bot.is_bot = true;
        bot.agent = "claude-code".into();
        let rows = vec![
            row(u1, "0.43.6", "claim", &["claim", "get", "x"], false, 3),
            row(u1, "0.43.7", "claim", &["claim", "get", "y"], true, 1),
            row(u1, "0.43.7", "battle", &["battle", "list"], false, 2),
            bot,
        ];
        let s = assemble_cli(rows, 1, "0.43.7", &m, true);
        assert_eq!((s.users, s.bot_users, s.new_users), (2, 1, 1));
        assert_eq!((s.invocations, s.failed), (7, 1));
        assert_eq!(s.top_commands[0].command, "claim get");
        assert_eq!(s.top_commands[0].invocations, 4);
        assert_eq!(s.top_commands[0].users, 1);
        assert_eq!(s.top_commands[0].failed, 1);
        assert_eq!(s.versions[0].version, "0.43.7");
        assert_eq!(s.versions[0].users, 2);
        // u1's highest version is 0.43.7, u2's too.
        assert_eq!(s.users_on_latest, 2);
        assert_eq!(s.agents[0].agent, "unknown");
    }

    #[test]
    fn crawler_assembly_folds_tail_and_sums_categories() {
        let mut rows = Vec::new();
        for i in 0..(MAX_FAMILIES + 5) {
            rows.push(FamilyRow {
                family: format!("Bot{i:02}"),
                category: if i == 0 {
                    "ai_search_api".into()
                } else {
                    "other_bot".into()
                },
                hits: 1000 - i as i64,
                battle_pages: 1,
                claim_pages: 2,
                battles_reached: 1,
                claims_reached: if i == 0 { 50 } else { 1 },
            });
        }
        let s = assemble_crawlers(rows, 10, 100);
        assert_eq!(s.by_family.len(), MAX_FAMILIES);
        assert_eq!(s.by_family.last().unwrap().family, "(other)");
        assert_eq!(s.by_family[0].coverage.claims_pct, 50.0);
        assert_eq!(s.by_family[0].coverage.battles_pct, 10.0);
        let total: i64 = (0..(MAX_FAMILIES as i64 + 5)).map(|i| 1000 - i).sum();
        assert_eq!(s.total, total);
        assert_eq!(s.by_category[0].category, UaCategory::OtherBot);
        assert_eq!(s.by_category[1].hits, 1000);
    }

    #[test]
    fn report_serializes_stable_shape() {
        let today = d("2026-10-25");
        let w = UsageWindow::resolve(None, None, today).unwrap();
        let now = today.and_hms_opt(6, 0, 0).unwrap().and_utc();
        let crawlers = assemble_crawlers(
            vec![FamilyRow {
                family: "KeenableBot".into(),
                category: "ai_search_api".into(),
                hits: 12,
                battle_pages: 3,
                claim_pages: 9,
                battles_reached: 2,
                claims_reached: 7,
            }],
            20,
            70,
        );
        let mut r = UsageReport {
            window: w.info(now),
            generated_at: now,
            server_version: "0.43.7".into(),
            crawlers,
            humans: HumanStats::default(),
            cli: CliStats::default(),
            content: ContentStats::default(),
            previous: PreviousWindow {
                window: w.previous().info(now),
                deltas: compute_deltas(
                    &BTreeMap::from([("crawlers.total".to_string(), 12)]),
                    &BTreeMap::from([("crawlers.total".to_string(), 10)]),
                ),
            },
            notes: report_notes(),
            summary: String::new(),
        };
        r.summary = render_summary(&r);
        let v = serde_json::to_value(&r).unwrap();
        for key in [
            "window",
            "generated_at",
            "server_version",
            "crawlers",
            "humans",
            "cli",
            "content",
            "previous",
            "notes",
            "summary",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert_eq!(v["window"]["first_day"], "2026-10-24");
        assert_eq!(v["window"]["days"], 1);
        assert_eq!(v["window"]["partial"], false);
        assert_eq!(v["crawlers"]["by_category"][0]["category"], "ai_search_api");
        let fam = &v["crawlers"]["by_family"][0];
        assert_eq!(fam["family"], "KeenableBot");
        assert_eq!(fam["coverage"]["claims"], 7);
        assert_eq!(fam["coverage"]["claims_pct"], 10.0);
        assert_eq!(fam["coverage"]["battles_pct"], 10.0);
        for key in [
            "visits",
            "devices",
            "engaged_devices",
            "new_devices",
            "voters_active",
            "users_active",
            "logins",
            "votes",
            "top_pages",
            "top_referrers",
        ] {
            assert!(v["humans"].get(key).is_some(), "humans.{key}");
        }
        for key in [
            "users",
            "bot_users",
            "new_users",
            "invocations",
            "failed",
            "top_commands",
            "versions",
            "users_on_latest",
            "agents",
        ] {
            assert!(v["cli"].get(key).is_some(), "cli.{key}");
        }
        assert_eq!(v["content"]["signups"]["voters"], 0);
        assert_eq!(v["previous"]["deltas"]["crawlers.total"]["pct"], 20.0);
        assert_eq!(v["previous"]["window"]["last_day"], "2026-10-23");
        let summary = v["summary"].as_str().unwrap();
        assert!(summary.contains("CRAWLERS  12 hits (+20%)"), "{summary}");
        assert!(summary.contains("HUMANS"));
        assert!(summary.contains("CLI"));
        assert!(summary.contains("CONTENT"));
        assert!(
            summary.contains("AI search APIs: KeenableBot 12 (7c)"),
            "{summary}"
        );
    }

    #[test]
    fn int_formatting() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(1000), "1,000");
        assert_eq!(fmt_int(1234567), "1,234,567");
        assert_eq!(fmt_int(-1234), "-1,234");
    }
}
