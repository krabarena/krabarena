use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub api_url: Option<String>,
    pub bearer_token: Option<String>,
    pub agent_backend: Option<String>,
    pub agent_model: Option<String>,
    /// `auto_update = false` opts out of the default-on self-update
    /// (see `crate::auto_update`). Unset means enabled. Declared above
    /// the `[telemetry]` table so it serializes as a top-level key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    /// `[telemetry]` section — controls the local-logs upload path
    /// landed in PR-B. Optional in the file so existing
    /// ~/.krab/config.toml files keep loading; when missing,
    /// [`Telemetry::share`] is treated as ON (the default) and a
    /// first-run consent banner is printed once.
    #[serde(default)]
    pub telemetry: Telemetry,
}

/// Telemetry toggles. Persisted as a TOML `[telemetry]` table.
///
/// `share` is the user-facing on/off (default ON). `consent_shown_at`
/// tracks whether we've printed the first-run banner — if it's None,
/// the next CLI invocation prints the banner and sets this to the
/// current ISO-8601 UTC string so it never appears again.
#[derive(Debug, Serialize, Deserialize)]
pub struct Telemetry {
    #[serde(default = "default_share")]
    pub share: bool,
    pub consent_shown_at: Option<String>,
}

fn default_share() -> bool {
    true
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            share: true,
            consent_shown_at: None,
        }
    }
}

impl Config {
    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".krab")
            .join("config.toml")
    }

    /// Legacy config path from the pre-rebrand `krabbit` CLI.
    fn legacy_config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".krabbit")
            .join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            return Ok(toml::from_str(&contents)?);
        }

        // One-time silent migration from ~/.krab/config.toml
        let legacy = Self::legacy_config_path();
        if legacy.exists() {
            let contents = std::fs::read_to_string(&legacy)?;
            let cfg: Self = toml::from_str(&contents)?;
            // Persist under the new path so subsequent loads skip the legacy branch.
            let _ = cfg.save();
            return Ok(cfg);
        }

        Ok(Self::default())
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        Ok(())
    }

    pub fn api_url(&self) -> &str {
        // `KRAB_API_URL` env var overrides everything else — used by
        // KrabScout local-test runs and any ops/dev wrapper that
        // points a single invocation at the test env without
        // touching ~/.krab/config.toml. Resolved lazily into a
        // process-local static via OnceLock so the &str signature
        // doesn't have to change for the env-var path.
        //
        // Below that: config file (`api_url` key) wins over the
        // baked-in default. Production lives at krabarena.com
        // (rebranded from .org on 2026-05-22; the .org apex
        // 301-redirects to .com so older configs that hardcoded
        // .org keep working, but new installs resolve to .com
        // directly).
        if let Some(env_url) = api_url_from_env_static() {
            return env_url;
        }
        self.api_url.as_deref().unwrap_or("https://krabarena.com")
    }
}

/// Read `KRAB_API_URL` once per process and intern it into a
/// `'static` slice so `Config::api_url` can keep its `&str` return
/// type. Returns `None` when the var is unset / empty / whitespace.
fn api_url_from_env_static() -> Option<&'static str> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Option<&'static str>> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let raw = std::env::var("KRAB_API_URL").ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        // Leak into a 'static — one-shot per process; harmless.
        Some(Box::leak(trimmed.to_string().into_boxed_str()))
    })
}
