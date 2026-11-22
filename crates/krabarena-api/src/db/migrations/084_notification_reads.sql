-- Migration 084: per-item read state for notifications.
--
-- Notifications are computed on read from the source tables
-- (activity_service::notifications_sql), so read state is stored beside
-- them: an item is read when its id is in `notification_reads` for the
-- user, or when it is not newer than `users.last_notifications_seen_at`
-- ("mark all read" watermark, migration 017). Shared by the CLI, the
-- API and the web bell, so reading anywhere marks it read everywhere.
--
-- Queried with explicit columns only. NEVER edit after it has run.

CREATE TABLE IF NOT EXISTS notification_reads (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id UUID NOT NULL,
    read_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, item_id)
);
