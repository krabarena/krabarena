//! Canonical telemetry record shape — the lingua franca every
//! harness-specific scraper emits, and the same JSON the eventual
//! `POST /v1/telemetry/cli` accepts.
//!
//! Lives in its own file so the server side (PR-B) can `use
//! krab_cli::telemetry::record::TelemetryRecord` without pulling in
//! the scraper trait + its filesystem dependencies. Keep this
//! lightweight: no fs, no async, no clap.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One scraped session (Claude Code, OpenClaw, etc.) projected into
/// a compact summary. The raw session log never leaves the box —
/// only counts and ≤5 redacted snippets. Path-shaped error text has
/// the user's `$HOME` redacted before it lands in `samples`, so
/// usernames and project paths don't leak in the upload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelemetryRecord {
    /// Discriminator. Examples: `"claude_code_session"`,
    /// `"openclaw_session"`, `"nemoclaw_session"`, `"hermes_session"`.
    pub kind: String,
    /// Best-effort harness identity (versioned when we can sniff it).
    /// Example: `"claude-code"`. Version goes on the wrapper, not
    /// here, so we don't have to update the scraper every release.
    pub agent: String,
    /// ISO-8601 timestamp at the start of the source session if the
    /// log carries one, else the file's mtime in UTC.
    pub ts: String,
    /// Stable session identifier — UUID for Claude Code, whatever
    /// the harness uses elsewhere. Used to dedupe re-ingests on the
    /// server side.
    pub session_id: String,
    /// Relative path the record was scraped from, with `$HOME`
    /// redacted. Surfaces in the per-record log for forensics; the
    /// server stores it for dedup.
    pub source_path: String,
    /// Total assistant turns observed (i.e. iterations the model
    /// took). A proxy for "how much work this session did".
    pub assistant_turns: u32,
    /// Total user-role records (includes tool_results, not just
    /// human prompts). Counted because some harnesses pack tool
    /// errors into user-role records.
    pub user_turns: u32,
    /// Total records in the source file — a rough size proxy.
    pub total_records: u32,
    /// Bucketed error counts. Stable, lowercase keys — see the
    /// `ErrorBucket` constants on the scraper trait. Use a `BTreeMap`
    /// so the JSON ordering is deterministic across runs (helps
    /// human diffing of two ingests).
    pub errors: BTreeMap<String, u32>,
    /// Up to 5 redacted error snippets — at most one per bucket so
    /// the upload stays tiny. Implementations should truncate each
    /// snippet at 160 chars.
    pub samples: Vec<ErrorSample>,
}

/// A single redacted error snippet. Used to flesh out the counts in
/// [`TelemetryRecord::errors`] with a concrete example — enough for
/// a human to recognize the failure mode, not enough to leak PII.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorSample {
    /// Same bucket key as the matching `errors` entry.
    pub kind: String,
    /// Up to 160 chars of the error text, `$HOME` redacted.
    pub snippet: String,
}

/// Replace the user's home directory with the literal string
/// `$HOME` so usernames and project paths don't leak in samples.
/// No-op if home can't be resolved (CI sandboxes, container builds).
pub fn redact_home(s: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return s.to_string();
    };
    let home_str = home.to_string_lossy();
    if home_str.is_empty() {
        return s.to_string();
    }
    s.replace(&*home_str, "$HOME")
}

/// Truncate `s` to at most `max` chars, appending `…` when cut.
/// Used by scrapers to bound the size of error snippets — 160 chars
/// is enough to recognise the error kind in the dashboard, and the
/// 5×160 = 800-byte ceiling per record keeps a busy session under a
/// few KB on the wire.
pub fn snippet_truncate(s: &str, max: usize) -> String {
    // Char-wise count vs byte-wise so multibyte input doesn't get
    // sliced mid-codepoint. `chars().count()` is O(n) twice — fine
    // for the ≤200-char inputs this fn actually sees.
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_truncate_at_max() {
        // Equal-or-shorter input passes through unchanged.
        assert_eq!(snippet_truncate("hello", 10), "hello");
        // 11 src chars, cap 5 → first 5 + ellipsis.
        assert_eq!(snippet_truncate("hello world", 5), "hello…");
        // 5 multibyte src chars, cap 3 → first 3 char-wise (not
        // byte-wise) + ellipsis. The `é` is a 2-byte UTF-8 codepoint;
        // a byte-wise slice at offset 3 would split it.
        assert_eq!(snippet_truncate("héllo", 3), "hél…");
        // Cap exactly equal to length → no ellipsis.
        assert_eq!(snippet_truncate("héllo", 5), "héllo");
    }

    #[test]
    fn redact_home_leaves_unrelated_paths_alone() {
        // We can't assert what home is in CI, so just check no panic
        // and that an obviously-unrelated path is unchanged.
        let out = redact_home("/etc/hosts");
        assert_eq!(out, "/etc/hosts");
    }
}
