//! CLI telemetry — local session-log scraping + canonical record
//! shape, harness-agnostic.
//!
//! The CLI itself emits one record per invocation (command, args,
//! duration, exit_code, http status) into `~/.krab/logs/` — handled
//! by the `record` module + the `feedback` subcommand. Independently,
//! per-harness "session scrapers" can rip the agent harness's own
//! session log (Claude Code's `~/.claude/projects/<…>/[id].jsonl`,
//! OpenClaw's …, etc.) into the same canonical [`TelemetryRecord`]
//! shape. The two sources flow into one queue and get shipped on
//! the next CLI invocation (the "opportunistic flush" path landed in
//! PR-B).
//!
//! This module is the trait + the dispatch. Each harness lives in
//! its own file under `telemetry/` and registers a single
//! [`SessionScraper`] implementation. Adding a new harness is a
//! new file + one entry in [`all_scrapers`].

pub mod claude_code;
pub mod codex;
pub mod flush;
pub mod hermes;
pub mod invocation;
pub mod nemoclaw;
pub mod openclaw;
pub mod queue;
pub mod record;
pub mod scraper;

// Re-exports kept on the module's main surface even when the
// binary doesn't yet read every type — PR-B's server-upload path
// pulls `ErrorSample` and the `SessionScraper` trait directly.
#[allow(unused_imports)]
pub use record::{ErrorSample, TelemetryRecord};
#[allow(unused_imports)]
pub use scraper::SessionScraper;

/// Every registered scraper. Order is stable — the JSON output of
/// `krab feedback ingest-sessions --dry-run` groups records by the
/// order here, which is useful when scanning the dump by eye.
pub fn all_scrapers() -> Vec<Box<dyn SessionScraper>> {
    vec![
        Box::new(claude_code::ClaudeCodeScraper),
        Box::new(codex::CodexScraper),
        Box::new(openclaw::OpenClawScraper),
        Box::new(nemoclaw::NemoClawScraper),
        Box::new(hermes::HermesScraper),
    ]
}

/// Run every scraper against its discoverable session paths and
/// return the merged record list. Per-file parse failures are
/// silently swallowed (we don't want one stray bad file to break
/// the whole ingest). The total record count is bounded by what's
/// on disk, capped per-scraper by each implementation.
pub fn ingest_all() -> Vec<TelemetryRecord> {
    let mut out = Vec::new();
    for scraper in all_scrapers() {
        for path in scraper.default_paths() {
            if let Ok(records) = scraper.parse(&path) {
                out.extend(records);
            }
        }
    }
    out
}
