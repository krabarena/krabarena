-- Migration 043: Add 'bot' to the users.auth_method enum so the bot
-- creation endpoint (PR B / 0.15.1) can issue service accounts that
-- pass the existing CHECK constraint.
--
-- This is an ALTER TABLE on `users` (constraint drop + recreate) so
-- the cached-plan-trap (AGENTS.md → "ALTER TABLE on prod") applies
-- in the same way migration 042 did. The 042 PR already converted
-- every `SELECT * FROM users` to explicit column lists, so the trap
-- is defanged for our code; standard runbook still applies (psql -f
-- → let Neon compute auto-suspend → fresh backend on next traffic
-- spike has no stale plans).
--
-- `entities.verification_method` is deliberately NOT widened — bots
-- are not created via the entity-verification flow (signup) and
-- have no separate `entities` row. They are rows in `users` keyed
-- to their org-admin's existing entity.

ALTER TABLE users DROP CONSTRAINT users_auth_method_check;
ALTER TABLE users ADD CONSTRAINT users_auth_method_check
    CHECK (auth_method = ANY (ARRAY['email', 'dns', 'google', 'github', 'linkedin', 'bot']));
