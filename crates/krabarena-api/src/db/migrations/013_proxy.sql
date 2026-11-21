-- Migration 013: Battle proxy — sponsored API access for claims
-- Sponsors provide API keys, claimers use them through KrabArena's proxy.
-- All traffic is logged for reproducibility and billing.

-- 1. Proxy configuration on battles (JSONB, never exposed to claimers)
ALTER TABLE battles ADD COLUMN IF NOT EXISTS proxy_spec JSONB;

-- 2. Proxy request log (every proxied API call)
CREATE TABLE proxy_logs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    battle_id UUID NOT NULL REFERENCES battles(id),
    user_id UUID NOT NULL REFERENCES users(id),
    claim_id UUID REFERENCES claims(id),
    service_name TEXT NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    request_body_hash TEXT,
    response_status INTEGER NOT NULL,
    response_body_size INTEGER,
    latency_ms INTEGER,
    cost_cents INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_proxy_logs_battle ON proxy_logs(battle_id, created_at DESC);
CREATE INDEX idx_proxy_logs_user ON proxy_logs(user_id, created_at DESC);

-- 3. Budget tracking (aggregated spend per battle)
CREATE TABLE proxy_budgets (
    battle_id UUID PRIMARY KEY REFERENCES battles(id),
    total_spent_cents INTEGER NOT NULL DEFAULT 0,
    total_requests INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 4. Public/private artifact split for claims
ALTER TABLE claims ADD COLUMN IF NOT EXISTS public_artifact_url TEXT;
ALTER TABLE claims ADD COLUMN IF NOT EXISTS private_artifact_hash TEXT;
