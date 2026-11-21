-- 026_gemini_calls.sql
--
-- Per-attempt audit log for every outbound Gemini API call. Lets us
-- compute an honest reviewer-availability SLO (currently a guess based
-- on log-scraped retry events) and slice it by purpose / model /
-- error_kind to see where the budget is going.
--
-- One row per HTTP attempt. Retries of the same logical review are
-- grouped by `request_id`; the row with the highest `attempt_no`
-- carries the terminal status (success | exhausted | non_2xx_terminal |
-- transport_error). Earlier rows in the group are `transient_retry`.
--
-- Status values:
--   success            — 2xx response, returned to caller
--   transient_retry    — 503/429/connect/timeout, will retry
--   exhausted          — retries used up, upstream still bad
--   non_2xx_terminal   — 4xx/5xx (non-503/429), not retried, returned
--   transport_error    — reqwest::Error not classified as connect/timeout
--
-- error_kind values (NULL on success):
--   upstream_status    — non-2xx HTTP response
--   connect            — reqwest is_connect()
--   timeout            — reqwest is_timeout()
--   transport          — other reqwest::Error
--
-- Purposes (from gemini_service call sites):
--   claim_review       — agentic_reviewer stage 1 + 2 (post a claim)
--   battle_review      — KrabReviewer for /v1/battles/{id}/review
--   verification_review— verifier sanity check
--   battle_summary     — executive summary regeneration
--   dedup_judge        — semantic dedup judgment
--   embed              — embedding lookup
--
-- Retention: nothing automated yet. At ~1 review/sec sustained the
-- table grows ~80MB/month; revisit with a `created_at < NOW() - INTERVAL '90 days'`
-- DELETE job once we have a few weeks of data and an SLO baseline.

CREATE TABLE IF NOT EXISTS gemini_calls (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Same UUID across every retry of one logical call so a single
    -- review's full attempt sequence is one GROUP BY.
    request_id   UUID         NOT NULL,
    attempt_no   INTEGER      NOT NULL,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    purpose      TEXT         NOT NULL,
    model        TEXT         NOT NULL,
    status       TEXT         NOT NULL,
    http_status  INTEGER,
    error_kind   TEXT,
    -- Truncated at write time; long Gemini error payloads aren't
    -- useful here (full body lives in stderr / Cloud Logging).
    error_msg    TEXT,
    latency_ms   INTEGER      NOT NULL
);

-- Time-window queries: "success rate over the last hour".
CREATE INDEX IF NOT EXISTS idx_gemini_calls_created_at
    ON gemini_calls(created_at DESC);

-- Group-by request_id for per-call rollups.
CREATE INDEX IF NOT EXISTS idx_gemini_calls_request
    ON gemini_calls(request_id);

-- Slice by purpose (claim_review SLO vs embed SLO etc.).
CREATE INDEX IF NOT EXISTS idx_gemini_calls_purpose_created
    ON gemini_calls(purpose, created_at DESC);
