-- Migration 054: Discord claim monitoring state.
--
-- Stores the progress and run metadata for the internal Discord
-- monitoring job. The job itself is added in a later task; this
-- migration only creates durable state.

CREATE TABLE IF NOT EXISTS discord_monitor_checkpoints (
    kind TEXT PRIMARY KEY,
    last_created_at TIMESTAMPTZ,
    last_id UUID,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT discord_monitor_checkpoints_kind_check
        CHECK (kind IN ('new_claims', 'new_verifications'))
);

CREATE TABLE IF NOT EXISTS discord_monitor_claim_alerts (
    claim_id UUID PRIMARY KEY REFERENCES claims(id),
    first_alert_sent_at TIMESTAMPTZ,
    last_alert_sent_at TIMESTAMPTZ,
    alert_count INT NOT NULL DEFAULT 0,
    last_alert_level TEXT,
    resolved_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT discord_monitor_claim_alerts_alert_count_check
        CHECK (alert_count >= 0),
    CONSTRAINT discord_monitor_claim_alerts_last_alert_level_check
        CHECK (last_alert_level IS NULL OR last_alert_level IN ('12h', '24h', '48h+'))
);

CREATE TABLE IF NOT EXISTS discord_monitor_runs (
    id UUID PRIMARY KEY,
    started_at TIMESTAMPTZ NOT NULL,
    finished_at TIMESTAMPTZ,
    new_claims_count INT NOT NULL DEFAULT 0,
    new_verifications_count INT NOT NULL DEFAULT 0,
    unverified_claims_count INT NOT NULL DEFAULT 0,
    messages_sent INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    error_message TEXT,
    CONSTRAINT discord_monitor_runs_new_claims_count_check
        CHECK (new_claims_count >= 0),
    CONSTRAINT discord_monitor_runs_new_verifications_count_check
        CHECK (new_verifications_count >= 0),
    CONSTRAINT discord_monitor_runs_unverified_claims_count_check
        CHECK (unverified_claims_count >= 0),
    CONSTRAINT discord_monitor_runs_messages_sent_check
        CHECK (messages_sent >= 0),
    CONSTRAINT discord_monitor_runs_status_check
        CHECK (status IN ('success', 'disabled', 'failed'))
);
