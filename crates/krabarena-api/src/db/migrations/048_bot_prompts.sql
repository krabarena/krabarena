-- 048_bot_prompts.sql — live-editable bot prompts in Neon.
--
-- Before this migration, KrabScout's webhook-scout.md and
-- KrabChecker's prompt.md were baked into the service containers at
-- image-build time. Iterating on the prompt required: change file ->
-- PR -> CI build -> Deploy to Test -> E2E -> Deploy to Prod (~15 min).
--
-- 0.18.2 moves the canonical prompt to this table so admins can ship
-- a prompt edit in seconds via POST /v1/admin/bot-prompts/<name>.
-- The wrappers (run-scout.sh, run-verifier.sh) fetch the current
-- version at delivery time and fall back to the baked-in markdown
-- if the API is unreachable.
--
-- Version is monotonic per bot_name. We never overwrite — every
-- edit creates a new row.

CREATE TABLE bot_prompts (
    id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Which bot this prompt drives. Free-text but conventionally:
    --   'krabscout-webhook'  (services/krabscout/prompts/webhook-scout.md)
    --   'krabscout-daily'    (services/krabscout/prompts/daily-scout.md)
    --   'krabchecker'        (services/krabchecker/prompt.md)
    bot_name            TEXT         NOT NULL,
    -- The prompt body (markdown).
    content             TEXT         NOT NULL,
    -- 1, 2, 3, ... — sequential per bot_name. Latest = MAX(version).
    version             INT          NOT NULL,
    -- Who edited (NULL for the seed row inserted at server boot from
    -- the baked-in file).
    updated_by_user_id  UUID         REFERENCES users(id) ON DELETE SET NULL,
    -- One-line summary of what changed in this version.
    comment             TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),

    UNIQUE (bot_name, version)
);

-- Latest-per-bot lookup (the hot path the wrapper hits every delivery).
CREATE INDEX idx_bot_prompts_latest
    ON bot_prompts (bot_name, version DESC);
