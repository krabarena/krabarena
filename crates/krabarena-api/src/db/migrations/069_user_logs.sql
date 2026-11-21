-- 069_user_logs.sql — append-only visit/login log keyed by the device cookie.
-- Feeds future feed personalization; issued alongside the ka_device cookie.
CREATE TABLE user_logs (
    id          BIGSERIAL PRIMARY KEY,
    device_id   UUID NOT NULL,
    voter_id    UUID,
    user_id     UUID,
    event       TEXT NOT NULL,      -- 'visit' | 'login' | 'logout' | 'vote'
    path        TEXT,
    method      TEXT,
    referer     TEXT,
    user_agent  TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_user_logs_device ON user_logs (device_id, created_at DESC);
CREATE INDEX idx_user_logs_voter  ON user_logs (voter_id, created_at DESC)
    WHERE voter_id IS NOT NULL;
