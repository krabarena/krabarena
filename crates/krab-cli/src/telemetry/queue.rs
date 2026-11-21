//! On-disk telemetry queue.
//!
//! Layout:
//!
//!   `~/.krab/logs/pending/<timestamp>-<rand>.jsonl` — one file per
//!   batch waiting to be uploaded. Each file holds one or more
//!   [`TelemetryRecord`] lines, JSONL.
//!
//! The queue is intentionally dumb: append a JSONL file when
//! `ingest-sessions` runs, drain files on a successful upload, drop
//! files older than 7 days regardless. No locks, no SQLite — the
//! per-file unit means concurrent CLI invocations don't step on
//! each other (each writes its own file), and partial reads on
//! failed uploads aren't a problem because the upload is
//! transactional at the file level (we delete only after a 2xx).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::TelemetryRecord;

/// Maximum age before a pending file is silently dropped on the
/// next flush. Records sitting in the queue for over a week are
/// almost certainly from a CLI that's been offline too long for
/// the data to be actionable — and they take up disk.
pub const MAX_AGE_DAYS: u64 = 7;

/// `~/.krab/logs/pending/`. Created lazily.
pub fn pending_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".krab")
        .join("logs")
        .join("pending")
}

/// Write a batch of session-summary records (from a SessionScraper)
/// to a new pending file. Convenience wrapper over [`enqueue_values`]
/// for the common case where the caller has typed
/// [`TelemetryRecord`]s and just wants them queued.
pub fn enqueue(records: &[TelemetryRecord]) -> anyhow::Result<PathBuf> {
    let values: Vec<Value> = records
        .iter()
        .map(|r| serde_json::to_value(r).expect("TelemetryRecord serializes"))
        .collect();
    enqueue_values(&values)
}

/// Write an arbitrary batch of JSON records to a new pending file.
/// Used by the per-invocation logger (`telemetry::invocation`) so we
/// can store a different record shape (`kind = "cli_command"`)
/// without trying to wedge it into the [`TelemetryRecord`] schema —
/// the server's envelope accepts both shapes via the flattened map.
///
/// Filename is `<unix-secs>-<pid>-<nanos>.jsonl` so two writers
/// don't collide.
pub fn enqueue_values(records: &[Value]) -> anyhow::Result<PathBuf> {
    let dir = pending_dir();
    fs::create_dir_all(&dir).context("create ~/.krab/logs/pending/")?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("{secs}-{pid}-{nanos}.jsonl"));
    let mut f = fs::File::create(&path).context("create pending file")?;
    for r in records {
        let line = serde_json::to_string(r).context("serialize record")?;
        writeln!(f, "{line}").context("write record")?;
    }
    Ok(path)
}

/// Pending files visible to the next flush. Filters non-jsonl
/// files so a stray editor backup doesn't break anything.
pub fn list_pending() -> Vec<PathBuf> {
    let dir = pending_dir();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
    out
}

/// Read one pending file into typed session records. Used by tests
/// and the synchronous `flush` summary path that wants to count
/// records by hand. Records that aren't [`TelemetryRecord`]-shaped
/// (e.g. `cli_command` invocation records) are silently skipped —
/// they're still uploaded via [`read_values`] but don't appear in
/// the typed count.
pub fn read_records(path: &std::path::Path) -> anyhow::Result<Vec<TelemetryRecord>> {
    let content = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(r) = serde_json::from_str::<TelemetryRecord>(line) {
            out.push(r);
        }
    }
    Ok(out)
}

/// Read one pending file into arbitrary JSON records. Used by the
/// flush path — the server's envelope accepts any shape, so we just
/// pipe through.
pub fn read_values(path: &std::path::Path) -> anyhow::Result<Vec<Value>> {
    let content = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            out.push(v);
        }
    }
    Ok(out)
}

/// Drop pending files older than MAX_AGE_DAYS. Best-effort — if the
/// metadata read or the unlink fails we just skip it; the next flush
/// tries again.
pub fn prune_stale() {
    let cutoff =
        SystemTime::now().checked_sub(std::time::Duration::from_secs(MAX_AGE_DAYS * 86_400));
    let Some(cutoff) = cutoff else {
        return;
    };
    for p in list_pending() {
        let too_old = fs::metadata(&p)
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if too_old {
            let _ = fs::remove_file(&p);
        }
    }
}

