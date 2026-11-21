//! `/v1/telemetry/*` — CLI telemetry inbox.
//!
//! Companion to migration 027 (`cli_telemetry` table). The CLI side
//! (krab-cli's `telemetry` module) builds the payload; this route
//! lands it on disk. Best-effort: a bad row in the batch doesn't
//! fail the whole upload — we count successes and report. Failures
//! also don't reach the client beyond a count, so a partially-bad
//! batch doesn't make the CLI think it must re-upload everything.
//!
//! Dedup is built into the table via `UNIQUE (user_id, kind,
//! session_id)` + `ON CONFLICT DO NOTHING`, so re-POSTing the same
//! record (which happens whenever the on-disk queue gets retried)
//! is a no-op on the server side.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::Value;

use crate::AppState;
use crate::envelope::ApiResponse;
use crate::error::ApiError;
use crate::routes::auth::extract_user_id;

/// `POST /v1/telemetry/cli`
///
/// Authenticated. Accepts a batch of CLI telemetry records. Each
/// record carries enough metadata for the server to (a) dedupe
/// re-uploads, (b) slice by kind / agent / time, and (c) keep the
/// full payload around as raw JSON for analyses we haven't thought
/// of yet.
pub async fn ingest_cli(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<IngestRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let user_id = extract_user_id(&headers, &state.jwt_secret)?;

    // Cap the batch size so a misbehaving client can't dump a giant
    // payload. 200 records × ~10 KB each ≈ 2 MB body — well under
    // the axum 2 MiB default body cap, but bounded here so we don't
    // surprise ourselves later.
    if req.records.len() > 200 {
        return Err(ApiError::ValidationError(format!(
            "too many records in one batch: {} > 200",
            req.records.len()
        )));
    }

    let krab_version = req.client.as_ref().and_then(|c| c.krab_version.clone());
    let os = req.client.as_ref().and_then(|c| c.os.clone());

    let mut accepted = 0u32;
    let mut deduped = 0u32;
    let mut rejected = 0u32;

    for rec in req.records {
        // Pull the must-have fields out of the record envelope. Any
        // missing/malformed field is a soft reject — we count it and
        // move on to the next record so one bad row can't kill the
        // batch.
        let Some(kind) = rec.kind.clone() else {
            rejected += 1;
            continue;
        };
        let Some(agent) = rec.agent.clone() else {
            rejected += 1;
            continue;
        };
        let Some(session_id) = rec.session_id.clone() else {
            rejected += 1;
            continue;
        };
        let Some(ts) = rec.ts.clone().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .ok()
                .map(|d| d.with_timezone(&chrono::Utc))
        }) else {
            rejected += 1;
            continue;
        };
        let raw_json = serde_json::to_value(&rec.full).unwrap_or(Value::Null);

        let result = sqlx::query(
            "INSERT INTO cli_telemetry \
                (user_id, kind, agent, ts, session_id, krab_version, os, raw_json) \
              VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
              ON CONFLICT (user_id, kind, session_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(&kind)
        .bind(&agent)
        .bind(ts)
        .bind(&session_id)
        .bind(krab_version.as_deref())
        .bind(os.as_deref())
        .bind(&raw_json)
        .execute(&state.pool)
        .await;

        match result {
            Ok(r) if r.rows_affected() == 1 => accepted += 1,
            Ok(_) => deduped += 1,
            Err(e) => {
                // Don't fail the whole request — count it and move on.
                tracing::warn!(
                    event = "cli_telemetry.insert_failed",
                    error = %e,
                    user_id = %user_id,
                    kind = %kind,
                    session_id = %session_id,
                    "cli_telemetry insert failed"
                );
                rejected += 1;
            }
        }
    }

    Ok((
        StatusCode::OK,
        Json(ApiResponse::new(serde_json::json!({
            "accepted": accepted,
            "deduped":  deduped,
            "rejected": rejected,
        }))),
    ))
}

/// Wire shape for `POST /v1/telemetry/cli`. The CLI builds this
/// exactly via `crate::commands::feedback::ingest_sessions_dry_run`.
#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    pub client: Option<ClientEnvelope>,
    pub records: Vec<RecordEnvelope>,
}

#[derive(Debug, Deserialize)]
pub struct ClientEnvelope {
    pub krab_version: Option<String>,
    pub os: Option<String>,
}

/// Loose envelope: we pull a few canonical fields by name but pass
/// the entire record through as raw JSON. The CLI's
/// `TelemetryRecord` is a stricter typed shape; the server accepts
/// extras so a CLI ahead of the server doesn't get records rejected
/// for adding a new field.
#[derive(Debug, Deserialize)]
pub struct RecordEnvelope {
    pub kind: Option<String>,
    pub agent: Option<String>,
    pub ts: Option<String>,
    pub session_id: Option<String>,
    /// Everything else (counts, samples, etc.). Captured into the
    /// JSONB column verbatim.
    #[serde(flatten)]
    pub full: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_deserializes_canonical_record() {
        let raw = r#"{
            "client": {"krab_version": "0.8.28", "os": "darwin/aarch64"},
            "records": [
                {
                    "kind": "claude_code_session",
                    "agent": "claude-code",
                    "ts": "2026-05-11T21:00:00Z",
                    "session_id": "abc-123",
                    "assistant_turns": 1500,
                    "user_turns": 800,
                    "total_records": 3000,
                    "errors": {"shell_exit_nonzero": 32},
                    "samples": [{"kind": "shell_exit_nonzero", "snippet": "Exit code 1"}]
                }
            ]
        }"#;
        let parsed: IngestRequest = serde_json::from_str(raw).expect("parses");
        assert_eq!(parsed.records.len(), 1);
        let r = &parsed.records[0];
        assert_eq!(r.kind.as_deref(), Some("claude_code_session"));
        assert_eq!(r.agent.as_deref(), Some("claude-code"));
        assert_eq!(r.session_id.as_deref(), Some("abc-123"));
        // The full body carries the counts via the flattened map.
        assert!(r.full.contains_key("assistant_turns"));
        assert!(r.full.contains_key("errors"));
    }

    #[test]
    fn envelope_rejects_missing_kind_at_write_time() {
        // The deserialize itself is permissive (kind = None is fine);
        // the route counts it as `rejected` and skips the INSERT.
        let raw = r#"{
            "records": [
                {"agent": "x", "ts": "2026-05-11T00:00:00Z", "session_id": "s"}
            ]
        }"#;
        let parsed: IngestRequest = serde_json::from_str(raw).expect("parses");
        assert!(parsed.records[0].kind.is_none());
    }
}
