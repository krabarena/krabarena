-- Migration 051: Resize `search_embedding` columns from 1536 → 768 dims.
--
-- Background: 0.19.2 / migration 050 added `search_embedding vector(1536)`
-- on claims / battles / users matching OpenAI `text-embedding-3-small`.
-- 0.19.3 swaps the embedding provider to Gemini `gemini-embedding-001`
-- at 768-dim matryoshka truncation, matching the existing
-- `claim_embedding vector(768)` column (migration 023) used for claim
-- dedup. One provider, one dimension across both surfaces.
--
-- Data loss is intentional: the only rows that had `search_embedding`
-- populated since 050 landed (a few hours ago) carried hash-fallback
-- garbage anyway — no `GEMINI_API_KEY` was set on the Cloud Run
-- service when 050 was applied, so the lazy backfill never ran. The
-- new write-path indexer (services::search_index) re-embeds on the
-- next boot at the new dimension.
--
-- pgvector doesn't support changing a vector column's dimensionality
-- in place. We DROP INDEX → DROP COLUMN → ADD COLUMN → CREATE INDEX
-- on each of the three tables. ADD COLUMN with no DEFAULT is
-- metadata-only (no row rewrite), so the cached-plan trap doesn't
-- bite — but follow the standard runbook anyway (apply during low
-- traffic, let Neon's PgBouncer backends recycle).

-- Drop the HNSW indexes first so the column drops don't fail.
DROP INDEX IF EXISTS idx_claims_search_embedding;
DROP INDEX IF EXISTS idx_battles_search_embedding;
DROP INDEX IF EXISTS idx_users_search_embedding;

-- Drop + readd at 768 dims.
ALTER TABLE claims  DROP COLUMN IF EXISTS search_embedding;
ALTER TABLE claims  ADD  COLUMN search_embedding vector(768);

ALTER TABLE battles DROP COLUMN IF EXISTS search_embedding;
ALTER TABLE battles ADD  COLUMN search_embedding vector(768);

ALTER TABLE users   DROP COLUMN IF EXISTS search_embedding;
ALTER TABLE users   ADD  COLUMN search_embedding vector(768);

-- Recreate the HNSW cosine indexes at the new dim.
CREATE INDEX idx_claims_search_embedding
    ON claims  USING hnsw (search_embedding vector_cosine_ops);
CREATE INDEX idx_battles_search_embedding
    ON battles USING hnsw (search_embedding vector_cosine_ops);
CREATE INDEX idx_users_search_embedding
    ON users   USING hnsw (search_embedding vector_cosine_ops);

-- The backfill is again NOOP — services::search_index::startup_backfill
-- fills NULL rows lazily on next boot, capped per kind.
