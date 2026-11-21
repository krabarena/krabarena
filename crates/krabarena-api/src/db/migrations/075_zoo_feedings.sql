-- Migration 075: Crawler Zoo food bowls (0.45.0).
--
-- A visitor drops a vended snack into a bot's bowl (`bowl_slug`); the
-- first bot to fetch `/zoo/bowls/{slug}.txt` eats every open snack in
-- that bowl. If the eater's species differs from `bowl_slug`, it stole
-- them. `device_id` is the feeder's `ka_device` cookie, used only for
-- the 5-tokens-a-day limit.
--
-- New table only — no ALTER on an existing table, so no PgBouncer
-- `cached plan must not change result type` risk. All readers use
-- explicit columns. Idempotent (`IF NOT EXISTS`).
--
-- NEVER edit this file after it has run in any environment (sqlx
-- checksums are immutable).

CREATE TABLE IF NOT EXISTS zoo_feedings (
    id             BIGSERIAL PRIMARY KEY,
    bowl_slug      TEXT        NOT NULL,
    slot           TEXT        NOT NULL,
    snack          TEXT        NOT NULL,
    device_id      UUID        NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    eaten_at       TIMESTAMPTZ,
    eaten_by_slug  TEXT,
    eaten_by_name  TEXT
);

CREATE INDEX IF NOT EXISTS idx_zoo_feedings_open
    ON zoo_feedings (bowl_slug, created_at) WHERE eaten_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_zoo_feedings_device_day
    ON zoo_feedings (device_id, created_at);
CREATE INDEX IF NOT EXISTS idx_zoo_feedings_eaten
    ON zoo_feedings (eaten_at DESC) WHERE eaten_at IS NOT NULL;
