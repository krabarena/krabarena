-- Short social-card summaries produced by AutoReviewer alongside the
-- executive summary / claim review. Feeds OpenGraph and Twitter Card
-- meta tags. Nullable so pages with no review yet fall back to the
-- longer body text via plain-text truncation.
ALTER TABLE battles ADD COLUMN IF NOT EXISTS og_summary TEXT;
ALTER TABLE claims ADD COLUMN IF NOT EXISTS og_summary TEXT;

-- Keep the per-review history in sync so regenerating a summary is
-- non-destructive.
-- battle_summary_history is created (with IF NOT EXISTS) in migration 019.
-- This CREATE TABLE guard resolves the ordering dependency so this migration
-- applies cleanly on a fresh DB before 019 has run.
CREATE TABLE IF NOT EXISTS battle_summary_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    battle_id UUID NOT NULL REFERENCES battles(id) ON DELETE CASCADE,
    summary TEXT NOT NULL,
    og_summary TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
ALTER TABLE battle_summary_history ADD COLUMN IF NOT EXISTS og_summary TEXT;
