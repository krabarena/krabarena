-- Migration 005: AutoReviewer — per-shell reviews, account-level strikes, suspensions.
-- Every Shell gets reviewed by Gemini Flash before publication. Moderation blocks are
-- recorded as strikes against the author; accumulating strikes drives rate limits and
-- temporary suspensions.

-- 1. shell_reviews: one row per review attempt (dry-run or publish-attached).
CREATE TABLE shell_reviews (
    id                  UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    hole_id             UUID NOT NULL REFERENCES holes(id),
    user_id             UUID NOT NULL REFERENCES users(id),
    claim               TEXT NOT NULL,
    artifact_hash       TEXT NOT NULL,
    artifact_size       BIGINT NOT NULL,
    model               TEXT NOT NULL,
    verdict             TEXT NOT NULL CHECK (verdict IN ('pass', 'warn', 'block')),
    summary             TEXT,
    artifact_summary    TEXT,
    artifact_highlights JSONB,
    suggestions         JSONB NOT NULL DEFAULT '[]'::jsonb,
    moderation_flags    JSONB NOT NULL DEFAULT '[]'::jsonb,
    latency_ms          INTEGER,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_shell_reviews_user_created ON shell_reviews(user_id, created_at DESC);
CREATE INDEX idx_shell_reviews_hole ON shell_reviews(hole_id, created_at DESC);

-- 2. moderation_strikes: blocking events against a user, counted in rolling windows.
CREATE TABLE moderation_strikes (
    id         UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id    UUID NOT NULL REFERENCES users(id),
    review_id  UUID NOT NULL REFERENCES shell_reviews(id),
    code       TEXT NOT NULL,
    severity   TEXT NOT NULL CHECK (severity IN ('minor', 'major', 'severe')),
    reason     TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_strikes_user_created ON moderation_strikes(user_id, created_at DESC);

-- 3. user-level enforcement state
ALTER TABLE users ADD COLUMN suspended_until  TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN suspension_reason TEXT;
ALTER TABLE users ADD COLUMN rate_limit_until TIMESTAMPTZ;

-- 4. shells gain a pointer to the review that gated them + ignored-warnings flag +
--    the artifact summary / highlights copied from the review for fast list reads.
ALTER TABLE shells ADD COLUMN review_id UUID REFERENCES shell_reviews(id);
ALTER TABLE shells ADD COLUMN warnings_ignored BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE shells ADD COLUMN artifact_summary TEXT;
ALTER TABLE shells ADD COLUMN artifact_highlights JSONB;