/// Sidecar payload shape the CLI POSTs and the server's
/// `/v1/telemetry/cli` accepts. `records` is intentionally untyped
/// JSON — we ship two shapes today (session summaries and
/// per-invocation `cli_command` records) and the server's envelope
/// in `routes/telemetry.rs` pulls the canonical fields by name +
/// captures the rest via a flattened map.
#[derive(Debug, Serialize, Deserialize)]
pub struct IngestPayload {
    pub client: ClientEnvelope,
    pub records: Vec<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClientEnvelope {
    pub krab_version: String,
    pub os: String,
}

/// Build the canonical client envelope from compile-time
/// `CARGO_PKG_VERSION` + the runtime os/arch. Used by both the
/// `--dry-run` printer and the real upload.
pub fn client_envelope() -> ClientEnvelope {
    ClientEnvelope {
        krab_version: env!("CARGO_PKG_VERSION").to_string(),
        os: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Build a synthetic record for fixtures. Stays in one place
    /// so test edits only touch one spot when the shape changes.
    fn fake_record(session_id: &str) -> TelemetryRecord {
        let mut errors = BTreeMap::new();
        errors.insert("shell_exit_nonzero".into(), 3);
        TelemetryRecord {
            kind: "claude_code_session".into(),
            agent: "claude-code".into(),
            ts: "2026-05-11T20:00:00Z".into(),
            session_id: session_id.into(),
            source_path: "$HOME/.claude/projects/foo/abc.jsonl".into(),
            assistant_turns: 100,
            user_turns: 80,
            total_records: 200,
            errors,
            samples: vec![super::super::record::ErrorSample {
                kind: "shell_exit_nonzero".into(),
                snippet: "Exit code 1".into(),
            }],
        }
    }

    /// Roundtrip: enqueue → list → read → assert shape preserved.
    /// Runs against a tempdir override of the pending dir so the
    /// test never touches the real ~/.krab/logs/. We can't easily
    /// override `pending_dir()` since it uses `dirs::home_dir()` —
    /// instead we set the `HOME` env var to a tempdir for the test
    /// duration. Note `dirs` reads HOME at every call so this is
    /// race-free per-test on Unix.
    #[test]
    fn roundtrip_through_pending_dir() {
        let tmp = tempfile_like_dir("krab-test-queue");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: tests in Rust are single-threaded per file by
        // default unless `cargo test --jobs N` is set high. The
        // queue tests don't share state with anything else here,
        // and we restore HOME at the end. If we ever go truly
        // parallel, swap this for a path-injecting trait.
        unsafe {
            std::env::set_var("HOME", &tmp);
        }

        // No pending records initially.
        assert!(list_pending().is_empty());

        let recs = vec![fake_record("aaa-001"), fake_record("aaa-002")];
        let path = enqueue(&recs).expect("enqueue ok");
        assert!(path.exists(), "enqueued file should exist: {path:?}");

        let listed = list_pending();
        assert_eq!(listed.len(), 1, "exactly one pending file");
        assert_eq!(listed[0], path);

        let read_back = read_records(&path).expect("read ok");
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[0].session_id, "aaa-001");
        assert_eq!(read_back[0].errors.get("shell_exit_nonzero"), Some(&3));
        assert_eq!(read_back[1].session_id, "aaa-002");

        // Cleanup — restore HOME.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `read_records` tolerates malformed lines (matches the
    /// scraper's resilience promise). One bad line in the file
    /// doesn't poison the upload — the good records still parse.
    #[test]
    fn read_records_skips_malformed_lines() {
        let tmp = tempfile_like_dir("krab-test-malformed");
        let bad_path = tmp.join("test.jsonl");
        std::fs::create_dir_all(&tmp).unwrap();
        let good = serde_json::to_string(&fake_record("ok-1")).unwrap();
        let contents = format!("not json\n{good}\n{{ broken\n");
        std::fs::write(&bad_path, contents).unwrap();

        let out = read_records(&bad_path).expect("read ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "ok-1");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `client_envelope` carries the right shape so the upstream
    /// envelope parser (in routes/telemetry.rs) recognises it.
    /// `krab_version` matches Cargo and `os` is non-empty.
    #[test]
    fn client_envelope_shape() {
        let e = client_envelope();
        assert_eq!(e.krab_version, env!("CARGO_PKG_VERSION"));
        assert!(!e.os.is_empty());
        assert!(
            e.os.contains('/'),
            "os field should be `<os>/<arch>`: {}",
            e.os
        );
    }

    /// Tempdir helper that doesn't pull in `tempfile` as a dep.
    /// Returns a brand-new dir under the system tmpdir.
    fn tempfile_like_dir(prefix: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        p.push(format!("{prefix}-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
