//! `krab feedback` — telemetry surface.
//!
//! Three CLI-visible operations land here:
//!
//!   - `krab feedback ingest-sessions [--dry-run]` — scrape every
//!     registered [`SessionScraper`](crate::telemetry::SessionScraper)
//!     and either print canonical records (`--dry-run`) or write
//!     them to `~/.krab/logs/pending/` for the next CLI invocation
//!     to flush.
//!   - `krab feedback on | off | status` — flip / inspect the
//!     `[telemetry].share` flag in `~/.krab/config.toml`.
//!   - `krab feedback flush` — force-send any pending records right
//!     now. The opportunistic flush in `main.rs` is the normal path;
//!     `flush` is the synchronous debug variant.

use anyhow::Context;

use crate::client::KrabClient;
use crate::config::Config;
use crate::telemetry::{self, queue};

/// `krab feedback ingest-sessions [--dry-run]` body.
/// Walks every registered scraper. With `--dry-run`, prints the
/// canonical payload to stdout. Without it, enqueues the records
/// for the next CLI invocation's opportunistic flush.
pub fn ingest_sessions(dry_run: bool) -> anyhow::Result<()> {
    let records = telemetry::ingest_all();
    if dry_run {
        // Serialize each record to a JSON Value so the dry-run output
        // matches the shape the server actually receives (flattened
        // `records: [{…}]`, not `records: [{TelemetryRecord}]`).
        let values: Vec<serde_json::Value> = records
            .iter()
            .map(|r| serde_json::to_value(r).expect("TelemetryRecord serializes"))
            .collect();
        let payload = queue::IngestPayload {
            client: queue::client_envelope(),
            records: values,
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    if records.is_empty() {
        eprintln!("No session records found to enqueue.");
        return Ok(());
    }
    let path = queue::enqueue(&records).context("enqueue records")?;
    eprintln!("Enqueued {} records → {}", records.len(), path.display());
    Ok(())
}

/// `krab feedback log [--file <path>] [--note <text>] [--harness <name>]`
/// — paste a free-form note (transcript, prompt that broke, design
/// idea) into telemetry. Body comes from `--file`, `--note`, or stdin
/// (in that order). Lands as a `manual_note` record with kind
/// `manual_note` so it shows up alongside session scrapes and per-
/// invocation traces in the same `cli_telemetry` table.
///
/// Useful for harnesses without a scraper (Codex pre-0.9.7 / Aider /
/// Cursor) or for one-off "here's what I just tried" notes from
/// either humans or agents.
pub fn log(
    file: Option<&str>,
    note: Option<&str>,
    harness: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<()> {
    use crate::telemetry::record::{TelemetryRecord, redact_home, snippet_truncate};
    use std::io::Read;

    let body = if let Some(p) = file {
        std::fs::read_to_string(p).with_context(|| format!("read {p}"))?
    } else if let Some(n) = note {
        n.to_string()
    } else {
        // Stdin — useful for `agent-foo | krab feedback log`.
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .context("read stdin")?;
        s
    };
    let body = body.trim();
    if body.is_empty() {
        anyhow::bail!("empty note — pass --file, --note, or pipe text on stdin");
    }

    // Build a synthetic session id from a hash of the body + time so
    // duplicate pastes still get distinct ids (dedup key on the server
    // is (user_id, kind, session_id)).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let session_id = format!("note-{now}-{:x}", fxhash(body));
    let ts = format_iso8601_now_utc(now);
    // Truncate aggressively so a paste of a 1 MB log doesn't blow
    // the cli_telemetry row size budget. Bigger notes should land as
    // separate uploads.
    let snippet = snippet_truncate(&redact_home(body), 4_000);

    let record = TelemetryRecord {
        kind: "manual_note".into(),
        agent: harness.unwrap_or("manual").to_string(),
        ts,
        session_id,
        source_path: "manual".into(),
        assistant_turns: 0,
        user_turns: 0,
        total_records: 1,
        errors: Default::default(),
        samples: vec![crate::telemetry::record::ErrorSample {
            kind: "note".into(),
            snippet,
        }],
    };

    if dry_run {
        let values = vec![serde_json::to_value(&record)?];
        let payload = queue::IngestPayload {
            client: queue::client_envelope(),
            records: values,
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    let path = queue::enqueue(&[record]).context("enqueue note")?;
    eprintln!("Enqueued manual_note → {}", path.display());
    Ok(())
}

/// Cheap non-cryptographic 64-bit hash of a `&str`. Used to give
/// `manual_note` records a deterministic-looking session id so two
/// uploads of the same body dedupe. Mirrors the FxHash mixing
/// function — we don't pull a fxhash crate just for one call site.
fn fxhash(s: &str) -> u64 {
    let mut h: u64 = 0;
    for b in s.bytes() {
        h = h.rotate_left(5) ^ (b as u64);
        h = h.wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    h
}

/// ISO-8601 UTC for the current moment, second precision. Reuses the
/// claude_code formatter so we don't pull chrono.
fn format_iso8601_now_utc(unix_secs: u64) -> String {
    crate::telemetry::claude_code::format_iso8601_utc_public(unix_secs as i64)
}

/// `krab feedback on` — set `[telemetry].share = true` and clear
/// the consent-banner flag so the user sees it again next run.
pub fn turn_on() -> anyhow::Result<()> {
    let mut cfg = Config::load()?;
    cfg.telemetry.share = true;
    cfg.save()?;
    eprintln!("Telemetry: ON — anonymized command + error logs will be uploaded.");
    Ok(())
}

/// `krab feedback off` — set `share = false` and drop any pending
/// records so the off switch is total. (Off doesn't mean "stop
/// uploading later"; it means "throw away what we've collected.")
pub fn turn_off() -> anyhow::Result<()> {
    let mut cfg = Config::load()?;
    cfg.telemetry.share = false;
    cfg.save()?;
    // Drop the on-disk queue so we don't accidentally upload it
    // later if the user flips it back on. The session JSONLs the
    // scraper reads live in the agent harness's own log dirs and
    // are untouched.
    for p in queue::list_pending() {
        let _ = std::fs::remove_file(&p);
    }
    eprintln!("Telemetry: OFF — pending records dropped, no future uploads.");
    Ok(())
}

/// `krab feedback status` — print the current state.
pub fn status() -> anyhow::Result<()> {
    let cfg = Config::load()?;
    let pending = queue::list_pending();
    eprintln!(
        "Telemetry: {}",
        if cfg.telemetry.share { "ON" } else { "OFF" }
    );
    if let Some(when) = &cfg.telemetry.consent_shown_at {
        eprintln!("  Consent banner shown: {when}");
    } else {
        eprintln!("  Consent banner not shown yet.");
    }
    eprintln!("  Pending records on disk: {}", pending.len());
    let mut total_bytes: u64 = 0;
    for p in &pending {
        if let Ok(m) = std::fs::metadata(p) {
            total_bytes += m.len();
        }
    }
    eprintln!("  Pending bytes: {total_bytes}");
    Ok(())
}

/// `krab feedback flush` — synchronous variant of the opportunistic
/// flush that runs at the top of every invocation. Useful when the
/// user wants to confirm the upload landed before they walk away.
pub async fn flush(client: &KrabClient) -> anyhow::Result<()> {
    let summary = crate::telemetry::flush::run_flush(client).await;
    eprintln!(
        "Flush: posted {} batches, {} records, {} errors",
        summary.batches, summary.records, summary.errors
    );
    Ok(())
}

/// Print the first-run consent banner exactly once. Called from
/// `main.rs` right after `Config::load()` so it lands before any
/// telemetry upload. If `share == false`, the banner is skipped —
/// the user opted out before we ever rendered it (rare, but
/// possible via direct config edit).
pub fn maybe_show_consent_banner(cfg: &mut Config) {
    if !cfg.telemetry.share {
        return;
    }
    if cfg.telemetry.consent_shown_at.is_some() {
        return;
    }
    eprintln!();
    eprintln!("  ⓘ KrabArena CLI collects anonymized command + error logs");
    eprintln!("    to improve the tool. Includes: command name, duration,");
    eprintln!("    exit code, error class. Excludes: file paths under your");
    eprintln!("    home directory (replaced with $HOME). Run `krab feedback");
    eprintln!("    off` to disable.");
    eprintln!();
    // Record so we don't show it again. ISO-8601 via `format!` to
    // avoid pulling chrono into krab-cli for this single use.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    cfg.telemetry.consent_shown_at = Some(format!("unix:{now}"));
    let _ = cfg.save();
}
