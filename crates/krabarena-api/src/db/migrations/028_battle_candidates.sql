-- Battle candidates: anyone proposes a Battle title; logged-in users
-- upvote. Editors curate the top-of-list into real Battles manually.
-- This is opt-in idea-flow, not a Battle-status. A candidate has no
-- proxy_spec / domain_tags / claim infra — those land when an editor
-- promotes it via the existing battle-create UI.

CREATE TABLE IF NOT EXISTS battle_candidates (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title              TEXT NOT NULL CHECK (char_length(title) BETWEEN 8 AND 140),
    rationale          TEXT NOT NULL CHECK (char_length(rationale) <= 2000),
    proposer_id        UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status             TEXT NOT NULL DEFAULT 'open'
                       CHECK (status IN ('open','promoted','dismissed')),
    promoted_battle_id UUID REFERENCES battles(id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_battle_candidates_status
    ON battle_candidates(status, created_at DESC);

-- Case-insensitive uniqueness only across `open` candidates — once one
-- is promoted or dismissed, the same title can be re-proposed.
CREATE UNIQUE INDEX IF NOT EXISTS idx_battle_candidates_unique_open_title
    ON battle_candidates(LOWER(title)) WHERE status = 'open';

CREATE TABLE IF NOT EXISTS battle_candidate_votes (
    candidate_id UUID NOT NULL REFERENCES battle_candidates(id) ON DELETE CASCADE,
    user_id      UUID NOT NULL REFERENCES users(id)              ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (candidate_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_battle_candidate_votes_recent
    ON battle_candidate_votes(candidate_id, created_at DESC);
