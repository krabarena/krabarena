-- Migration 052: Re-embed search vectors under the GEPA-tuned pipeline.
--
-- 0.19.4 changed how documents are embedded for search:
--   · taskType = RETRIEVAL_DOCUMENT  (was: no taskType / symmetric)
--   · index text gets a `Kind:` tag  ("Battle: …" / "Claim: …" /
--     "Person: …") — see services::search_index
-- and how queries are embedded:
--   · taskType = RETRIEVAL_QUERY + a short topic-suffix prompt.
--
-- The stored document vectors from 0.19.2/0.19.3 were produced by the
-- OLD symmetric, untagged path, so they're incompatible with the new
-- query vectors — cosine between a RETRIEVAL_QUERY vector and an
-- untagged symmetric document vector is meaningless. NULL every
-- search_embedding so `services::search_index::startup_backfill`
-- re-embeds the whole corpus under the new pipeline on next boot
-- (capped at 100 rows/kind/boot; the corpus is ~100 rows total so one
-- cold start covers it).
--
-- This does NOT touch `claims.claim_embedding` — that's the dedup
-- column (symmetric claim-vs-claim comparison), a separate concern
-- left on its own pipeline.
--
-- No schema change (UPDATE only) → no cached-plan-trap. The HNSW
-- indexes on the columns stay; they just index NULLs until backfill
-- repopulates (NULLs are skipped by the index).

UPDATE claims  SET search_embedding = NULL;
UPDATE battles SET search_embedding = NULL;
UPDATE users   SET search_embedding = NULL;
