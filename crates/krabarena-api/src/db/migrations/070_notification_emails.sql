-- Migration 070: instant notification emails (since 0.43.0).
--
-- Two NEW tables; no existing table is altered, so this migration is
-- free of the `0A000: cached plan must not change result type` trap
-- (AGENTS.md "Database") — no `SELECT *` plan anywhere changes shape.
-- Fully idempotent (`IF NOT EXISTS` everywhere).
--
-- 1. `user_notification_prefs` — per-user switch for instant
--    notification emails. Absence of a row = default ON. Kept separate
--    from `user_email_preferences` (migration 017) so we don't have to
--    widen that table's `digest_frequency` CHECK constraint, and so the
--    external KrabAgent digest cron that reads `user_email_preferences`
--    sees no contract change. `digest_frequency = 'off'` still
--    suppresses instant emails too (it means "no KrabArena email").
--
-- 2. `notification_email_log` — one row per send attempt
--    (`sent` / `throttled` / `failed`). Source of truth for the
--    throttle (one email per user+kind+target per 10 min, max 10 per
--    user per hour — see `services/notification_email.rs`) and an
--    audit trail of what we emailed whom.
--
-- Must be applied BEFORE the 0.43.0 code takes traffic (the deploy's
-- `SQLX_AUTO_MIGRATE=true` does this on startup; manual alternative:
--   /opt/homebrew/opt/libpq/bin/psql "$DATABASE_URL" \
--     -f crates/krabarena-api/src/db/migrations/070_notification_emails.sql
-- ). Without it, instant emails fail (logged, fire-and-forget) and the
-- preferences page falls back to "instant emails on".
--
-- NEVER edit this file after it has run in any environment (sqlx
-- checksums are immutable).

CREATE TABLE IF NOT EXISTS user_notification_prefs (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    instant_email BOOLEAN NOT NULL DEFAULT TRUE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS notification_email_log (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    target_id UUID NOT NULL,
    event_id UUID NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('sent', 'throttled', 'failed')),
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_notification_email_log_user_created
    ON notification_email_log (user_id, created_at DESC);
