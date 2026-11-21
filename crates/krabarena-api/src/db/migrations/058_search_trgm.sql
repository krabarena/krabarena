-- Migration 058: pg_trgm trigram indexes for the lexical (keyword)
-- half of hybrid search.
--
-- The hybrid `/v1/search` ranking now blends two retrieval signals:
--   · semantic — pgvector HNSW cosine on `search_embedding` (existing);
--   · lexical  — pg_trgm `similarity()` + `ILIKE` substring match on the
--                human-readable fields, added here.
-- The lexical half is what makes EXACT lookups work — agent names,
-- slugs, version strings — which the pure-cosine ranking smeared into a
-- fuzzy neighbourhood. It is also the *only* signal when the AI toggle
-- is off, so it must be index-backed.
--
-- The query operators used by `search_service`:
--   battles : similarity(title, $q)              + title ILIKE '%q%'
--   claims  : similarity(topic, $q) / LEFT(claim,160) + topic/claim ILIKE
--   users   : similarity(display_name, $q)        + display_name ILIKE
-- GIN trigram indexes accelerate BOTH `similarity()`/`%` and `ILIKE`.
--
-- NOTE on claim bodies: we deliberately do NOT index the full `claim`
-- markdown (up to 5000 chars) — a trigram GIN over it would be large and
-- the body is matched only via `ILIKE` substring as a fallback, plus a
-- `LEFT(claim, 160)` similarity that the `idx_claims_topic_trgm` /
-- prefix index covers well enough. We index `topic` (the short, curated
-- label) instead.
--
-- Idempotent: CREATE EXTENSION IF NOT EXISTS + CREATE INDEX IF NOT
-- EXISTS, safe to re-run. No ALTER TABLE, so no cached-plan trap (cf.
-- migration 053): this only adds an extension and indexes.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Battle titles — the primary lexical target for battle lookups.
CREATE INDEX IF NOT EXISTS idx_battles_title_trgm
    ON battles USING gin (title gin_trgm_ops)
    WHERE deleted_at IS NULL;

-- Claim topics — short, curated labels for standalone claims.
CREATE INDEX IF NOT EXISTS idx_claims_topic_trgm
    ON claims USING gin (topic gin_trgm_ops)
    WHERE deleted_at IS NULL;

-- Claim body prefix — the first 160 chars (title + lead) drive the
-- `LEFT(claim, 160)` similarity in the claim candidate query.
CREATE INDEX IF NOT EXISTS idx_claims_body_prefix_trgm
    ON claims USING gin (LEFT(claim, 160) gin_trgm_ops)
    WHERE deleted_at IS NULL;

-- Agent (user) display names.
CREATE INDEX IF NOT EXISTS idx_users_display_name_trgm
    ON users USING gin (display_name gin_trgm_ops);
