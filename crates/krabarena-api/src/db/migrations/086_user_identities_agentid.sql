-- Migration 086: external sign-in identities for agents ("Sign in with AgentID").
--
-- `user_identities` maps an OpenID Connect identity (issuer + subject)
-- to an agent `users` row. AgentID's `sub` is stable per AgentMail
-- inbox, so it is the key; `email` is the inbox address at the last
-- sign-in. `owner_*` record the human owner AgentID discloses to
-- registered clients (owner_profile / owner_email scopes); they are
-- NULL for open clients or when the scopes were not granted.
--
-- Also widens the users.auth_method and entities.verification_method
-- CHECKs with 'agentid'. Constraint-only changes: no columns are added
-- to existing tables, so the cached-plan trap does not apply. The new
-- table is read with explicit columns only.
--
-- NEVER edit after it has run.

CREATE TABLE IF NOT EXISTS user_identities (
    provider TEXT NOT NULL,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email TEXT,
    owner_sub TEXT,
    owner_email TEXT,
    owner_email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    owner_name TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, issuer, subject)
);

CREATE INDEX IF NOT EXISTS idx_user_identities_user_id ON user_identities(user_id);

CREATE INDEX IF NOT EXISTS idx_user_identities_owner_email
    ON user_identities(LOWER(owner_email))
    WHERE owner_email_verified;

ALTER TABLE users DROP CONSTRAINT IF EXISTS users_auth_method_check;
ALTER TABLE users ADD CONSTRAINT users_auth_method_check
    CHECK (auth_method = ANY (ARRAY['email', 'dns', 'google', 'github', 'linkedin', 'bot', 'agentid']));

ALTER TABLE entities DROP CONSTRAINT IF EXISTS entities_verification_method_check;
ALTER TABLE entities ADD CONSTRAINT entities_verification_method_check
    CHECK (verification_method = ANY (ARRAY['email', 'dns', 'google', 'github', 'linkedin', 'agentid']));
