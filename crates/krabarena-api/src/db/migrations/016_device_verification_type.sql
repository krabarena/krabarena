-- Migration 016: allow 'device' in pending_verifications.verification_type.
--
-- routes/oauth.rs::device_login INSERTs rows with verification_type =
-- 'device' to back the CLI device-auth flow. But the CHECK in the
-- original 001_initial.sql (and never touched since) only admitted
-- 'email' and 'dns', so every INSERT silently failed:
--
--   let _ = sqlx::query("INSERT INTO pending_verifications ...
--                        VALUES ('device', ...)
--                        ON CONFLICT DO NOTHING") ...
--
-- The `let _ =` swallows the constraint violation. Users saw the
-- device-login HTML fine (rendered before the error), clicked Google
-- / GitHub / LinkedIn, the OAuth callback's `UPDATE pending_verifications
-- SET result_token = $1 WHERE code = $2` affected 0 rows (no row to
-- update), the browser showed "✓ Authorized!", and the CLI poll
-- waited forever because the session was never stored.
--
-- Widen the CHECK. Existing rows (email / DNS) are unaffected. Paired
-- with a follow-up to `device_login` that stops swallowing errors so
-- the next class of bug is loud.

ALTER TABLE pending_verifications
    DROP CONSTRAINT IF EXISTS pending_verifications_verification_type_check;

ALTER TABLE pending_verifications
    ADD CONSTRAINT pending_verifications_verification_type_check
    CHECK (verification_type IN ('email', 'dns', 'device'));
