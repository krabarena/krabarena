-- Migration 067: claim presentation (viral share layer).
--
-- KrabReviewer now emits a `presentation` object (layout, tweet_title,
-- tweet_annotation, hero_stat, refutation) next to its og_summary.
-- Persist both: on claim_reviews as the review-of-record, and on
-- claims for render-time reads. claims.og_summary (migration 018)
-- already exists but never had a write path — this release adds one.
--
-- ⚠️  CACHED-PLAN TRAP — READ BEFORE / AFTER APPLYING  (AGENTS.md "Database")
-- Both altered tables are poisoned equally: `claims` still has
-- `SELECT *` read paths, and `claim_reviews` gains TWO new columns
-- here (og_summary, presentation) with its own `SELECT *` /
-- `RETURNING *` plans (see review_service.rs's claim_reviews INSERT).
-- After ALTER TABLE, any cached prepared plan doing `SELECT *` /
-- `RETURNING *` against EITHER table returns
--   0A000: cached plan must not change result type
-- on every Neon PgBouncer backend holding the stale plan — a Cloud Run
-- revision swap is NOT enough. The recycle step below covers both
-- tables' backends; there is no separate step needed for
-- `claim_reviews`.
--
-- Runbook (mandatory ordering):
--   1. Apply this migration:
--        /opt/homebrew/opt/libpq/bin/psql "$DATABASE_URL" \
--          -f crates/krabarena-api/src/db/migrations/067_claim_presentation.sql
--   2. Recycle Neon backends BEFORE the deploy carrying the new code
--      lands traffic (let the compute auto-suspend ~5min idle, or force
--      a compute restart via the Neon API).
--   3. Then deploy.

ALTER TABLE claims ADD COLUMN IF NOT EXISTS presentation JSONB;
ALTER TABLE claim_reviews ADD COLUMN IF NOT EXISTS og_summary TEXT;
ALTER TABLE claim_reviews ADD COLUMN IF NOT EXISTS presentation JSONB;
