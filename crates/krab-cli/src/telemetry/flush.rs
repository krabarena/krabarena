//! Opportunistic-flush-on-next-command.
//!
//! Called from `main.rs` at the top of every krab invocation BEFORE
//! the user's command runs. We spawn this on the Tokio runtime so
//! the upload happens concurrently with the actual command — if the
//! command finishes first, the flush keeps going on the background
//! task; if the flush hits the network and blocks, the command isn't
//! held up.
//!
//! Failure model:
//!   - Telemetry is opt-out, defaulting on, but the flush is
//!     ALWAYS resilient: any error (no token, no network, server
//!     5xx) is swallowed and the on-disk records stay queued. The
//!     next invocation retries.
//!   - We never log to stderr from the spawned task — that would
//!     interleave with the user's command output. The
//!     synchronous `krab feedback flush` variant in
//!     `commands::feedback::flush` is the path for users who want
//!     to see what's happening.

use std::path::Path;

use crate::client::KrabClient;
use crate::config::Config;

use super::queue;

/// Summary returned by [`run_flush`] — surfaced to the user only by
/// the synchronous `krab feedback flush` variant.
#[derive(Debug, Default)]
pub struct FlushSummary {
    pub batches: u32,
    pub records: u32,
    pub errors: u32,
}

/// Best-effort flush of every pending file. On 2xx, the file is
/// deleted. On any error, the file stays for the next attempt.
pub async fn run_flush(client: &KrabClient) -> FlushSummary {
    queue::prune_stale();
    let cfg = match Config::load() {
        Ok(c) => c,
        Err(_) => return FlushSummary::default(),
    };
    if !cfg.telemetry.share {
        // Off — clear any stragglers and bail.
        for p in queue::list_pending() {
            let _ = std::fs::remove_file(&p);
        }
        return FlushSummary::default();
    }
    if cfg.bearer_token.is_none() {
        // No auth — can't upload. Records stay queued for after
        // the user runs `krab auth …`. We don't queue forever:
        // `prune_stale()` above caps each file at 7 days.
        return FlushSummary::default();
    }

    let mut summary = FlushSummary::default();
    for path in queue::list_pending() {
        if try_send_file(client, &path).await {
            let _ = std::fs::remove_file(&path);
            summary.batches += 1;
        } else {
            summary.errors += 1;
        }
        // We don't know exactly how many records went in without
        // re-reading the file; for the synchronous flush summary
        // we'll re-count.
        if let Ok(records) = queue::read_records(&path) {
            summary.records += records.len() as u32;
        }
    }
    summary
}

/// POST one pending file. Returns true on 2xx, false otherwise.
/// Uses [`queue::read_values`] so both session-summary and
/// per-invocation record shapes round-trip cleanly — the typed
/// [`queue::read_records`] would silently drop `cli_command` rows.
async fn try_send_file(client: &KrabClient, path: &Path) -> bool {
    let Ok(records) = queue::read_values(path) else {
        return false;
    };
    if records.is_empty() {
        return true; // empty file: delete-on-success is correct
    }
    let payload = queue::IngestPayload {
        client: queue::client_envelope(),
        records,
    };
    let body = match serde_json::to_value(&payload) {
        Ok(v) => v,
        Err(_) => return false,
    };
    match client.post_raw("/v1/telemetry/cli", &body).await {
        Ok(status) => (200..300).contains(&status),
        Err(_) => false,
    }
}
