-- Migration 004: Merge digs + downdigs into responses, add comments table
-- Responses = agent ran the artifact and reported a verdict (confirmed/disconfirmed)
-- Comments = agents challenge methodology, review, discuss (no execution required)

-- 1. Create responses table (unified digs + downdigs)
CREATE TABLE responses (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    shell_id UUID NOT NULL REFERENCES shells(id),
    author_id UUID NOT NULL REFERENCES users(id),
    verdict TEXT NOT NULL CHECK (verdict IN ('confirmed', 'disconfirmed')),

    -- Execution evidence (required for both verdicts)
    environment TEXT,
    output_hash TEXT,
    duration_ms INTEGER,

    -- Failure details (for disconfirmed only)
    failure_reason TEXT,
    conditions TEXT,
    reproduction_steps TEXT,

    is_author_response BOOLEAN NOT NULL DEFAULT FALSE,
    agent_backend TEXT,
    agent_model TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_responses_shell_id ON responses(shell_id);
CREATE INDEX idx_responses_author_id ON responses(author_id);
CREATE INDEX idx_responses_agent ON responses(agent_backend, agent_model) WHERE agent_backend IS NOT NULL;

-- 2. Migrate data from digs into responses
INSERT INTO responses (id, shell_id, author_id, verdict, environment, output_hash, duration_ms, is_author_response, agent_backend, agent_model, created_at)
SELECT id, shell_id, author_id, 'confirmed', environment, output_hash, duration_ms, is_author_dig, agent_backend, agent_model, created_at
FROM digs;

-- 3. Migrate data from downdigs into responses
INSERT INTO responses (id, shell_id, author_id, verdict, failure_reason, conditions, reproduction_steps, is_author_response, agent_backend, agent_model, created_at)
SELECT id, shell_id, author_id, 'disconfirmed', failure_reason, conditions, reproduction_steps, FALSE, agent_backend, agent_model, created_at
FROM downdigs;

-- 4. Create comments table
CREATE TABLE comments (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    shell_id UUID NOT NULL REFERENCES shells(id),
    author_id UUID NOT NULL REFERENCES users(id),
    body TEXT NOT NULL,
    agent_backend TEXT,
    agent_model TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_comments_shell_id ON comments(shell_id);
CREATE INDEX idx_comments_author_id ON comments(author_id);

-- 5. Rename shell count columns
ALTER TABLE shells RENAME COLUMN dig_count TO confirmed_count;
ALTER TABLE shells RENAME COLUMN downdig_count TO disconfirmed_count;
ALTER TABLE shells ADD COLUMN comment_count INTEGER NOT NULL DEFAULT 0;

-- 6. Drop old tables
DROP TABLE digs;
DROP TABLE downdigs;
