-- 059_pagination_indexes.sql
--
-- Supporting indexes for the unified keyset (cursor) pagination
-- introduced alongside `src/pagination.rs`. Two classes of query gain
-- an index range scan instead of a sequential scan:
--
--   1. Per-claim comment activity. `idx_comments_claim_id` (migration
--      004) covers only the `claim_id` equality, so the claim activity
--      feed's `ORDER BY created_at DESC` fell back to a sort over the
--      whole matching set. The composite below lets the keyset
--      `(created_at, id)` walk be an index-only range scan.
--
--   2. Contributor activity (`/agents/{id}`). The rewritten endpoint
--      UNIONs a user's battles / claims / verifications / comments and
--      pages them by `(created_at, id)`. Each leg needs an
--      `(author_id, created_at DESC)` index to avoid scanning the whole
--      table per author.
--
-- All plain `CREATE INDEX IF NOT EXISTS` (not CONCURRENTLY): the sqlx
-- migrator runs each file in a transaction, and CONCURRENTLY cannot run
-- inside one. Matches migration 054. CREATE INDEX never changes a
-- SELECT's result type, so it's free of the `0A000 cached plan` trap.

-- (1) Per-claim comment activity keyset.
CREATE INDEX IF NOT EXISTS idx_comments_claim_created
    ON comments (claim_id, created_at DESC, id);

-- (2) Contributor activity legs, ordered by recency per author.
CREATE INDEX IF NOT EXISTS idx_battles_author_created
    ON battles (author_id, created_at DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_claims_author_created
    ON claims (author_id, created_at DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_verifications_author_created
    ON verifications (author_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_comments_author_created
    ON comments (author_id, created_at DESC);
