-- Migration 074: time-range index on user_logs for usage stats.
--
-- `GET /v1/admin/usage` (services/usage_stats.rs) scans user_logs by
-- `created_at` window; 069 only indexed (device_id, created_at) and
-- (voter_id, created_at), so every window query was a full scan of the
-- ~1.5M-row table. Index only — no column changes, so no
-- `cached plan must not change result type` risk. Idempotent
-- (`IF NOT EXISTS`); safe under SQLX_AUTO_MIGRATE. Plain (non-
-- CONCURRENT) build because sqlx runs each migration in a transaction;
-- user_logs inserts are fire-and-forget, so the brief write lock only
-- delays log rows.
--
-- NEVER edit this file after it has run in any environment (sqlx
-- checksums are immutable).

CREATE INDEX IF NOT EXISTS idx_user_logs_created_at ON user_logs (created_at);
