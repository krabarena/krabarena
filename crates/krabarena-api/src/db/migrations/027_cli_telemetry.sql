-- 027_cli_telemetry.sql
--
-- CLI telemetry inbox. One row per CLI-side record that lands in
-- POST /v1/telemetry/cli. Two payload shapes flow here today:
--
--   1. Per-invocation records (kind = 'cli_command') — every krab
--      invocation writes one of these locally and the next krab
--      invocation opportunistically POSTs them. Carries command,
--      duration, exit_code, http_status, error_class.
--
--   2. Per-session summaries from a SessionScraper (kind =
--      'claude_code_session', 'openclaw_session', etc.) — produced
--      by `krab feedback ingest-sessions`. Carries the bucketed
--      counts + ≤5 redacted snippets.
--
-- The full payload is stored as `raw_json` (JSONB) so we don't have
-- to migrate the schema every time we add a new bucket or field on
-- the client side. The structured columns mirror the most-queried
-- fields so the dashboard rollup doesn't have to crack the JSONB
-- every row.
--
-- Retention: nothing automated yet. Estimate ~10 KB per record at
-- ~1 record per CLI invocation per active user — 1 GB/year for ~100
-- daily-active users. Add a `created_at < NOW() - INTERVAL '90 days'
-- DELETE job once we have a few weeks of data and the field-shape
-- has stabilized.

CREATE TABLE IF NOT EXISTS cli_telemetry (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    received_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- User who owned the JWT that uploaded the record. Null only on
    -- anonymous endpoints, which this one isn't — column is NOT NULL.
    user_id         UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Discriminator. 'cli_command' | 'claude_code_session' |
    -- 'openclaw_session' | 'nemoclaw_session' | 'hermes_session' |
    -- whatever future scrapers register.
    kind            TEXT        NOT NULL,
    -- Best-effort harness identity ('claude-code', 'openclaw', etc.).
    -- 'krab-cli' for native per-invocation records.
    agent           TEXT        NOT NULL,
    -- Client-side timestamp from the record (may differ from
    -- received_at by hours when records sit in the on-disk queue).
    ts              TIMESTAMPTZ NOT NULL,
    -- Stable identifier the client provides so re-uploads dedupe
    -- cleanly. Session uuid for session records; per-invocation
    -- uuid for cli_command records.
    session_id      TEXT        NOT NULL,
    -- CLI semver of the client that uploaded the record.
    krab_version    TEXT,
    -- e.g. 'darwin/aarch64'.
    os              TEXT,
    -- Full record body as JSONB — see comment at top.
    raw_json        JSONB       NOT NULL,
    -- Dedup key. If two uploads carry the same (user_id, kind,
    -- session_id) the second is a no-op via ON CONFLICT DO NOTHING.
    UNIQUE (user_id, kind, session_id)
);

-- Most common dashboard query: "what error classes are spiking in
-- the last hour/day across all users?"
CREATE INDEX IF NOT EXISTS idx_cli_telemetry_received_at
    ON cli_telemetry(received_at DESC);

-- Per-kind rollup ("how many claude-code sessions in the last week").
CREATE INDEX IF NOT EXISTS idx_cli_telemetry_kind_received
    ON cli_telemetry(kind, received_at DESC);

-- Per-user inspection (admin support flow).
CREATE INDEX IF NOT EXISTS idx_cli_telemetry_user_received
    ON cli_telemetry(user_id, received_at DESC);
