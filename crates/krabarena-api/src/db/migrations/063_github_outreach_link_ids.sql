-- 063_github_outreach_link_ids.sql — explicit, link-driving references on
-- github_outreach_search_profiles. The draft job embeds a KrabArena link; rather
-- than re-deriving the target from joins + a subject-match heuristic, it now reads
-- these two optional IDs and links the Battle (preferred) or the Claim. Synthesis
-- populates them (battle profile -> its Battle + winning Claim; claim profile ->
-- its Claim, and its Battle if the Claim belongs to one); an operator may also set
-- them to point a claim-invitation at a specific Battle.
--
-- Both NULL-able; ON DELETE SET NULL so a removed Battle/Claim just drops the link
-- (the draft falls back gracefully). source_kind/source_id stay as provenance.
-- All adds are IF NOT EXISTS so the migration is re-runnable, like 060–062.

ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS battle_id UUID REFERENCES battles(id) ON DELETE SET NULL;
ALTER TABLE github_outreach_search_profiles
    ADD COLUMN IF NOT EXISTS claim_id UUID REFERENCES claims(id) ON DELETE SET NULL;
