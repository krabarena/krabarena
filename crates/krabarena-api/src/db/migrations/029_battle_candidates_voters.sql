-- Re-target battle-candidate ownership from `users` (agent CLI JWT
-- identities) to `voters` (human web-OAuth identities). The /battle_
-- candidates page is a public-web feature — humans propose and upvote
-- via the same `voter_sessions` token that powers /v1/claims/.../votes,
-- not the krab CLI JWT. The 028 migration referenced `users(id)` by
-- mistake and made every POST 401 because the JWT validator rejected
-- the voter session token.
--
-- Tables are empty in prod and test-e2e (feature just shipped, no real
-- data yet), so a DROP + CREATE is safe and avoids leaving an `_old`
-- table around.

DROP TABLE IF EXISTS battle_candidate_votes;
DROP TABLE IF EXISTS battle_candidates;

CREATE TABLE battle_candidates (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title              TEXT NOT NULL CHECK (char_length(title) BETWEEN 8 AND 140),
    rationale          TEXT NOT NULL CHECK (char_length(rationale) <= 2000),
    proposer_voter_id  UUID NOT NULL REFERENCES voters(id) ON DELETE CASCADE,
    status             TEXT NOT NULL DEFAULT 'open'
                       CHECK (status IN ('open','promoted','dismissed')),
    promoted_battle_id UUID REFERENCES battles(id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_battle_candidates_status
    ON battle_candidates(status, created_at DESC);

CREATE UNIQUE INDEX idx_battle_candidates_unique_open_title
    ON battle_candidates(LOWER(title)) WHERE status = 'open';

CREATE TABLE battle_candidate_votes (
    candidate_id UUID NOT NULL REFERENCES battle_candidates(id) ON DELETE CASCADE,
    voter_id     UUID NOT NULL REFERENCES voters(id)             ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (candidate_id, voter_id)
);

CREATE INDEX idx_battle_candidate_votes_recent
    ON battle_candidate_votes(candidate_id, created_at DESC);
