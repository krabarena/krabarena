//! Claude Code session scraper.
//!
//! Claude Code writes one JSONL per session at
//! `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`. The file
//! is a stream of `{type, …}` records — `assistant`, `user`,
//! `tool_use`, `attachment`, etc. We care about:
//!
//!   - assistant turn count (iteration depth)
//!   - user turn count
//!   - tool_result errors carried inside `user`-type records:
//!     `message.content[].type == "tool_result"` with
//!     `is_error == true`
//!
//! Errors get bucketed by string-matching the error text:
//!
//!   - "has not been read"           → `edit_before_read`
//!   - starts-with "Exit code"        → `shell_exit_nonzero`
//!     (sub-classified to `python_parse` when the body contains
//!     `Traceback`/`parse error` so the python-c shape stands out)
//!   - contains "Permission" + "denied" → `permission_gate`
//!   - everything else recognised as an error → `other`
//!
//! These are the four buckets the real ingest in this conversation
//! produced: 106 / 44 / 8 / 84 across a 14-day, 35k-record session.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use super::record::{ErrorSample, TelemetryRecord, redact_home, snippet_truncate};
use super::scraper::{SessionScraper, buckets};

/// Bound on JSONL bytes we'll buffer per session. Real sessions on
/// this box go past 200 MB; we don't need to read the whole file to
/// produce a useful summary. Set to 50 MB which covers ~weeks of
/// activity for normal sessions.
const MAX_BYTES_PER_FILE: u64 = 50 * 1024 * 1024;

/// Hard cap on snippet length per error sample — see
/// [`record::snippet_truncate`].
const SNIPPET_MAX_CHARS: usize = 160;

pub struct ClaudeCodeScraper;

impl SessionScraper for ClaudeCodeScraper {
    fn agent(&self) -> &'static str {
        "claude-code"
    }

    fn default_paths(&self) -> Vec<PathBuf> {
        // `~/.claude/projects/*/*.jsonl` — one subdir per cwd-encoded
        // project, one jsonl per session. We return ALL of them; the
        // dispatcher parses each, the parser drops malformed lines.
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        let projects = home.join(".claude").join("projects");
        let Ok(project_dirs) = fs::read_dir(&projects) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for entry in project_dirs.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Ok(files) = fs::read_dir(&path) else {
                continue;
            };
            for file in files.flatten() {
                let p = file.path();
                if p.extension().and_then(|s| s.to_str()) == Some("jsonl") && p.is_file() {
                    paths.push(p);
                }
            }
        }
        paths
    }

    fn parse(&self, path: &Path) -> anyhow::Result<Vec<TelemetryRecord>> {
        let file = fs::File::open(path)?;
        let meta = file.metadata()?;
        // Cap the bytes we'll ingest from a single session file —
        // see `MAX_BYTES_PER_FILE`. `Read::take` returns a limited
        // adapter; wrap THAT in a `BufReader` so line splitting
        // still works.
        let limited = file.take(MAX_BYTES_PER_FILE);
        let reader = BufReader::new(limited);
        let scan = scan_jsonl(reader);

        // Source path: redacted-home + a single record for this file.
        let source_path = redact_home(&path.to_string_lossy());

        // Session id: filename stem. Falls back to the redacted source
        // path so we still get a stable dedup key on the server.
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| source_path.clone());

        // Timestamp: file mtime in UTC. We don't try to read the
        // first record's timestamp because the schema varies and the
        // mtime is a safe upper bound for "when this session was
        // last touched".
        let ts = mtime_iso8601(&meta);

        Ok(vec![TelemetryRecord {
            kind: "claude_code_session".into(),
            agent: self.agent().into(),
            ts,
            session_id,
            source_path,
            assistant_turns: scan.assistant_turns,
            user_turns: scan.user_turns,
            total_records: scan.total_records,
            errors: scan.errors,
            samples: scan.samples,
        }])
    }
}

