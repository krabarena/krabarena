-- 062_github_outreach_profile_synthesis.sql — provenance + watermark columns
-- on github_outreach_search_profiles so the profile-synthesis job (subtask 13)
-- can auto-generate search profiles from battles / standalone claims, upsert
-- them keyed on their source, and auto-enable only high-confidence, validated
-- ones. Extends migration 060; touches github_outreach_search_profiles only.
--
-- All adds are IF NOT EXISTS so the migration is re-runnable (idempotent), like
-- 060/061. Existing hand-authored rows default to source_kind='manual',
-- auto_synthesized=false, needs_review=false — synthesis never overwrites them.

ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS source_kind TEXT NOT NULL DEFAULT 'manual'
        CHECK (source_kind IN ('manual', 'battle', 'claim'));
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS source_id UUID;
-- The source row's updated_at captured at synthesis time. Drives the
-- changed-source watermark (re-synthesize only when source.updated_at advances).
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS source_updated_at TIMESTAMPTZ;
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS needs_review BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS auto_synthesized BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS synthesis_confidence REAL;
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS synthesis_prompt_version TEXT;
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS synthesized_at TIMESTAMPTZ;
-- Why a profile is enabled / needs_review / skipped (validation or "no decided
-- winner" reason). Observable; never auto-enabled rows store the blocker here.
-- Must never contain secrets (reasoning is derived from public battle/claim facts).
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS synthesis_reason TEXT;

-- One synthesized profile per source: lets the job upsert on
-- (source_kind, source_id) and never duplicate. Manual rows (source_id NULL)
-- are exempt via the partial predicate.
CREATE UNIQUE INDEX IF NOT EXISTS idx_github_outreach_search_profiles_source
    ON github_outreach_search_profiles (source_kind, source_id)
    WHERE source_id IS NOT NULL;
