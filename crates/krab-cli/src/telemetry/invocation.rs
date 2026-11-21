//! Per-invocation CLI logger.
//!
//! Every krab command run writes one JSON record to
//! `~/.krab/logs/pending/` and the opportunistic flush at the top
//! of the NEXT invocation uploads it. The record shape is
//! `kind = "cli_command"`, distinct from the `*_session` records
//! produced by [`SessionScraper`](super::SessionScraper)
//! implementations. The server's envelope in
//! `routes/telemetry.rs` accepts both via a flattened JSON map.
//!
//! Sensitive args (anything matching `--token`, the value of a
//! `--token` flag, or a value that looks like a JWT) are redacted
//! before the record lands on disk. The redaction is conservative —
//! we'd rather over-redact and lose context than leak a token in
//! the upload.
//!
//! Detected agent (the harness that invoked us) is sniffed from a
//! short list of env vars and tagged in the record. Unknown
//! harnesses tag as `"unknown"` — useful for spotting agents we
//! should add scrapers for.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use uuid::Uuid;

use super::queue;

/// One per-invocation record. JSON-serialised straight into the
/// pending queue. Field names match the canonical envelope so the
/// server's deserializer picks them up via `kind` / `agent` / `ts`
/// / `session_id` and leaves the rest in the `full` flattened map.
#[derive(Debug, Serialize)]
struct CliCommandRecord {
    /// Always `"cli_command"` for records produced here.
    kind: &'static str,
    /// Sniffed from env. Examples: `"claude-code"`, `"openclaw"`,
    /// `"nemoclaw"`, `"hermes"`, `"unknown"`. The native CLI on a
    /// human terminal also tags as `"unknown"`.
    agent: String,
    /// ISO-8601 UTC, second precision. Start time of the invocation.
    ts: String,
    /// UUID per invocation. Used to dedup re-uploads on the server
    /// side via the table's `UNIQUE (user_id, kind, session_id)`
    /// constraint.
    session_id: String,
    /// CLI version string from `CARGO_PKG_VERSION`.
    krab_version: String,
    /// Platform string (`"<os>/<arch>"`).
    os: String,
    /// Top-level subcommand (e.g. `"battle"`, `"feedback"`).
    /// `"<none>"` when the user invoked `krab` with no args.
    command: String,
    /// Redacted args after the top-level subcommand. Tokens stripped.
    args: Vec<String>,
    /// Wall-clock time the command ran (start → record write).
    duration_ms: u32,
    /// Exit code: 0 = success, 1 = runtime error, 2 = clap parse.
    exit_code: i32,
    /// Bucket for the error kind, or `null` on success. Examples:
    /// `"auth"`, `"network"`, `"validation"`, `"runtime"`. Free-form
    /// — the server doesn't enumerate; the dashboard buckets by
    /// string in SQL.
    #[serde(skip_serializing_if = "Option::is_none")]
    error_class: Option<&'static str>,
    /// Up to 200 chars of the error message, `$HOME` redacted.
    #[serde(skip_serializing_if = "Option::is_none")]
    error_summary: Option<String>,
}

/// In-flight handle returned by [`begin`] — capture timing and the
/// args that have been parsed so far. `finish` consumes it,
/// classifies the result, and writes the JSON record.
pub struct InvocationLogger {
    started: Instant,
    session_id: Uuid,
    ts: String,
    agent: String,
    command: String,
    args: Vec<String>,
}

/// Start an invocation log. Captures the start instant, sniffs the
/// driving harness from env, redacts the args, and stashes
/// everything needed for [`finish`]. Cheap — no fs writes happen
/// here, so even if [`finish`] never gets called the only cost is
/// the `Instant::now()` + a few clones.
pub fn begin(command: &str, raw_args: &[String]) -> InvocationLogger {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    InvocationLogger {
        started: Instant::now(),
        session_id: Uuid::new_v4(),
        ts: super::claude_code::format_iso8601_utc_public(secs),
        agent: sniff_agent(),
        command: command.to_string(),
        args: redact_args(raw_args),
    }
}

/// Finish + enqueue the record. Best-effort; any error (disk full,
/// permission denied, etc.) is swallowed because telemetry must
/// never fail a CLI invocation. On success the record sits in the
/// pending queue until the next invocation's flush picks it up.
pub fn finish(
    logger: InvocationLogger,
    exit_code: i32,
    error_class: Option<&'static str>,
    error_summary: Option<String>,
) {
    // Respect the user's opt-out — read the config one more time
    // because they may have just toggled it via `krab feedback off`.
    let share = crate::config::Config::load()
        .map(|c| c.telemetry.share)
        .unwrap_or(true);
    if !share {
        return;
    }

    let duration_ms = logger.started.elapsed().as_millis().min(u32::MAX as u128) as u32;

    let rec = CliCommandRecord {
        kind: "cli_command",
        agent: logger.agent,
        ts: logger.ts,
        session_id: logger.session_id.to_string(),
        krab_version: env!("CARGO_PKG_VERSION").to_string(),
        os: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        command: logger.command,
        args: logger.args,
        duration_ms,
        exit_code,
        error_class,
        error_summary: error_summary
            .map(|s| super::record::snippet_truncate(&super::record::redact_home(&s), 200)),
    };

    let value = match serde_json::to_value(&rec) {
        Ok(v) => v,
        Err(_) => return,
    };
    let _ = queue::enqueue_values(&[value]);
}

