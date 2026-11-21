//! Library facade for the krabarena-api crate.
//!
//! Exists so out-of-tree binaries (under `src/bin/`) can pull in
//! the same module tree the main `krabarena-api` server uses
//! without re-implementing services / models / clients. The
//! `krabarena-api` binary at `src/main.rs` and the backfill
//! binaries at `src/bin/*.rs` both depend on this lib.

use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;

pub mod cache;
pub mod db;
pub mod envelope;
pub mod error;
pub mod markdown;
pub mod models;
pub mod openapi;
pub mod pagination;
pub mod routes;
pub mod services;
pub mod web;

/// Short-TTL memo for the leaderboard aggregates (`/agents`, the home
/// top-10, `/arena`). See `crate::cache`.
pub const LEADERBOARD_CACHE_TTL: Duration = Duration::from_secs(60);

/// Bundle of leaderboard result caches, shared behind an `Arc` so
/// cloning `AppState` per request is cheap.
pub struct LeaderboardCaches {
    /// `web::pages::top_agents_by_effort` rows, keyed by `limit`. Powers
    /// the `/agents` page and the home page top-10.
    pub agents_web: cache::TtlCache<Vec<web::pages::AgentRow>>,
    /// JSON `/v1/agents` leaderboard rows, keyed by `limit`.
    pub agents_json: cache::TtlCache<Vec<routes::agents::KrabberResponse>>,
    /// `/arena` merged rows + cost rollup.
    pub arena: cache::TtlCache<(Vec<web::pages::ArenaRow>, web::pages::ReviewCostRollup)>,
}

impl LeaderboardCaches {
    pub fn new() -> Self {
        Self {
            agents_web: cache::TtlCache::new(LEADERBOARD_CACHE_TTL),
            agents_json: cache::TtlCache::new(LEADERBOARD_CACHE_TTL),
            arena: cache::TtlCache::new(LEADERBOARD_CACHE_TTL),
        }
    }
}

impl Default for LeaderboardCaches {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_secret: String,
    pub blob_store_url: String,
    /// Non-public store for claims' *private* artifacts
    /// (`PRIVATE_BLOB_STORE_URL`, e.g. `gs://krabarena-artifacts-private`).
    /// `None` = private uploads refused (production without a private
    /// bucket configured). Never served by URL — only streamed through
    /// the authorized `GET /v1/claims/{id}/private-artifact` route.
    pub private_blob_store_url: Option<String>,
    pub public_base_url: String,
    pub discord_monitor: services::discord_monitor::DiscordMonitorConfig,
    pub email_service: Option<services::email_service::EmailService>,
    pub gemini: services::gemini_service::GeminiClient,
    /// Shared reqwest client for the battle proxy (`routes::proxy`).
    /// Built once at startup so connections to upstream search APIs
    /// are pooled + TLS sessions reused across requests. Without
    /// this, every proxy hit ate a fresh ~500-700ms TCP+TLS
    /// handshake (measured: 800ms p50 through proxy vs 180ms direct
    /// to the same upstream).
    pub proxy_http: reqwest::Client,
    /// Per-IP / per-user Redis rate limiter (`web::rate_limit`). `None`
    /// when `REDIS_URL` is unset (local/dev) — the middleware then
    /// becomes a zero-cost pass-through. Built once at startup; `Clone`
    /// is cheap (the pool is an `Arc` inside).
    pub rate_limiter: Option<web::rate_limit::RateLimiter>,
    /// Short-TTL leaderboard memo, shared across requests.
    pub leaderboards: Arc<LeaderboardCaches>,
    /// Google reCAPTCHA Enterprise config for the score-based sign-in bot
    /// gate. Disabled (verification skipped, no widget rendered) unless fully
    /// configured — see [`RecaptchaConfig`].
    pub recaptcha: RecaptchaConfig,
}

/// Google reCAPTCHA Enterprise (score-based) configuration.
///
/// The [`Default`] (all strings empty) disables captcha entirely: no widget
/// is rendered and the sign-in gate is skipped — the local-dev / key-less
/// behavior. Populated from `RECAPTCHA_*` env in `main.rs`.
#[derive(Clone, Debug)]
pub struct RecaptchaConfig {
    /// GCP project id that owns the Enterprise key and receives assessments
    /// (`RECAPTCHA_ENTERPRISE_PROJECT_ID`).
    pub project_id: String,
    /// Enterprise site key id — rendered into the page HTML and sent as the
    /// assessment `event.siteKey` (`RECAPTCHA_SITE_KEY`). Public by design.
    pub site_key: String,
    /// API key restricted to the reCAPTCHA Enterprise API, used to
    /// authenticate the assessment call (`RECAPTCHA_ENTERPRISE_API_KEY`).
    pub api_key: String,
    /// Minimum acceptable risk score in `[0.0, 1.0]`
    /// (`RECAPTCHA_SCORE_THRESHOLD`, default `0.5`). Sign-ins scoring below
    /// this are rejected.
    pub score_threshold: f64,
}

impl Default for RecaptchaConfig {
    fn default() -> Self {
        Self {
            project_id: String::new(),
            site_key: String::new(),
            api_key: String::new(),
            score_threshold: 0.5,
        }
    }
}

impl RecaptchaConfig {
    /// Captcha is active only when the project id, site key, and API key are
    /// all present. Any missing → disabled (skip verification, render no
    /// widget).
    pub fn is_enabled(&self) -> bool {
        !self.project_id.is_empty() && !self.site_key.is_empty() && !self.api_key.is_empty()
    }
}
