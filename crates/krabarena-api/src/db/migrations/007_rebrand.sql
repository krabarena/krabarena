-- Migration 007: KrabArena rebrand — rename tables and columns.
--
-- Metadata-only ALTERs: no row rewrites. Safe on live data.
--
-- Terminology map:
--   holes          -> battles
--   shells         -> claims
--   responses      -> verifications
--   shell_reviews  -> claim_reviews
--
-- The verdict string values ('confirmed', 'disconfirmed') are intentionally
-- preserved on disk. The Rust layer translates them to Verdict::Verified /
-- Verdict::Refuted at the API boundary. Rewriting the data would require a
-- mass UPDATE and would invalidate any downstream analytics that already
-- know the legacy values.
--
-- The battles.status values ('open', 'contested', 'holding', 'settled') are
-- likewise preserved.

-- 1. Rename tables.
ALTER TABLE holes RENAME TO battles;
ALTER TABLE shells RENAME TO claims;
ALTER TABLE responses RENAME TO verifications;
ALTER TABLE shell_reviews RENAME TO claim_reviews;

-- 2. Rename foreign-key columns.
ALTER TABLE claims RENAME COLUMN hole_id TO battle_id;
ALTER TABLE verifications RENAME COLUMN shell_id TO claim_id;
ALTER TABLE comments RENAME COLUMN shell_id TO claim_id;
ALTER TABLE votes RENAME COLUMN shell_id TO claim_id;
ALTER TABLE claim_reviews RENAME COLUMN hole_id TO battle_id;

-- 3. Rename count columns on claims.
ALTER TABLE claims RENAME COLUMN confirmed_count TO verify_count;
ALTER TABLE claims RENAME COLUMN disconfirmed_count TO refute_count;

-- 4. Rename indexes.
ALTER INDEX idx_holes_status_tags       RENAME TO idx_battles_status_tags;
ALTER INDEX idx_holes_parent_id         RENAME TO idx_battles_parent_id;
ALTER INDEX idx_holes_updated_at        RENAME TO idx_battles_updated_at;
ALTER INDEX idx_shells_hole_id          RENAME TO idx_claims_battle_id;
ALTER INDEX idx_shells_predecessor_id   RENAME TO idx_claims_predecessor_id;
ALTER INDEX idx_shells_artifact_hash    RENAME TO idx_claims_artifact_hash;
ALTER INDEX idx_responses_shell_id      RENAME TO idx_verifications_claim_id;
ALTER INDEX idx_responses_author_id     RENAME TO idx_verifications_author_id;
ALTER INDEX idx_responses_agent         RENAME TO idx_verifications_agent;
ALTER INDEX idx_comments_shell_id       RENAME TO idx_comments_claim_id;
ALTER INDEX idx_votes_shell_id          RENAME TO idx_votes_claim_id;
ALTER INDEX idx_shell_reviews_user_created RENAME TO idx_claim_reviews_user_created;
ALTER INDEX idx_shell_reviews_hole      RENAME TO idx_claim_reviews_battle;
