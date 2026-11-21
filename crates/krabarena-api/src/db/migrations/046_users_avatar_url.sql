-- Migration 046: Add `users.avatar_url` for profile photos.
--
-- 0.18.1 introduces user-uploaded profile photos. Authenticated users
-- POST a multipart image (or a JSON `{ url }`) to
-- `/v1/users/me/avatar` and the server stores the bytes in GCS
-- (`gs://krabarena-artifacts/avatars/<sha>.<ext>`) and writes the
-- resulting public URL here. NULL means "no photo set" — every author
-- card and the agent profile page fall back to the existing initials
-- avatar in that case, so the column is nullable and has no default.
--
-- Cached-plan-trap mitigation (AGENTS.md → "ALTER TABLE on prod"):
-- This is an `ADD COLUMN` on `users`, which changes the result type
-- of every `SELECT * FROM users` plan still living on a Neon
-- PgBouncer backend. Migration 042 already converted every read
-- through `services/auth_service.rs::USER_COLUMNS` to an explicit
-- column list; this PR extends that list to include `avatar_url`.
-- The agent-profile / leaderboard SQL in `web/pages.rs` and
-- `routes/agents.rs` also reads explicit columns, so the only `*`
-- exposure is from external operators running ad-hoc psql sessions
-- — those are advised to reconnect after the migration.
--
-- Standard runbook still applies:
--   1. psql -f migrations/046_users_avatar_url.sql
--   2. Wait for Neon's compute to auto-suspend (~5 min idle) so
--      fresh backends start with no cached plans, OR force-restart
--      via the Neon API.
--   3. Deploy the new Cloud Run revision (which knows about the
--      new column).
--
-- Bots (e.g. KrabScout, KrabChecker) leave `avatar_url` NULL until
-- an admin manually sets it via a forthcoming admin-side endpoint
-- (not in this PR — we want a human to pick the generated logo
-- before wiring it in).

ALTER TABLE users
    ADD COLUMN avatar_url TEXT;

-- Light validation so we never persist an obviously-bogus value.
-- Avatars are stored on GCS (`https://storage.googleapis.com/…`)
-- or, in dev fallback, an `http(s)://…` URL the server printed;
-- block schemes other than http/https. Length cap mirrors the
-- existing 2 KB ceiling on `users.email` / `entities.domain`-style
-- text columns; a real avatar URL is well under 200 chars but the
-- cap is defensive.
ALTER TABLE users
    ADD CONSTRAINT users_avatar_url_format CHECK (
        avatar_url IS NULL OR (
            length(avatar_url) <= 2048
            AND (avatar_url LIKE 'https://%' OR avatar_url LIKE 'http://%')
        )
    );
