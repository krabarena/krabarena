-- Hybrid embedding + freshness + hotness search (since 0.18.0).
--
-- Adds a 1536-dim `search_embedding` column to claims, battles, and
-- users so the new `/v1/search` endpoint can rank entities across
-- all three kinds via cosine similarity against a query embedding.
--
-- 1536 dims matches OpenAI `text-embedding-3-small` (the default
-- production embedding model). When `OPENAI_API_KEY` is unset the
-- embedding service falls back to a deterministic SHA-256-derived
-- hash vector of the same dimensionality so the column shape stays
-- stable across local/dev/prod and tests don't depend on network.
--
-- This is a NEW column on `claims` — the existing 768-dim
-- `claim_embedding` (migration 023) stays as-is because it powers
-- the per-Battle semantic dedup pre-check, which is a different
-- product surface using a different model (Gemini text-embedding-001
-- @ 768 dims) and would not benefit from a re-embedding pass.
--
-- pgvector ships pre-installed on Neon; the CREATE EXTENSION is a
-- no-op there but keeps the migration runnable against a fresh
-- local Postgres for E2E reproduction.
CREATE EXTENSION IF NOT EXISTS vector;

ALTER TABLE claims  ADD COLUMN IF NOT EXISTS search_embedding vector(1536);
ALTER TABLE battles ADD COLUMN IF NOT EXISTS search_embedding vector(1536);
ALTER TABLE users   ADD COLUMN IF NOT EXISTS search_embedding vector(1536);

-- HNSW indexes for cosine distance. `vector_cosine_ops` is the
-- opclass that matches the `<=>` operator we use in queries.
--
-- HNSW (vs IVFFlat) gives better recall at small dataset sizes
-- (KrabArena is in the thousands of rows, not millions) and doesn't
-- require an explicit list-count parameter. Default `m=16`,
-- `ef_construction=64` is appropriate for our scale.
CREATE INDEX IF NOT EXISTS idx_claims_search_embedding
    ON claims  USING hnsw (search_embedding vector_cosine_ops);
CREATE INDEX IF NOT EXISTS idx_battles_search_embedding
    ON battles USING hnsw (search_embedding vector_cosine_ops);
CREATE INDEX IF NOT EXISTS idx_users_search_embedding
    ON users   USING hnsw (search_embedding vector_cosine_ops);

-- Backfill is intentionally NOOP. The server's startup hook scans
-- rows where `search_embedding IS NULL` (capped per startup) and
-- fills them lazily; on insert/update the embedding pipeline fires
-- via tokio::spawn so the write path stays fast.
