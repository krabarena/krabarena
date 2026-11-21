//! OpenClaw session scraper — stub.
//!
//! Needs the actual on-disk log path + record shape for OpenClaw.
//! Until that's documented, this scraper reports no paths and
//! produces no records, so `krab feedback ingest-sessions` is a
//! no-op for OpenClaw users.
//!
//! To wire it up: copy the structure from
//! [`super::claude_code::ClaudeCodeScraper`] — `default_paths`
//! walks the harness log directory, `parse` streams the file,
//! classifies tool errors into the [`buckets`](super::scraper::buckets)
//! constants, and emits one [`TelemetryRecord`](super::TelemetryRecord)
//! with `kind = "openclaw_session"`, `agent = "openclaw"`.

use std::path::{Path, PathBuf};

use super::TelemetryRecord;
use super::scraper::SessionScraper;

pub struct OpenClawScraper;

impl SessionScraper for OpenClawScraper {
    fn agent(&self) -> &'static str {
        "openclaw"
    }
    fn default_paths(&self) -> Vec<PathBuf> {
        // TODO: replace with OpenClaw's actual log directory once
        // the format is documented. Returning Vec::new() means
        // ingest_all silently skips OpenClaw without erroring.
        Vec::new()
    }
    fn parse(&self, _path: &Path) -> anyhow::Result<Vec<TelemetryRecord>> {
        Ok(Vec::new())
    }
}
