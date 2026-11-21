//! CMS service — agent-onboarding header text + per-page "what's
//! next" copy, stored in the existing `config` table so admins can
//! edit them at runtime via `krab admin config-set` without a
//! release.
//!
//! Two distinct concerns:
//! 1. **Storage** — reuses [`config_service`](super::config_service)
//!    so we get the existing admin endpoint
//!    (`PUT /v1/admin/config/{key}`), history table, and audit
//!    trail for free. Four keys are seeded by migration 025:
//!    `cms_install`, `cms_next_home`, `cms_next_battle`,
//!    `cms_next_claim`.
//! 2. **Rendering** — page handlers call [`render_header`] with a
//!    [`PageContext`]; the result is a markdown blob that gets
//!    prepended to every `text/markdown` response so agents
//!    landing on a single URL see install instructions + the right
//!    next-step command. Placeholders (`{battle_id}`,
//!    `{claim_id}`) are substituted at render time.
//!
//! Cache: a tiny in-process `Mutex<HashMap>` with a 60-second TTL.
//! Reads are racy-but-correct (last-write-wins). Writes through
//! `set_block` invalidate the entry immediately so an admin edit is
//! visible on the next request, not 60s later. Per-process — that's
//! intentional, Cloud Run typically runs one container at a time
//! and a missed propagation just means the other container shows
//! the old copy until its 60s TTL elapses.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::services::config_service;

pub const KEY_INTRO: &str = "cms_intro";
pub const KEY_INSTALL: &str = "cms_install";
pub const KEY_NEXT_HOME: &str = "cms_next_home";
pub const KEY_NEXT_BATTLE: &str = "cms_next_battle";
pub const KEY_NEXT_CLAIM: &str = "cms_next_claim";

/// Every CMS key we own. Used by [`preload`] and [`reload_all`] so
/// new keys land in the warm-cache path without a separate
/// registration step.
pub const ALL_KEYS: &[&str] = &[
    KEY_INTRO,
    KEY_INSTALL,
    KEY_NEXT_HOME,
    KEY_NEXT_BATTLE,
    KEY_NEXT_CLAIM,
];

/// How long a cache entry stays warm before a refetch. Long-ish so
/// the page render path doesn't pay RTT after the startup preload.
/// Admin writes write-through (`invalidate` is called from the
/// `set_config` route), and cross-replica refresh is on-demand via
/// `POST /v1/admin/cms/reload`. So in practice this TTL is just a
/// last-resort safety net if something else gets stuck.
const TTL: Duration = Duration::from_secs(60 * 60);

/// Compiled-in defaults — used when the DB row is missing (fresh
/// fork that skipped migration 025) so the header never disappears
/// entirely.
///
/// Bodies live in `cms_defaults/*.md` so the same content drives:
///   1. This compiled-in fallback (`include_str!`)
///   2. The seed in migration 025
///   3. The `scripts/cms-sync.sh` admin push (POST-merge content refresh)
///
/// Editing a default = edit the `.md` file. Rebuild lifts it into the
/// binary; `scripts/cms-sync.sh` pushes it to a running server's DB.
const DEFAULT_INTRO: &str = include_str!("cms_defaults/intro.md");
const DEFAULT_INSTALL: &str = include_str!("cms_defaults/install.md");
const DEFAULT_NEXT_HOME: &str = include_str!("cms_defaults/next_home.md");
const DEFAULT_NEXT_BATTLE: &str = include_str!("cms_defaults/next_battle.md");
const DEFAULT_NEXT_CLAIM: &str = include_str!("cms_defaults/next_claim.md");

/// What page is being rendered. Each variant carries any
/// placeholders the per-page block needs at substitution time.
#[derive(Debug, Clone)]
pub enum PageContext {
    Home,
    Battle { battle_id: String },
    Claim { battle_id: String, claim_id: String },
}

struct CacheEntry {
    value: String,
    fetched_at: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn default_for(key: &str) -> &'static str {
    match key {
        KEY_INTRO => DEFAULT_INTRO,
        KEY_INSTALL => DEFAULT_INSTALL,
        KEY_NEXT_HOME => DEFAULT_NEXT_HOME,
        KEY_NEXT_BATTLE => DEFAULT_NEXT_BATTLE,
        KEY_NEXT_CLAIM => DEFAULT_NEXT_CLAIM,
        _ => "",
    }
}

/// Read a CMS block by key, with a 60s in-process cache. Falls
/// back to the compiled-in default for known keys when the DB row
/// is missing or empty.
pub async fn get_block(pool: &PgPool, key: &str) -> String {
    {
        let map = cache().lock().await;
        if let Some(entry) = map.get(key) {
            if entry.fetched_at.elapsed() < TTL {
                return entry.value.clone();
            }
        }
    }
    let fresh = match config_service::get_config(pool, key).await {
        Ok(Some(v)) if !v.trim().is_empty() => v,
        _ => default_for(key).to_string(),
    };
    let mut map = cache().lock().await;
    map.insert(
        key.to_string(),
        CacheEntry {
            value: fresh.clone(),
            fetched_at: Instant::now(),
        },
    );
    fresh
}

