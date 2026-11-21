-- 054_verifications_created_at_index.sql
--
-- The home Verifications tab (0.23.0) ranks recent verification
-- outcomes by hotness (recency × verdict weight, exponentially decayed).
-- The decay expression is non-sargable, so instead of ordering by it in
-- SQL (a full scan + sort on every home load) the handler pulls the
-- most-recent N verifications and ranks them in the app. This index
-- makes that `ORDER BY created_at DESC LIMIT N` pre-filter an index
-- range scan instead of a sequential scan, and the cost stays flat as
-- the verifications corpus grows.
--
-- Apply manually (see AGENTS.md "Database" runbook):
--   psql "$DATABASE_URL" -f 054_verifications_created_at_index.sql
-- Safe on a live table — CREATE INDEX does not change any SELECT's
-- result type, so it doesn't hit the `0A000 cached plan` class of issue.
CREATE INDEX IF NOT EXISTS idx_verifications_created_at
    ON verifications (created_at DESC);
