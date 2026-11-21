//! Audit log for outbound Gemini API calls.
//!
//! Backs the `gemini_calls` table (migration 026). One row per HTTP
//! attempt — retries of one logical call share a `request_id` so a
//! GROUP BY collapses them into per-call rollups.
//!
//! The write path is **fire-and-forget** via `tokio::spawn` — the
//! audit must never add latency to the review critical path, and an
//! audit-insert failure must never fail the review. If the DB is
//! down, the row is simply lost (Cloud Logging still captures the
//! `tracing::warn!` line from the retry helper).
//!
//! The pool is stashed in a process-wide `OnceLock` from `main.rs`
//! after `db::create_pool` runs. Unit tests that don't initialize it
//! get a no-op `record` — they never touch the DB.
//!
//! ## Reading the table
//!
//! Per-call rollup (success rate over a window):
//!
//! ```sql
//! WITH calls AS (
//!   SELECT request_id,
//!          MAX(attempt_no)              AS attempts,
//!          BOOL_OR(status = 'success')  AS ok
//!   FROM gemini_calls
//!   WHERE created_at > NOW() - INTERVAL '1 hour'
//!   GROUP BY request_id
//! )
//! SELECT COUNT(*) total,
//!        COUNT(*) FILTER (WHERE ok)::float / COUNT(*) AS success_rate
//! FROM calls;
//! ```
//!
//! Per-attempt mix (where the retry budget is going):
//!
//! ```sql
//! SELECT status, error_kind, COUNT(*)
//! FROM gemini_calls
//! WHERE created_at > NOW() - INTERVAL '1 hour'
//! GROUP BY 1, 2 ORDER BY 3 DESC;
//! ```

use std::sync::OnceLock;

use sqlx::PgPool;
use uuid::Uuid;

/// Initialized once from `main.rs` after the pool is up. Reads via
/// `pool()` clone-and-return; writes (`init`) are one-shot.
static AUDIT_POOL: OnceLock<PgPool> = OnceLock::new();

/// Install the process-wide pool used for `record`. Idempotent in
/// practice (the second `init_pool` call from a misconfigured caller
/// is silently ignored). Called from `main.rs::main` once.
pub fn init_pool(pool: PgPool) {
    let _ = AUDIT_POOL.set(pool);
}

fn pool() -> Option<&'static PgPool> {
    AUDIT_POOL.get()
}

/// Status codes recorded on the row. Mirrors the comment at the head
/// of migration 026.
pub mod status {
    /// 2xx, returned to caller. Always the final attempt for a
    /// successful logical call.
    pub const SUCCESS: &str = "success";
    /// Non-terminal: 503/429/connect/timeout that triggered another
    /// retry. Never the last row in a request_id group.
    pub const TRANSIENT_RETRY: &str = "transient_retry";
    /// Retries used up while still seeing 503/429. Terminal.
    pub const EXHAUSTED: &str = "exhausted";
    /// 4xx (or non-503/429 5xx) — not retried, returned. Terminal.
    pub const NON_2XX_TERMINAL: &str = "non_2xx_terminal";
    /// `reqwest::Error` not classified as connect/timeout (TLS
    /// handshake, dns, etc.). Terminal — we don't retry these.
    pub const TRANSPORT_ERROR: &str = "transport_error";
}

/// Insert a single attempt row. Fire-and-forget — spawns a task so
/// the caller doesn't await the DB round-trip. Errors are swallowed
/// with a `tracing::error!` so the audit can never break a review.
#[allow(clippy::too_many_arguments)]
pub fn record(
    request_id: Uuid,
    attempt_no: i32,
    purpose: &'static str,
    model: String,
    status: &'static str,
    http_status: Option<i32>,
    error_kind: Option<&'static str>,
    error_msg: Option<String>,
    latency_ms: i32,
) {
    let Some(pool) = pool() else {
        // Pool not initialized (unit tests, or main forgot to wire
        // it). Silent no-op — we don't want to spam stderr on every
        // call.
        return;
    };
    let pool = pool.clone();
    // Truncate the error body — full text lives in Cloud Logging
    // already, the DB row just needs enough to filter on.
    // Byte-slicing here panicked when byte 500 landed inside a
    // multi-byte char in an upstream error body — use the
    // char-counting helper instead of `&m[..500]`.
    let error_msg_trunc =
        error_msg.map(|m| crate::services::presentation_check::truncate_chars(&m, 500));
    tokio::spawn(async move {
        let res = sqlx::query(
            "INSERT INTO gemini_calls \
             (request_id, attempt_no, purpose, model, status, http_status, error_kind, error_msg, latency_ms) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(request_id)
        .bind(attempt_no)
        .bind(purpose)
        .bind(model)
        .bind(status)
        .bind(http_status)
        .bind(error_kind)
        .bind(error_msg_trunc)
        .bind(latency_ms)
        .execute(&pool)
        .await;
        if let Err(e) = res {
            tracing::error!(
                event = "gemini_audit.insert_failed",
                error = %e,
                "could not record gemini_calls row"
            );
        }
    });
}
