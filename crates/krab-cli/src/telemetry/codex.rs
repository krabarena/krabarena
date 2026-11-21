//! Codex (OpenAI codex_cli_rs) session scraper.
//!
//! Codex writes one JSONL per session at
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`. Records
//! are `{timestamp, type, payload}` with two top-level types we care
//! about:
//!
//!   - `response_item` — the model's per-turn output. `payload.type`
//!     fans out into `message` (with `role: assistant|user|developer|
//!     system`), `reasoning`, `function_call`, `function_call_output`,
//!     `web_search_call`.
//!   - `event_msg` — runtime events (token_count, agent_message,
//!     user_message, task_started, task_complete, web_search_end).
//!     We use `user_message` events as a backup user-turn count for
//!     sessions that don't reflect user input via `message` records.
//!
//! Errors live in `payload.type == "function_call_output"` whose
//! `payload.output` is a free-form string. Codex's typical exec
//! wrapper prints "Process exited with code N" + the captured stdout/
//! stderr, so we classify on that text the same way the Claude Code
//! scraper does — exit-code shells get `shell_exit_nonzero`, with the
//! `python_parse` and `permission_gate` sub-buckets matching the same
//! string patterns.
//!
//! Adding `codex_session` doesn't need a server schema change —
//! `cli_telemetry.kind` is free-form TEXT (migration 027) and the
//! ingest endpoint dedupes on `(user_id, kind, session_id)`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use super::claude_code::format_iso8601_utc_public;
use super::record::{ErrorSample, TelemetryRecord, redact_home, snippet_truncate};
use super::scraper::{SessionScraper, buckets};

/// Same 50 MB cap the Claude-Code scraper uses.
const MAX_BYTES_PER_FILE: u64 = 50 * 1024 * 1024;
const SNIPPET_MAX_CHARS: usize = 160;

pub struct CodexScraper;

