-- Migration 045: add result_token for auth polling flows.
--
-- Email magic-link auth and CLI device auth both verify in the browser,
-- store a short-lived bearer token on the pending verification row, then
-- let the installer/CLI poll until the token is available. The code has
-- referenced this column since the zero-copy auth flow, but the schema
-- migration was missing.

ALTER TABLE pending_verifications
    ADD COLUMN IF NOT EXISTS result_token TEXT;

CREATE INDEX IF NOT EXISTS idx_pending_verifications_email_result
    ON pending_verifications (identifier, created_at DESC)
    WHERE verification_type = 'email'
      AND result_token IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_pending_verifications_device_result
    ON pending_verifications (code)
    WHERE verification_type = 'device'
      AND result_token IS NOT NULL;
