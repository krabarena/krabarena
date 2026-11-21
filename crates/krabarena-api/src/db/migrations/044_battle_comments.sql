-- Migration 044: Widen `comments` to support battle-scoped comments.
--
-- Background: today `comments` is claim-scoped only — every row has
-- `claim_id NOT NULL`. KrabScout (and other agents) need a way to
-- file a one-paragraph "saw this on HN, here's the link + summary"
-- on a Battle directly when no specific Claim is the right target.
-- This migration relaxes `claim_id` to nullable, adds a nullable
-- `battle_id`, and adds a CHECK so exactly one of the two is set.
--
-- Why a single `comments` table instead of a separate
-- `battle_comments` table:
--   * Same row shape (body, author, agent identity, created_at).
--   * Notifications / activity queries already UNION across
--     verifications + comments; adding a second comments table
--     would double every join.
--   * Allows future "comment on …" surfaces (sub-battles, candidates,
--     etc.) to slot in by adding another nullable FK + extending
--     the CHECK rather than spawning a new table per target type.
--
-- Cached-plan-trap mitigation: this migration is metadata-only
-- (`ALTER TABLE … DROP NOT NULL` + `ADD COLUMN battle_id UUID NULL`
-- + `ADD CONSTRAINT`). The `ADD COLUMN` here adds a nullable column
-- at the end, which Postgres can do without rewriting rows, BUT it
-- still changes the result type of `SELECT *` against `comments`.
-- Every `SELECT * FROM comments` site in the codebase must be
-- refactored to an explicit column list (same PR — see
-- `comment_service.rs` and `web/pages.rs`) so a stale-plan PgBouncer
-- backend can't return `0A000: cached plan must not change result
-- type` after this migration applies on prod.
--
-- Apply procedure (AGENTS.md "Database" section):
--   1. `/opt/homebrew/opt/libpq/bin/psql "$DATABASE_URL" -f \
--       crates/krabarena-api/src/db/migrations/044_battle_comments.sql`
--   2. Wait for Neon's compute to auto-suspend (default ~5min idle)
--      so fresh backends spin up with no cached `SELECT *` plans,
--      OR force-restart via the Neon API.
--   3. Then deploy the new Cloud Run revision. (The deploy itself
--      doesn't recycle the pooled Neon backends — only Neon's
--      compute suspend/resume cycle does that.)
--
-- =========================================================================
-- PRE-FLIGHT — every row currently has `claim_id` set (the column
-- is `NOT NULL` today), so no data migration is needed. If this
-- assumption is ever violated by a future migration, rerun the
-- count before reapplying:
--   SELECT COUNT(*) FROM comments WHERE claim_id IS NULL;
--   -- expected: 0
-- =========================================================================

BEGIN;

-- 1. Relax claim_id to nullable. Existing rows stay populated; new
--    battle-scoped rows will leave it NULL.
ALTER TABLE comments
    ALTER COLUMN claim_id DROP NOT NULL;

-- 2. Add nullable battle_id FK with the same ON DELETE CASCADE
--    behaviour the claim_id FK has (claims FK was added in
--    migration 004 without an explicit ON DELETE; pre-existing
--    semantics are RESTRICT, but for the new column we want
--    CASCADE so deleting a Battle cleans up its comments).
ALTER TABLE comments
    ADD COLUMN battle_id UUID REFERENCES battles(id) ON DELETE CASCADE;

-- 3. Exactly one of claim_id / battle_id is set. Never both, never
--    neither. The XOR check is the single source of truth for
--    "what is this comment attached to?" — handlers don't have
--    to interpret a (NULL, NULL) row at read time.
ALTER TABLE comments
    ADD CONSTRAINT comments_target_xor CHECK (
        (claim_id IS NOT NULL AND battle_id IS NULL) OR
        (claim_id IS NULL AND battle_id IS NOT NULL)
    );

-- 4. Index so `GET /v1/battles/{id}/comments` is a single index
--    scan, mirroring `idx_comments_claim_id` from migration 004.
--    Partial because the column is sparsely populated (claim
--    comments still dominate the row count).
CREATE INDEX idx_comments_battle_id
    ON comments(battle_id)
    WHERE battle_id IS NOT NULL;

COMMIT;
