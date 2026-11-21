-- 035_standalone_claims.sql
--
-- Feature: standalone claims + post-hoc Battle adoption.
--
-- Today every Claim must reference a Battle (`claims.battle_id` is
-- NOT NULL). This migration loosens that so an author can post a
-- result against a topic that doesn't yet have a curated Battle,
-- then move it into a Battle later (or have an editor adopt it).
--
-- Visibility model (re-using the existing `claims.visibility`
-- column added in migration 010, which used to only carry
-- 'public' / 'private'):
--   * `public` — discoverable on lists, indexed by SEO. Existing
--     claims (all with battle_id) keep this default.
--   * `private` — only visible to the author + admins. The
--     standalone-claim CLI flow OVERRIDES the default to 'private'
--     on insert so the orphan-claim space can't be spammed.
--   * `adopted` — set when a previously-standalone claim is moved
--     into a Battle (via `POST /v1/claims/{id}/adopt`). Renders as
--     a public claim under that Battle.
--
-- Indexes:
--   * `idx_claims_standalone_public` — for the `/claims/standalone`
--     listing page (only public standalone rows).
--   * `idx_claims_topic_tags` — GIN on the new `topic_tags TEXT[]`
--     so a future "claims by topic tag" filter is an index lookup.

-- 1. Loosen the NOT NULL on claims.battle_id so a Claim can exist
--    without a Battle. Existing rows are unaffected (they all have
--    a battle_id set).
ALTER TABLE claims ALTER COLUMN battle_id DROP NOT NULL;

-- 1a. The companion `claim_reviews.battle_id` column also needs to
--     loosen — every claim post writes a row there, and a standalone
--     review has no Battle to point at. Existing rows already have
--     a battle_id, so the relaxation is additive.
ALTER TABLE claim_reviews ALTER COLUMN battle_id DROP NOT NULL;

-- 2. Standalone claims need a topic (the question they're answering)
--    because the title alone often doesn't surface it. Server-side
--    validation requires `topic` and at least one entry in
--    `topic_tags` on the standalone path; Battle-attached claims
--    may leave either empty.
ALTER TABLE claims
    ADD COLUMN IF NOT EXISTS topic TEXT,
    ADD COLUMN IF NOT EXISTS topic_tags TEXT[] NOT NULL DEFAULT '{}'::text[];

-- 3. Widen the existing visibility CHECK to include 'adopted'. The
--    legacy column was NOT NULL DEFAULT 'public' with CHECK
--    (visibility IN ('public', 'private')) — adding 'adopted' is
--    additive (no existing row holds a value that the new CHECK
--    would reject).
ALTER TABLE claims DROP CONSTRAINT IF EXISTS claims_visibility_check;
ALTER TABLE claims
    ADD CONSTRAINT claims_visibility_check
    CHECK (visibility IN ('private', 'public', 'adopted'));

CREATE INDEX IF NOT EXISTS idx_claims_standalone_public
    ON claims(created_at DESC)
    WHERE battle_id IS NULL AND visibility = 'public';

CREATE INDEX IF NOT EXISTS idx_claims_topic_tags
    ON claims USING gin(topic_tags);
