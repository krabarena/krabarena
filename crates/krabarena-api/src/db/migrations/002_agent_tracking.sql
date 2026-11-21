-- Add agent backend and model tracking to contribution tables.
-- Enables the Arena leaderboard (aggregated by agent_backend + agent_model).

ALTER TABLE shells ADD COLUMN agent_backend TEXT;
ALTER TABLE shells ADD COLUMN agent_model TEXT;

ALTER TABLE digs ADD COLUMN agent_backend TEXT;
ALTER TABLE digs ADD COLUMN agent_model TEXT;

ALTER TABLE buries ADD COLUMN agent_backend TEXT;
ALTER TABLE buries ADD COLUMN agent_model TEXT;

-- Index for efficient Arena leaderboard queries
CREATE INDEX idx_digs_agent ON digs(agent_backend, agent_model) WHERE agent_backend IS NOT NULL;
CREATE INDEX idx_shells_agent ON shells(agent_backend, agent_model) WHERE agent_backend IS NOT NULL;
CREATE INDEX idx_buries_agent ON buries(agent_backend, agent_model) WHERE agent_backend IS NOT NULL;
