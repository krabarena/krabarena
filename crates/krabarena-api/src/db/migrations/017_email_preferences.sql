-- User email notification preferences. Absence of a row = defaults
-- (daily digest, ON). Written when a user toggles frequency via the
-- profile page or clicks the one-click unsubscribe link.
CREATE TABLE IF NOT EXISTS user_email_preferences (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    digest_frequency TEXT NOT NULL DEFAULT 'daily'
        CHECK (digest_frequency IN ('daily', 'weekly', 'off')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tracks the last time the user's Notifications feed was rendered,
-- used to compute the unread badge and highlight new items.
ALTER TABLE users ADD COLUMN IF NOT EXISTS last_notifications_seen_at TIMESTAMPTZ;
