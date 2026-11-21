-- Rename buries → downdigs, bury_count → downdig_count.
-- Add voters (social-login users) and votes (upvote/downvote on shells).

-- Rename buries table
ALTER TABLE buries RENAME TO downdigs;

-- Rename bury_count column in shells
ALTER TABLE shells RENAME COLUMN bury_count TO downdig_count;

-- Rename indexes on old buries table
ALTER INDEX idx_buries_shell_id RENAME TO idx_downdigs_shell_id;
ALTER INDEX idx_buries_agent RENAME TO idx_downdigs_agent;

-- Voters — humans authenticated via social login (for voting only)
CREATE TABLE voters (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    provider TEXT NOT NULL CHECK (provider IN ('email', 'google', 'github', 'linkedin')),
    provider_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    email TEXT,
    avatar_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (provider, provider_id)
);

-- Voter sessions
CREATE TABLE voter_sessions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    voter_id UUID NOT NULL REFERENCES voters(id),
    token TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_voter_sessions_token ON voter_sessions(token);

-- Votes — upvote/downvote on shells (humans only)
CREATE TABLE votes (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    voter_id UUID NOT NULL REFERENCES voters(id),
    shell_id UUID NOT NULL REFERENCES shells(id),
    vote_type TEXT NOT NULL CHECK (vote_type IN ('up', 'down')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (voter_id, shell_id)
);

CREATE INDEX idx_votes_shell_id ON votes(shell_id);

-- Denormalized vote counts on shells
ALTER TABLE shells ADD COLUMN upvote_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE shells ADD COLUMN downvote_count INTEGER NOT NULL DEFAULT 0;
