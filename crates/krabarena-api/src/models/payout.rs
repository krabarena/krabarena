use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Explicit column list for `SELECT`s against `payout_transactions`.
///
/// `context` is read as text (`context::text`) so we don't need the
/// sqlx `json` feature to decode JSONB — callers parse the string with
/// `serde_json` only when they need the structured fields.
pub const PAYOUT_SELECT: &str = "id, user_id, amount, currency, network, status, \
     event_type, subject_type, subject_id, context::text AS context, reason, \
     dedup_key, wallet_address, created_at, updated_at";

/// One row of the payout ledger. Deliberately decoupled from any single
/// earning scenario: the reason is described by `event_type` plus a
/// polymorphic (`subject_type`, `subject_id`) soft reference and a
/// `context` JSON blob.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PayoutRow {
    pub id: Uuid,
    pub user_id: Uuid,
    /// Whole Toncoins (BIGINT). Money elsewhere is integer too.
    pub amount: i64,
    pub currency: String,
    pub network: String,
    /// `pending` | `paid` | `rejected`.
    pub status: String,
    /// What earned the credit, e.g. `"claim_verified"`.
    pub event_type: String,
    /// Polymorphic target kind, e.g. `"claim"` (no FK).
    pub subject_type: Option<String>,
    /// Soft reference to the subject row.
    pub subject_id: Option<Uuid>,
    /// Raw JSON text (selected via `context::text`).
    pub context: String,
    pub reason: String,
    pub dedup_key: Option<String>,
    /// Snapshot of the destination address at credit time (may be NULL
    /// if the user had no wallet yet).
    pub wallet_address: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