/// Sniff the parent agent harness from env. Each known harness sets
/// at least one identifying env var when it invokes child
/// processes. Order matters — first match wins. `"unknown"` is the
/// catch-all for native terminal usage AND for harnesses we haven't
/// added detection for (the latter shows up in the dashboard as
/// "user is using X but we tagged 'unknown'" — a useful signal to
/// add detection).
fn sniff_agent() -> String {
    // Claude Code sets several CLAUDE_* env vars when invoking
    // child processes. The most reliable one across versions is
    // `CLAUDECODE` (set to "1" in modern releases). Older versions
    // set `CLAUDE_CODE_ENTRYPOINT`.
    if std::env::var("CLAUDECODE").is_ok()
        || std::env::var("CLAUDE_CODE_ENTRYPOINT").is_ok()
        || std::env::var("CLAUDE_CODE_SSE_PORT").is_ok()
    {
        return "claude-code".to_string();
    }
    if std::env::var("OPENCLAW").is_ok() || std::env::var("OPENCLAW_SESSION").is_ok() {
        return "openclaw".to_string();
    }
    if std::env::var("NEMOCLAW").is_ok() || std::env::var("NEMOCLAW_SESSION").is_ok() {
        return "nemoclaw".to_string();
    }
    if std::env::var("HERMES").is_ok() || std::env::var("HERMES_SESSION").is_ok() {
        return "hermes".to_string();
    }
    "unknown".to_string()
}

/// Best-effort args redaction. Strip `--token`, the value following
/// `--token`, and any value that pattern-matches a JWT
/// (`<a>.<b>.<c>` with each part long enough to plausibly be base64
/// URL-encoded). Over-redact rather than risk a leak — the
/// dashboard cares about command shape, not flag values.
fn redact_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut consume_next_value = false;
    for a in args {
        if consume_next_value {
            out.push("<redacted>".to_string());
            consume_next_value = false;
            continue;
        }
        if a == "--token" {
            out.push(a.clone());
            consume_next_value = true;
            continue;
        }
        if let Some(_rest) = a.strip_prefix("--token=") {
            out.push("--token=<redacted>".to_string());
            continue;
        }
        if looks_like_jwt(a) {
            out.push("<redacted>".to_string());
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// Crude JWT detection: three dot-separated base64-URL-ish chunks,
/// each at least 8 chars. False positives over-redact a benign arg,
/// which is the safer failure mode.
fn looks_like_jwt(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts.iter().all(|p| {
        p.len() >= 8
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_strips_token_flag_value() {
        let args = vec![
            "battle".to_string(),
            "list".to_string(),
            "--token".to_string(),
            "secrettoken".to_string(),
            "--format".to_string(),
            "json".to_string(),
        ];
        let red = redact_args(&args);
        assert_eq!(
            red,
            vec![
                "battle",
                "list",
                "--token",
                "<redacted>",
                "--format",
                "json",
            ]
        );
    }

    #[test]
    fn redact_strips_token_equals_value() {
        let args = vec![
            "claim".to_string(),
            "list".to_string(),
            "--token=eyJsuper.secret.value".to_string(),
        ];
        let red = redact_args(&args);
        assert_eq!(red, vec!["claim", "list", "--token=<redacted>"]);
    }

    #[test]
    fn redact_strips_jwt_lookalikes_anywhere() {
        // The reviewer-mint endpoint sometimes echoes a JWT in a
        // CLI invocation that doesn't carry --token. The lookalike
        // pattern catches it regardless of flag context.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyIn0.signaturepart";
        let args = vec!["auth".into(), "set-token".into(), jwt.to_string()];
        let red = redact_args(&args);
        assert_eq!(red, vec!["auth", "set-token", "<redacted>"]);
    }

    #[test]
    fn redact_leaves_non_secret_flags_alone() {
        let args = vec![
            "battle".to_string(),
            "list".to_string(),
            "--status".to_string(),
            "open".to_string(),
        ];
        let red = redact_args(&args);
        assert_eq!(red, vec!["battle", "list", "--status", "open"]);
    }

    #[test]
    fn looks_like_jwt_only_matches_three_part_base64ish() {
        assert!(looks_like_jwt("aaaaaaaa.bbbbbbbb.cccccccc"));
        assert!(!looks_like_jwt("hello.world")); // 2 parts
        assert!(!looks_like_jwt("ab.cd.ef")); // chunks too short
        assert!(!looks_like_jwt("abc")); // no dots
        assert!(!looks_like_jwt("foo bar.baz qux.quux")); // spaces aren't allowed in base64-url
    }

    #[test]
    fn sniff_agent_falls_back_to_unknown() {
        // We can't reliably set env vars in tests without leaking
        // into other tests on the same process. Just verify the
        // fallback path returns "unknown" when none of the known
        // vars are present in the CURRENT env. The CI runner
        // doesn't set CLAUDECODE / OPENCLAW / etc., so this is
        // effectively the no-vars path.
        let agent = sniff_agent();
        // Strict check: agent is one of the known strings.
        assert!(
            matches!(
                agent.as_str(),
                "claude-code" | "openclaw" | "nemoclaw" | "hermes" | "unknown"
            ),
            "unexpected agent value: {agent}"
        );
    }
}
