-- Migration 077: authentication hardening (2026-11).
--
-- pending_verifications:
--   * failed_attempts — wrong-code counter for the 8-digit voter email
--     login code. `POST /auth/voter/email/verify` increments it on every
--     miss for the identifier's live codes and refuses a code once it
--     reaches 5, so the code is effectively invalidated. A fresh
--     `/auth/voter/email` request issues a NEW code row (counter 0) and
--     deletes the old ones; it never resets an exhausted counter on an
--     existing code.
--   * pending_token / browser_binding — CLI device flow. The OAuth
--     callback no longer writes the JWT straight into `result_token`
--     (which `/v1/auth/device/poll` hands out); it stages it in
--     `pending_token` together with `browser_binding` = SHA-256 of the
--     browser's HttpOnly OAuth-state nonce. Only an explicit "Authorize"
--     POST from that same browser moves it into `result_token`.
--
-- voter_sessions:
--   * verified_email — the email address the provider (or our email code)
--     proved at login time, if any. `/auth/voter/me` mints the admin JWT
--     only when this matches an admin user, never from the unverified
--     `voters.email` profile value.
--
-- Readers: every query on pending_verifications uses explicit columns;
-- voter_sessions is only read via `SELECT v.* FROM voters v JOIN
-- voter_sessions s` (voters columns only) and explicit-column queries, so
-- no `SELECT *` result type changes (see AGENTS.md cached-plan note).
-- All statements are idempotent.

ALTER TABLE pending_verifications
    ADD COLUMN IF NOT EXISTS failed_attempts INTEGER NOT NULL DEFAULT 0;

ALTER TABLE pending_verifications
    ADD COLUMN IF NOT EXISTS pending_token TEXT;

ALTER TABLE pending_verifications
    ADD COLUMN IF NOT EXISTS browser_binding TEXT;

ALTER TABLE voter_sessions
    ADD COLUMN IF NOT EXISTS verified_email TEXT;
