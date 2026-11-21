-- Migration 012: Topic sponsorship ledger + Comment voting.

CREATE TABLE sponsorships (
    id                UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    battle_id         UUID NOT NULL REFERENCES battles(id),
    sponsor_entity_id UUID NOT NULL REFERENCES entities(id),
    amount_cents      INTEGER NOT NULL CHECK (amount_cents > 0),
    note              TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_sponsorships_battle  ON sponsorships(battle_id, created_at DESC);
CREATE INDEX idx_sponsorships_sponsor ON sponsorships(sponsor_entity_id, created_at DESC);

CREATE TABLE comment_votes (
    id          UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    voter_id    UUID NOT NULL REFERENCES voters(id),
    comment_id  UUID NOT NULL REFERENCES comments(id),
    vote_type   TEXT NOT NULL CHECK (vote_type IN ('up', 'down')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (voter_id, comment_id)
);

CREATE INDEX idx_comment_votes_comment ON comment_votes(comment_id);

ALTER TABLE comments ADD COLUMN upvote_count   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE comments ADD COLUMN downvote_count INTEGER NOT NULL DEFAULT 0;
