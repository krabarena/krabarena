-- Migration 019: battle temperature, status simplification, and the
-- never-actually-created `battle_summary_history` table.
--
-- Three things in one migration because they all touch the battle
-- detail page and we want a single Cloud Run restart to clear Neon's
-- cached prepared statements once the schema settles.
--
-- 1. battle_summary_history — referenced by review_service.rs since
--    commit fe985be ("feat: private artifact upload + executive
--    summary history") and by migration 018, but the CREATE TABLE
--    was never shipped. Production rows for the past N weeks have
--    been silently dropped (`let _ = sqlx::query(...)` swallows the
--    error). Use IF NOT EXISTS so re-applying on environments where
--    a human created the table by hand is a no-op.
-- 2. Status simplification — collapse the four-value enum
--    (open|contested|holding|settled) to three (open|active|settled).
--    `contested` and `holding` conflated lifecycle with debate
--    intensity; the new `temperature` column carries that signal
--    instead. Existing rows get migrated in-place: contested→active,
--    holding→active. The check constraint is dropped and re-added
--    so old values are no longer accepted.
-- 3. Temperature snapshot columns — `battles.temperature` for fast
--    list/index rendering, plus a frozen snapshot of temperature
--    and counts on each battle_summary_history row so the timeline
--    can plot evolution without re-deriving from event tables.

CREATE TABLE IF NOT EXISTS battle_summary_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    battle_id UUID NOT NULL REFERENCES battles(id) ON DELETE CASCADE,
    summary TEXT NOT NULL,
    og_summary TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_battle_summary_history_battle
    ON battle_summary_history(battle_id, created_at DESC);

ALTER TABLE battle_summary_history
    ADD COLUMN IF NOT EXISTS temperature REAL,
    ADD COLUMN IF NOT EXISTS claim_count INT,
    ADD COLUMN IF NOT EXISTS verify_count INT,
    ADD COLUMN IF NOT EXISTS refute_count INT;

ALTER TABLE battles
    ADD COLUMN IF NOT EXISTS temperature REAL NOT NULL DEFAULT 0;

-- Drop the legacy check constraint BEFORE rewriting rows. The old
-- constraint (named `holes_status_check` from the pre-rebrand schema,
-- or `battles_status_check` post-rebrand) only allows the four legacy
-- values, so an UPDATE that sets a row to 'active' would otherwise
-- fail the check.
ALTER TABLE battles DROP CONSTRAINT IF EXISTS holes_status_check;
ALTER TABLE battles DROP CONSTRAINT IF EXISTS battles_status_check;

UPDATE battles SET status = 'active' WHERE status IN ('contested', 'holding');

ALTER TABLE battles
    ADD CONSTRAINT battles_status_check
    CHECK (status IN ('open', 'active', 'settled'));
