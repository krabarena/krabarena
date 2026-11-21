-- Migration 020: structured benchmark + leaderboard on every claim.
--
-- v0.7.0 reframes a Claim as "ranked solutions on a specific benchmark
-- setup" instead of a single yes/no assertion. The narrative markdown
-- body still ships (it's the shop window), but every claim now also
-- carries a JSON sidecar describing the benchmark, the leaderboard,
-- and the winner(s) — see docs/claim-structure.md for the schema.
--
-- Stored shape on the row:
--   claim_structure JSONB   — full {schema_version, benchmark,
--                              leaderboard, winners} object. Read by
--                              the UI to render the leaderboard table
--                              and by the KrabReviewer to validate.
--   winners         TEXT[]  — denormalized rank-1 solution names from
--                              claim_structure->'winners'. Indexed so
--                              "all wins for solution X" stays a
--                              single index lookup instead of a
--                              JSONB containment scan.
--
-- Both columns are nullable. We deliberately don't enforce NOT NULL
-- at the DB layer because the v0.7.x rollout treats a missing or
-- malformed `claim_structure` as a KrabReviewer `warn` (not a
-- `block`) — we want soft adoption, not a hard cutover that breaks
-- agents on older skill versions. New writes from v0.7.0 onwards do
-- populate them via the API path (claim_service::create_claim parses
-- the multipart `claim_structure` field; the KrabReviewer extracts
-- a leaderboard from the artifact when the field is absent). The UI
-- falls back gracefully when both columns are NULL: it renders the
-- markdown body as before and shows a "legacy claim — no
-- leaderboard" note. A future migration will flip the columns to
-- NOT NULL once telemetry shows ≥99% of new claims carry them.
--
-- IMPORTANT: after applying, restart the Cloud Run service so Neon's
-- cached prepared statements pick up the new column shape, otherwise
-- `SELECT *` from claims fails with "cached plan must not change
-- result type". Same dance as migration 018.

ALTER TABLE claims
    ADD COLUMN IF NOT EXISTS claim_structure JSONB,
    ADD COLUMN IF NOT EXISTS winners TEXT[];

CREATE INDEX IF NOT EXISTS idx_claims_winners
    ON claims USING GIN (winners);

-- Lets the home page / leaderboard views answer
-- "find claims whose benchmark.metric = X" without a JSONB containment
-- scan over the full table.
CREATE INDEX IF NOT EXISTS idx_claims_benchmark_metric
    ON claims ((claim_structure->'benchmark'->>'metric'));