impl SessionScraper for CodexScraper {
    fn agent(&self) -> &'static str {
        "codex"
    }

    fn default_paths(&self) -> Vec<PathBuf> {
        // Walk `~/.codex/sessions/YYYY/MM/DD/*.jsonl` — three layers
        // deep. We don't recurse arbitrarily; that's a deliberate
        // bound on how much disk we'll touch (and matches how Codex
        // itself lays the files out).
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        let root = home.join(".codex").join("sessions");
        let mut paths = Vec::new();
        let Ok(year_dirs) = fs::read_dir(&root) else {
            return Vec::new();
        };
        for year in year_dirs.flatten() {
            let yp = year.path();
            if !yp.is_dir() {
                continue;
            }
            let Ok(month_dirs) = fs::read_dir(&yp) else {
                continue;
            };
            for month in month_dirs.flatten() {
                let mp = month.path();
                if !mp.is_dir() {
                    continue;
                }
                let Ok(day_dirs) = fs::read_dir(&mp) else {
                    continue;
                };
                for day in day_dirs.flatten() {
                    let dp = day.path();
                    if !dp.is_dir() {
                        continue;
                    }
                    let Ok(files) = fs::read_dir(&dp) else {
                        continue;
                    };
                    for file in files.flatten() {
                        let p = file.path();
                        if p.extension().and_then(|s| s.to_str()) == Some("jsonl") && p.is_file() {
                            paths.push(p);
                        }
                    }
                }
            }
        }
        paths
    }

    fn parse(&self, path: &Path) -> anyhow::Result<Vec<TelemetryRecord>> {
        let file = fs::File::open(path)?;
        let meta = file.metadata()?;
        let limited = file.take(MAX_BYTES_PER_FILE);
        let reader = BufReader::new(limited);
        let scan = scan_jsonl(reader);

        let source_path = redact_home(&path.to_string_lossy());
        // Codex session ids are baked into the filename:
        // `rollout-<ts>-<uuid>.jsonl`. Use the full stem as a stable
        // dedup key so re-ingest is idempotent.
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| source_path.clone());

        let ts = mtime_iso8601(&meta);

        Ok(vec![TelemetryRecord {
            kind: "codex_session".into(),
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

struct Scan {
    assistant_turns: u32,
    user_turns: u32,
    total_records: u32,
    errors: BTreeMap<String, u32>,
    samples: Vec<ErrorSample>,
}

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
        let top = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
        let payload = v.get("payload");
        let p_type = payload
            .and_then(|p| p.get("type"))
            .and_then(|s| s.as_str())
            .unwrap_or("");

        match (top, p_type) {
            ("response_item", "message") => {
                let role = payload
                    .and_then(|p| p.get("role"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                match role {
                    "assistant" => assistant_turns += 1,
                    "user" => user_turns += 1,
                    _ => {} // developer / system — instructions, not turns
                }
            }
            ("event_msg", "user_message") => {
                // Backup user-turn signal: Codex emits this for the
                // raw human-typed input even when there's no matching
                // `response_item.message`.
                user_turns += 1;
            }
            ("response_item", "function_call_output") => {
                let output = payload
                    .and_then(|p| p.get("output"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !is_error_output(output) {
                    continue;
                }
                let bucket = classify(output);
                *errors.entry(bucket.to_string()).or_insert(0) += 1;
                if sample_seen.insert(bucket.to_string()) {
                    samples.push(ErrorSample {
                        kind: bucket.to_string(),
                        snippet: snippet_truncate(&redact_home(output), SNIPPET_MAX_CHARS),
                    });
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

/// Codex's exec wrapper emits "Process exited with code 0" even on
/// success. We only count outputs whose body contains a non-zero exit
/// marker or an explicit error/permission string — otherwise every
/// successful command would be a fake error.
fn is_error_output(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    if text.contains("Process exited with code 0") || text.contains("exit code 0") {
        return false;
    }
    let lower = text.to_lowercase();
    text.contains("Process exited with code ")
        || text.contains("Exit code ")
        || lower.contains("permission") && lower.contains("denied")
        || lower.contains("traceback")
        || lower.contains("error: ")
        || (lower.contains("\"error\"") && lower.contains(":"))
}

fn classify(text: &str) -> &'static str {
    if text.contains("has not been read") {
        buckets::EDIT_BEFORE_READ
    } else if text.contains("Traceback") || text.contains("parse error") {
        buckets::PYTHON_PARSE
    } else if text.contains("Permission") && text.to_lowercase().contains("denied") {
        buckets::PERMISSION_GATE
    } else if text.contains("Process exited with code ") || text.contains("Exit code ") {
        buckets::SHELL_EXIT_NONZERO
    } else {
        buckets::OTHER
    }
}

fn mtime_iso8601(meta: &fs::Metadata) -> String {
    let Ok(mtime) = meta.modified() else {
        return String::new();
    };
    let Ok(dur) = mtime.duration_since(UNIX_EPOCH) else {
        return String::new();
    };
    format_iso8601_utc_public(dur.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const FIXTURE: &str = concat!(
        // session_meta — not a turn, total_records bumps by 1
        r#"{"timestamp":"2026-01-22T15:24:34Z","type":"session_meta","payload":{"id":"abc"}}"#,
        "\n",
        // assistant turn
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello"}]}}"#,
        "\n",
        // user turn via response_item.message
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"What's next?"}]}}"#,
        "\n",
        // user turn via event_msg.user_message — second user signal
        r#"{"type":"event_msg","payload":{"type":"user_message","message":"Follow up"}}"#,
        "\n",
        // developer message — should NOT count as user/assistant
        r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<perms>"}]}}"#,
        "\n",
        // function_call_output, success → ignored
        r#"{"type":"response_item","payload":{"type":"function_call_output","output":"Process exited with code 0\nok"}}"#,
        "\n",
        // function_call_output, error with Traceback → python_parse
        r#"{"type":"response_item","payload":{"type":"function_call_output","output":"Process exited with code 1\nTraceback (most recent call last):\n  File \"<string>\""}}"#,
        "\n",
        // function_call_output, error generic exit → shell_exit_nonzero
        r#"{"type":"response_item","payload":{"type":"function_call_output","output":"Process exited with code 2\nnpm ERR! missing script"}}"#,
        "\n",
        // function_call_output, permission denied → permission_gate
        r#"{"type":"response_item","payload":{"type":"function_call_output","output":"Permission for this action has been denied. Reason: prod write"}}"#,
        "\n",
        // function_call_output, JSON error body → other
        r#"{"type":"response_item","payload":{"type":"function_call_output","output":"{\"error\":\"network unreachable\"}"}}"#,
        "\n",
    );

    #[test]
    fn scan_counts_match_fixture() {
        let scan = scan_jsonl(Cursor::new(FIXTURE));
        assert_eq!(scan.assistant_turns, 1, "one assistant message");
        assert_eq!(
            scan.user_turns, 2,
            "one message+role=user + one event_msg user_message"
        );
        assert_eq!(scan.errors.get(buckets::PYTHON_PARSE), Some(&1));
        assert_eq!(scan.errors.get(buckets::SHELL_EXIT_NONZERO), Some(&1));
        assert_eq!(scan.errors.get(buckets::PERMISSION_GATE), Some(&1));
        assert_eq!(scan.errors.get(buckets::OTHER), Some(&1));
        // exit-code-0 is ignored.
        assert_eq!(scan.errors.values().sum::<u32>(), 4);
    }

    #[test]
    fn exit_code_zero_is_not_an_error() {
        assert!(!is_error_output("Process exited with code 0\nfoo"));
        assert!(!is_error_output(""));
        assert!(is_error_output("Process exited with code 1\nbar"));
        assert!(is_error_output(
            "Permission for this action has been denied"
        ));
    }

    #[test]
    fn classifier_handles_codex_shapes() {
        assert_eq!(
            classify("Traceback (most recent call last)..."),
            buckets::PYTHON_PARSE
        );
        assert_eq!(
            classify("Process exited with code 2\nls: not found"),
            buckets::SHELL_EXIT_NONZERO
        );
        assert_eq!(
            classify("Permission for this action has been denied"),
            buckets::PERMISSION_GATE
        );
        assert_eq!(
            classify("{\"error\":\"network unreachable\"}"),
            buckets::OTHER
        );
    }
}
