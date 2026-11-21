-- Migration 072: actual-cost billing for the Battle Proxy.
--
-- One NEW side table; `proxy_logs` is NOT altered. `proxy_logs` is an
-- append-only diagnostic log and adding a column to it would risk the
-- `0A000: cached plan must not change result type` trap on Neon's
-- PgBouncer backends (AGENTS.md "Database"). Fully idempotent
-- (`IF NOT EXISTS` everywhere).
--
-- `proxy_charges` holds one row per proxied request to a service whose
-- proxy spec carries a `billing` block (usage-priced upstreams: Monid,
-- OpenRouter, Exa — see docs/proxy-spec.md). Amounts are integer
-- micro-dollars (1 USD = 1,000,000) so sub-cent prices like $0.004 or
-- a $0.00003 Haiku call survive exactly instead of rounding to 0¢.
--
-- Lifecycle of a row:
--   1. `reserved` — inserted under a per-battle advisory lock before the
--      upstream call, with `amount_micro_usd` = the per-request cap
--      (or the flat rate). Concurrent requests see each other's
--      reservations, so budget / per-claimer caps hold under load.
--   2. settled to `actual` / `capped` / `fallback` / `duplicate` once
--      the upstream response has been parsed. `dedupe_key` (e.g.
--      Monid `runId`) is unique per battle+service so a poll that
--      repeats a run's cost is recorded as `duplicate` with amount 0.
--   A `reserved` row that never settles (process crash, client hung
--   up mid-call) is counted at `fallback_micro_usd` once it is older
--   than 5 minutes (the upstream timeout is 120s).
--
-- Spend for a billed service = SUM(proxy_charges) + flat rate × the
-- service's `proxy_logs` rows with no charge (history from before the
-- billing block was added). Services without a billing block never get
-- charge rows and keep the flat `count × cost_per_1k_cents / 1000` math.
--
-- NEVER edit this file after it has run in any environment (sqlx
-- checksums are immutable).

CREATE TABLE IF NOT EXISTS proxy_charges (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    battle_id UUID NOT NULL REFERENCES battles(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    service_name TEXT NOT NULL,
    -- `proxy_logs.id` of the request this charge belongs to (no FK: the
    -- log row is written after the charge is reserved).
    proxy_log_id UUID,
    amount_micro_usd BIGINT NOT NULL DEFAULT 0 CHECK (amount_micro_usd >= 0),
    fallback_micro_usd BIGINT NOT NULL DEFAULT 0 CHECK (fallback_micro_usd >= 0),
    source TEXT NOT NULL DEFAULT 'reserved'
        CHECK (source IN ('reserved', 'actual', 'capped', 'fallback', 'duplicate')),
    dedupe_key TEXT,
    duplicate_of_key TEXT,
    -- Uncapped cost reported by the upstream when `source = 'capped'`.
    reported_micro_usd BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    settled_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_proxy_charges_battle
    ON proxy_charges (battle_id, service_name);
CREATE INDEX IF NOT EXISTS idx_proxy_charges_battle_user
    ON proxy_charges (battle_id, user_id);
CREATE INDEX IF NOT EXISTS idx_proxy_charges_log
    ON proxy_charges (proxy_log_id);
CREATE UNIQUE INDEX IF NOT EXISTS uq_proxy_charges_dedupe
    ON proxy_charges (battle_id, service_name, dedupe_key)
    WHERE dedupe_key IS NOT NULL;
