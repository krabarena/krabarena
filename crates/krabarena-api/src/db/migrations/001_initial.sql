-- Krabbit MVP: Initial schema

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- Entities (organizations / domains)
CREATE TABLE entities (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name TEXT NOT NULL,
    domain TEXT NOT NULL UNIQUE,
    verification_method TEXT NOT NULL CHECK (verification_method IN ('email', 'dns')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Users (contributors linked to entities)
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    entity_id UUID NOT NULL REFERENCES entities(id),
    display_name TEXT NOT NULL,
    email TEXT UNIQUE,
    auth_method TEXT NOT NULL CHECK (auth_method IN ('email', 'dns')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_users_entity_id ON users(entity_id);

-- Holes (contested questions)
CREATE TABLE holes (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    author_id UUID NOT NULL REFERENCES users(id),
    parent_id UUID REFERENCES holes(id),
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 500),
    description TEXT NOT NULL CHECK (char_length(description) BETWEEN 1 AND 10000),
    domain_tags TEXT[] NOT NULL CHECK (array_length(domain_tags, 1) >= 1),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'contested', 'holding', 'settled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_holes_status_tags ON holes(status, domain_tags);
CREATE INDEX idx_holes_parent_id ON holes(parent_id);
CREATE INDEX idx_holes_updated_at ON holes(updated_at DESC);

-- Shells (falsifiable claims with artifacts)
CREATE TABLE shells (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    hole_id UUID NOT NULL REFERENCES holes(id),
    author_id UUID NOT NULL REFERENCES users(id),
    claim TEXT NOT NULL CHECK (char_length(claim) BETWEEN 1 AND 5000),
    artifact_hash TEXT NOT NULL,
    artifact_url TEXT NOT NULL,
    artifact_size BIGINT NOT NULL CHECK (artifact_size > 0 AND artifact_size <= 104857600),
    predecessor_id UUID REFERENCES shells(id),
    dig_count INTEGER NOT NULL DEFAULT 0,
    bury_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_shells_hole_id ON shells(hole_id, created_at DESC);
CREATE INDEX idx_shells_predecessor_id ON shells(predecessor_id);
CREATE INDEX idx_shells_artifact_hash ON shells(artifact_hash);

-- Digs (verification events)
CREATE TABLE digs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    shell_id UUID NOT NULL REFERENCES shells(id),
    author_id UUID NOT NULL REFERENCES users(id),
    environment TEXT NOT NULL CHECK (char_length(environment) BETWEEN 1 AND 2000),
    output_hash TEXT NOT NULL,
    duration_ms INTEGER NOT NULL CHECK (duration_ms > 0),
    is_author_dig BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_digs_shell_id ON digs(shell_id, created_at DESC);

-- Buries (refutation events)
CREATE TABLE buries (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    shell_id UUID NOT NULL REFERENCES shells(id),
    author_id UUID NOT NULL REFERENCES users(id),
    failure_reason TEXT NOT NULL CHECK (char_length(failure_reason) BETWEEN 1 AND 5000),
    conditions TEXT NOT NULL CHECK (char_length(conditions) BETWEEN 1 AND 2000),
    reproduction_steps TEXT NOT NULL CHECK (char_length(reproduction_steps) BETWEEN 1 AND 5000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_buries_shell_id ON buries(shell_id, created_at DESC);

-- Pending verifications (for email/DNS auth flow)
CREATE TABLE pending_verifications (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    verification_type TEXT NOT NULL CHECK (verification_type IN ('email', 'dns')),
    identifier TEXT NOT NULL,
    code TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_pending_verifications_identifier ON pending_verifications(identifier);
