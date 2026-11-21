//! Generic payout ledger writes + read aggregates.
//!
//! `payout_transactions` is intentionally decoupled from any single
//! earning scenario (see migration 064 / `models::payout`). This service
//! is the one place that inserts a credit: callers describe *why* via an
//! `event_type` + polymorphic subject + a `context` blob, and that's it.
//! Adding a new kind of payout is a new `event_type` string and one
//! [`credit`] call from wherever the action happens — no schema change.
//!
//! All money movement and status transitions (pending → paid | rejected)
//! are performed MANUALLY by an admin via SQL; nothing here flips status.

use serde::Serialize;
use uuid::Uuid;

use crate::models::payout::{PAYOUT_SELECT, PayoutRow};

/// A request to credit one payout. Borrowed where cheap; `reason` /
/// `dedup_key` are owned because callers usually format them.
pub struct CreditRequest<'a> {
    pub user_id: Uuid,
    /// Whole Toncoins (> 0).
    pub amount: i64,
    pub event_type: &'a str,
    pub subject_type: Option<&'a str>,
    pub subject_id: Option<Uuid>,
    pub context: serde_json::Value,
    pub reason: String,
    /// Idempotency key. When set and a row with the same key already
    /// exists, the insert is a no-op and [`credit`] returns `Ok(None)`.
    pub dedup_key: Option<String>,
}

/// Insert a payout credit. Idempotent on `dedup_key` (returns `Ok(None)`
/// when the key already exists). The destination `wallet_address` is
/// snapshotted from the recipient's wallet at credit time (NULL if they
/// have none yet). Accepts any `PgExecutor`, so it can run inside a
/// caller's transaction (`&mut *tx`) or against a pool directly.
pub async fn credit<'e, E>(executor: E, req: CreditRequest<'_>) -> sqlx::Result<Option<PayoutRow>>
where
    E: sqlx::PgExecutor<'e>,
{
    let context_text = req.context.to_string();
    let sql = format!(
        "INSERT INTO payout_transactions \
           (user_id, amount, status, event_type, subject_type, subject_id, \
            context, reason, dedup_key, wallet_address) \
         VALUES ($1, $2, 'pending', $3, $4, $5, $6::jsonb, $7, $8, \
                 (SELECT address FROM wallets WHERE user_id = $1)) \
         ON CONFLICT (dedup_key) WHERE dedup_key IS NOT NULL DO NOTHING \
         RETURNING {PAYOUT_SELECT}"
    );
    sqlx::query_as::<_, PayoutRow>(&sql)
        .bind(req.user_id)
        .bind(req.amount)
        .bind(req.event_type)
        .bind(req.subject_type)
        .bind(req.subject_id)
        .bind(context_text)
        .bind(req.reason)
        .bind(req.dedup_key)
        .fetch_optional(executor)
        .await
}

/// Sum of credits already booked against a subject for one event type,
/// counting only `pending` + `paid` (so `rejected` frees the slot). Used
/// to enforce per-subject caps (e.g. ≤5 Toncoin per claim). Runs inside
/// the caller's transaction so a row lock on the subject serialises it.
pub async fn subject_credited_total<'e, E>(
    executor: E,
    event_type: &str,
    subject_type: &str,
    subject_id: Uuid,
) -> sqlx::Result<i64>
where
    E: sqlx::PgExecutor<'e>,
{
    let (sum,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(amount), 0)::bigint \
           FROM payout_transactions \
          WHERE event_type = $1 AND subject_type = $2 AND subject_id = $3 \
            AND status IN ('pending', 'paid')",
    )
    .bind(event_type)
    .bind(subject_type)
    .bind(subject_id)
    .fetch_one(executor)
    .await?;
    Ok(sum)
}

/// Total a user has *earned but not yet been paid* — `SUM(amount)` over
/// `pending` rows. Surfaced as "Earned" on the profile (grey).
pub async fn earned_pending(pool: &sqlx::PgPool, user_id: Uuid) -> sqlx::Result<i64> {
    sum_for_status(pool, user_id, "pending").await
}

/// Total a user has *been paid* — `SUM(amount)` over `paid` rows.
/// Surfaced as "Paid" on the profile (green).
pub async fn paid_total(pool: &sqlx::PgPool, user_id: Uuid) -> sqlx::Result<i64> {
    sum_for_status(pool, user_id, "paid").await
}

async fn sum_for_status(pool: &sqlx::PgPool, user_id: Uuid, status: &str) -> sqlx::Result<i64> {
    let (sum,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(amount), 0)::bigint \
           FROM payout_transactions WHERE user_id = $1 AND status = $2",
    )
    .bind(user_id)
    .bind(status)
    .fetch_one(pool)
    .await?;
    Ok(sum)
}

/// One row of a user's transaction history, joined to its claim (when the
/// subject is a claim) so the UI can link it. `verification_id` is pulled
/// straight out of `context` via the JSON `->>` operator (no sqlx `json`
/// feature needed).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TransactionItem {
    pub id: Uuid,
    pub amount: i64,
    pub currency: String,
    pub network: String,
    pub status: String,
    pub event_type: String,
    pub reason: String,
    pub wallet_address: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub subject_type: Option<String>,
    pub subject_id: Option<Uuid>,
    pub claim_slug: Option<String>,
    pub claim_title: Option<String>,
    pub verification_id: Option<String>,
}

/// A user's payouts, newest first, capped at `limit`.
pub async fn list_transactions(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    limit: i64,
) -> sqlx::Result<Vec<TransactionItem>> {
    sqlx::query_as::<_, TransactionItem>(
        "SELECT p.id, p.amount, p.currency, p.network, p.status, p.event_type, \
                p.reason, p.wallet_address, p.created_at, p.updated_at, \
                p.subject_type, p.subject_id, \
                c.slug AS claim_slug, c.claim AS claim_title, \
                (p.context ->> 'verification_id') AS verification_id \
           FROM payout_transactions p \
           LEFT JOIN claims c \
             ON p.subject_type = 'claim' AND p.subject_id = c.id \
          WHERE p.user_id = $1 \
          ORDER BY p.created_at DESC \
          LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}