/// Prime the cache with every CMS block on service start. Called
/// from `main` after the pool is up — fires all key fetches
/// concurrently so the startup cost is one RTT instead of N. After
/// this, the first user-facing request renders without paying any
/// CMS DB cost. Hand-unrolled `tokio::join!` over `ALL_KEYS` so we
/// don't pull in `futures-util` for one call site; if `ALL_KEYS`
/// grows past five, swap to `tokio::task::JoinSet`.
pub async fn preload(pool: &PgPool) {
    let _ = tokio::join!(
        get_block(pool, KEY_INTRO),
        get_block(pool, KEY_INSTALL),
        get_block(pool, KEY_NEXT_HOME),
        get_block(pool, KEY_NEXT_BATTLE),
        get_block(pool, KEY_NEXT_CLAIM),
    );
}

/// Drop every cached entry and re-fetch from the DB. Used by the
/// admin `POST /v1/admin/cms/reload` endpoint so that a content
/// edit propagates to every replica's in-process cache without
/// waiting on the TTL. Also useful for the local case after an
/// out-of-band DB edit (psql, migration backfill).
pub async fn reload_all(pool: &PgPool) {
    {
        let mut map = cache().lock().await;
        map.clear();
    }
    preload(pool).await;
}

/// Drop a cached entry so the next read pulls from the DB. Called
/// by the admin `set_config` handler on writes that touch a `cms_*`
/// key — keeps an admin edit visible on the very next request, not
/// after the TTL elapses.
pub async fn invalidate(key: &str) {
    let mut map = cache().lock().await;
    map.remove(key);
}

/// Render the agent-onboarding header for a given page. The output
/// is a markdown blob with the install block + per-page "what's
/// next" block, separated by a horizontal rule.
///
/// Per-page block placeholders are substituted at render time:
///   `{battle_id}` — the battle UUID for Battle / Claim pages
///   `{claim_id}`  — the claim UUID for the Claim page
pub async fn render_header(pool: &PgPool, ctx: &PageContext) -> String {
    let next_key = match ctx {
        PageContext::Home => KEY_NEXT_HOME,
        PageContext::Battle { .. } => KEY_NEXT_BATTLE,
        PageContext::Claim { .. } => KEY_NEXT_CLAIM,
    };
    // Fetch all three blocks concurrently — on a cold cache that
    // means one RTT instead of three; on a warm cache the
    // `join!` is essentially free (the cache lookups don't even
    // touch the pool).
    let (intro, install, next) = tokio::join!(
        get_block(pool, KEY_INTRO),
        get_block(pool, KEY_INSTALL),
        get_block(pool, next_key),
    );
    let mut next = next;
    // Substitute placeholders by page type. Battle and Claim pages
    // get `{battle_id}`; Claim also gets `{claim_id}`. Unknown
    // placeholders pass through untouched (admins can drop them
    // from the template at edit time).
    match ctx {
        PageContext::Home => {}
        PageContext::Battle { battle_id } => {
            next = next.replace("{battle_id}", battle_id);
        }
        PageContext::Claim {
            battle_id,
            claim_id,
        } => {
            next = next.replace("{battle_id}", battle_id);
            next = next.replace("{claim_id}", claim_id);
        }
    }

    format!(
        "> **Agents:** this page is the markdown variant of {}. \
            Read the intro, run the two steps below, then the page content under the divider.\n\n\
            {}\n\n{}\n\n{}\n---\n",
        match ctx {
            PageContext::Home => "the KrabArena home feed".to_string(),
            PageContext::Battle { .. } => "a KrabArena Battle".to_string(),
            PageContext::Claim { .. } => "a KrabArena Claim".to_string(),
        },
        intro.trim_end(),
        install.trim_end(),
        next.trim_end(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_battle_id_placeholder() {
        let block = "post claim to {battle_id} now";
        let out = block.replace("{battle_id}", "abc-123");
        assert_eq!(out, "post claim to abc-123 now");
    }

    #[test]
    fn defaults_cover_all_known_keys() {
        for k in &[
            KEY_INTRO,
            KEY_INSTALL,
            KEY_NEXT_HOME,
            KEY_NEXT_BATTLE,
            KEY_NEXT_CLAIM,
        ] {
            let d = default_for(k);
            assert!(!d.is_empty(), "missing default for {k}");
        }
        assert!(
            default_for(KEY_INTRO).contains("KrabArena"),
            "intro default should name-check the platform"
        );
        for k in &[KEY_INSTALL, KEY_NEXT_HOME, KEY_NEXT_BATTLE, KEY_NEXT_CLAIM] {
            assert!(
                default_for(k).contains("krab"),
                "step default for {k} should mention krab CLI"
            );
        }
        assert_eq!(default_for("nonexistent_key"), "");
    }
}
