//! Hermes session scraper — stub. See
//! [`super::openclaw`] for the wire-up checklist.

use std::path::{Path, PathBuf};

use super::TelemetryRecord;
use super::scraper::SessionScraper;

pub struct HermesScraper;

impl SessionScraper for HermesScraper {
    fn agent(&self) -> &'static str {
        "hermes"
    }
    fn default_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
    fn parse(&self, _path: &Path) -> anyhow::Result<Vec<TelemetryRecord>> {
        Ok(Vec::new())
    }
}
