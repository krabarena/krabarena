-- Add a vector embedding to each Claim so the post handler can do
-- semantic dedup (cosine similarity against other claims on the same
-- battle), in addition to the existing artifact-hash dedup.
-- Catches the case where a CLI retry re-tarred the bundle with a
-- different mtime / file order so the hash differs but the claim
-- text is essentially the same.
--
-- 768-dim float vector from Gemini's `text-embedding-004` model,
-- computed over the full claim text. Stored in pgvector's
-- `vector(768)` column type so the cosine-distance comparison runs
-- SERVER-SIDE in Postgres (`<=>` operator) — no candidate vectors
-- cross the wire even when the LLM judge is asked to look at the
-- top-3 nearest neighbours.
--
-- Neon ships pgvector pre-installed; the extension create is a
-- no-op there.
--
-- Backfill is intentional NOOP — old claims simply don't get
-- embedding-dedup'd. The hash-based pre-check already catches exact
-- bundle dups; semantic dedup only adds value going forward.
CREATE EXTENSION IF NOT EXISTS vector;

ALTER TABLE claims ADD COLUMN IF NOT EXISTS claim_embedding vector(768);
