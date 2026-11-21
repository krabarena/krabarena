-- Migration 006: User roles + admin-configurable settings
-- Adds RBAC (admin/moderator/user) and a config table for runtime settings
-- like the AutoReviewer prompt.

-- 1. Add role column to users (all existing users default to 'user')
ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'user'
  CHECK (role IN ('admin', 'moderator', 'user'));

-- 2. Config key-value store for admin-managed settings
CREATE TABLE config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_by UUID REFERENCES users(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 3. Config change history for audit trail
CREATE TABLE config_history (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_by UUID NOT NULL REFERENCES users(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_config_history_key ON config_history(key, updated_at DESC);