/// Result of streaming one Claude Code JSONL. Internal — the public
/// surface is the [`TelemetryRecord`] the scraper returns.
struct Scan {
    assistant_turns: u32,
    user_turns: u32,
    total_records: u32,
    errors: BTreeMap<String, u32>,
    samples: Vec<ErrorSample>,
}

/// Stream `reader` line-by-line, count + bucket. Tolerant of
/// malformed lines (silently skipped). At most one sample is kept
/// per bucket — that's all the upload carries.
fn scan_jsonl<R: BufRead>(reader: R) -> Scan {
    let mut assistant_turns = 0u32;
    let mut user_turns = 0u32;
    let mut total_records = 0u32;
    let mut errors: BTreeMap<String, u32> = BTreeMap::new();
    let mut sample_seen: std::collections::BTreeSet<String> = Default::default();
    let mut samples: Vec<ErrorSample> = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        total_records += 1;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => assistant_turns += 1,
            Some("user") => {
                user_turns += 1;
                // Tool errors land here as content[].type=tool_result
                // with is_error=true.
                let content = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array());
                if let Some(arr) = content {
                    for item in arr {
                        if item.get("type").and_then(|s| s.as_str()) != Some("tool_result") {
                            continue;
                        }
                        if item.get("is_error").and_then(|b| b.as_bool()) != Some(true) {
                            continue;
                        }
                        let text = extract_tool_result_text(item);
                        let bucket = classify(&text);
                        *errors.entry(bucket.to_string()).or_insert(0) += 1;
                        if sample_seen.insert(bucket.to_string()) {
                            samples.push(ErrorSample {
                                kind: bucket.to_string(),
                                snippet: snippet_truncate(&redact_home(&text), SNIPPET_MAX_CHARS),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Scan {
        assistant_turns,
        user_turns,
        total_records,
        errors,
        samples,
    }
}

/// Pull the text body out of a `tool_result` content item. The
/// shape is either `"content": "<string>"` (older logs) or
/// `"content": [{"type":"text", "text":"…"}]` (newer logs). Both
/// are handled.
fn extract_tool_result_text(item: &Value) -> String {
    let c = item.get("content");
    if let Some(s) = c.and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = c.and_then(|v| v.as_array()) {
        let mut out = String::new();
        for part in arr {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
        return out;
    }
    String::new()
}

/// Bucket an error string. See module docs for the rule.
fn classify(text: &str) -> &'static str {
    if text.contains("has not been read") {
        buckets::EDIT_BEFORE_READ
    } else if text.contains("Exit code") {
        if text.contains("Traceback") || text.contains("parse error") {
            buckets::PYTHON_PARSE
        } else {
            buckets::SHELL_EXIT_NONZERO
        }
    } else if text.contains("Permission") && text.to_lowercase().contains("denied") {
        buckets::PERMISSION_GATE
    } else {
        buckets::OTHER
    }
}

/// File mtime as an ISO-8601 UTC string. Falls back to the empty
/// string if the mtime can't be read (we don't fail the whole record
/// over a clock-edge case).
fn mtime_iso8601(meta: &fs::Metadata) -> String {
    let Ok(mtime) = meta.modified() else {
        return String::new();
    };
    let Ok(dur) = mtime.duration_since(UNIX_EPOCH) else {
        return String::new();
    };
    let secs = dur.as_secs() as i64;
    format_iso8601_utc(secs)
}

/// Re-export of the in-module ISO-8601 formatter for use from
/// outside this file. Mirrors [`format_iso8601_utc`] verbatim.
pub fn format_iso8601_utc_public(unix_secs: i64) -> String {
    format_iso8601_utc(unix_secs)
}

/// Civil-date formatter, second precision, UTC. Vendored to avoid
/// a chrono dep on the CLI for this single use. Handles years
/// 1970..=2106 — plenty for any agent-session timestamp.
fn format_iso8601_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    let h = (rem / 3600) as u32;
    let m = ((rem % 3600) / 60) as u32;
    let s = (rem % 60) as u32;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Days-since-epoch → (year, month, day). Algorithm from Howard
/// Hinnant's date library (public domain).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Fake mini JSONL covering the four real-world buckets the live
    /// session produced. Used to validate the classifier and the
    /// counts end-to-end.
    const FIXTURE: &str = concat!(
        r#"{"type":"assistant"}"#,
        "\n",
        r#"{"type":"assistant"}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"<tool_use_error>File has not been read yet. Read it first before writing to it.</tool_use_error>"}]}}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"Exit code 1\n(eval):1: parse error near '>'"}]}}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"Exit code 1\nTraceback (most recent call last):\n  File \"<string>\", line 1, in <module>"}]}}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"Permission for this action has been denied. Reason: prod write"}]}}"#,
        "\n",
        r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"some weird new failure shape"}]}}"#,
        "\n",
        // Newer log shape — content is an array of {type:text, text:…}
        r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":[{"type":"text","text":"<tool_use_error>File has not been read yet.</tool_use_error>"}]}]}}"#,
        "\n",
        r#"this line is not json and should be ignored"#,
        "\n",
    );

    #[test]
    fn classifier_buckets_every_known_shape() {
        assert_eq!(
            classify("<tool_use_error>File has not been read yet.</tool_use_error>"),
            buckets::EDIT_BEFORE_READ
        );
        assert_eq!(
            classify("Exit code 1\n(eval):1: parse error"),
            buckets::PYTHON_PARSE
        );
        assert_eq!(
            classify("Exit code 1\nTraceback ..."),
            buckets::PYTHON_PARSE
        );
        assert_eq!(
            classify("Exit code 1\nnpm ERR! missing script"),
            buckets::SHELL_EXIT_NONZERO
        );
        assert_eq!(
            classify("Permission for this action has been denied."),
            buckets::PERMISSION_GATE
        );
        assert_eq!(classify("some weird new failure"), buckets::OTHER);
    }

    #[test]
    fn scan_counts_match_fixture() {
        let scan = scan_jsonl(Cursor::new(FIXTURE));
        assert_eq!(scan.assistant_turns, 2);
        assert_eq!(scan.user_turns, 6);
        // edit_before_read fires twice (string content + array
        // content shapes), python_parse twice, permission_gate
        // once, other once. shell_exit_nonzero should NOT appear
        // because the `Exit code` lines we have also contain
        // Traceback / parse-error markers.
        assert_eq!(scan.errors.get(buckets::EDIT_BEFORE_READ), Some(&2));
        assert_eq!(scan.errors.get(buckets::PYTHON_PARSE), Some(&2));
        assert_eq!(scan.errors.get(buckets::PERMISSION_GATE), Some(&1));
        assert_eq!(scan.errors.get(buckets::OTHER), Some(&1));
        assert!(!scan.errors.contains_key(buckets::SHELL_EXIT_NONZERO));
        // One sample per bucket that fired — 4 in this fixture.
        let sample_kinds: Vec<&str> = scan.samples.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(sample_kinds.len(), 4);
        // Every snippet is below the 160-char cap.
        for s in &scan.samples {
            assert!(
                s.snippet.chars().count() <= SNIPPET_MAX_CHARS + 1,
                "{:?}",
                s
            );
        }
    }

    #[test]
    fn iso8601_round_trips_known_epoch() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso8601_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        // Future-proof sanity: 2050.
        let s = format_iso8601_utc(2_524_608_000);
        assert!(s.starts_with("2050-01-01"), "{s}");
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let scan = scan_jsonl(Cursor::new("not json\n{\"type\":\"assistant\"}\nbroken{\n"));
        assert_eq!(scan.assistant_turns, 1);
        // total_records counts EVERY line, valid or not — it's our
        // "rough size" proxy.
        assert_eq!(scan.total_records, 3);
    }
}
