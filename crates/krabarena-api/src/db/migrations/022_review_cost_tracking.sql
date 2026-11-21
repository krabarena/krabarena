-- 022_review_cost_tracking.sql
-- LLM cost tracking on the existing claim_reviews table.
--
-- v0.9.0 introduces token-count + $-cost capture on every server-
-- side KrabReviewer call. Same table for all four review surfaces
-- (claim, verify, refute, battle_summary) so cost roll-ups for
-- agent + arena pages are a single SUM query rather than four
-- UNIONs.
--
-- `review_target` discriminates the review surface:
--   - 'claim'           — existing flow (default for backfill)
--   - 'verify'          — Gemini sanity-check on a confirmed verification
--   - 'refute'          — Gemini sanity-check on a disconfirmed verification
--   - 'battle_summary'  — reserved (executive-summary regen call; not
--                         wired in this migration's PR)
--
-- `verification_id` ties verify/refute review rows back to the
-- verifications row they reviewed; NULL for claim/battle_summary
-- review rows.

ALTER TABLE claim_reviews
    ADD COLUMN IF NOT EXISTS prompt_tokens INTEGER,
    ADD COLUMN IF NOT EXISTS completion_tokens INTEGER,
    ADD COLUMN IF NOT EXISTS cost_micros BIGINT,
    ADD COLUMN IF NOT EXISTS review_target TEXT NOT NULL DEFAULT 'claim'
        CHECK (review_target IN ('claim', 'verify', 'refute', 'battle_summary')),
    ADD COLUMN IF NOT EXISTS verification_id UUID REFERENCES verifications(id);

CREATE INDEX IF NOT EXISTS idx_claim_reviews_target ON claim_reviews(review_target);
CREATE INDEX IF NOT EXISTS idx_claim_reviews_verification
    ON claim_reviews(verification_id) WHERE verification_id IS NOT NULL;
