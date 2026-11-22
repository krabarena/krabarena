-- Migration 083: running spend totals for the Battle Proxy budget gate.
--
-- Until now every proxied call to a budget-capped Battle recomputed the
-- Battle's spend from scratch (GROUP BY over all of its proxy_logs and
-- proxy_charges) under a per-Battle advisory lock, so calls serialized
-- and got slower as history grew. `proxy_spend` keeps the sum of
-- `proxy_charges.amount_micro_usd` per Battle (user_id = nil UUID) and
-- per (Battle, Claimer). A trigger on `proxy_charges` keeps it exact for
-- every insert / settle / dedupe / release, whichever code path writes,
-- so the gate reads two rows instead of the whole history.
--
-- Read by `services::proxy_service::{reserve_charge, check_budgets}`
-- with explicit columns only (no SELECT *).
--
-- Backfilled in this same migration. CREATE TRIGGER takes a
-- SHARE ROW EXCLUSIVE lock on proxy_charges, so charges written by the
-- previous revision during the deploy either land before the backfill
-- or wait and then fire the trigger: the totals cannot drift.
--
-- NEVER edit this file after it has run in any environment.

CREATE TABLE IF NOT EXISTS proxy_spend (
    battle_id UUID NOT NULL REFERENCES battles(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    charged_micro_usd BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (battle_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_proxy_charges_open_reservations
    ON proxy_charges (battle_id, created_at)
    WHERE source = 'reserved';

CREATE OR REPLACE FUNCTION proxy_spend_apply() RETURNS trigger AS $$
DECLARE
    b UUID;
    u UUID;
    delta BIGINT;
BEGIN
    IF TG_OP = 'INSERT' THEN
        b := NEW.battle_id; u := NEW.user_id; delta := NEW.amount_micro_usd;
    ELSIF TG_OP = 'DELETE' THEN
        b := OLD.battle_id; u := OLD.user_id; delta := -OLD.amount_micro_usd;
    ELSE
        b := NEW.battle_id; u := NEW.user_id;
        delta := NEW.amount_micro_usd - OLD.amount_micro_usd;
    END IF;
    IF delta <> 0 THEN
        INSERT INTO proxy_spend (battle_id, user_id, charged_micro_usd)
        VALUES (b, '00000000-0000-0000-0000-000000000000', delta), (b, u, delta)
        ON CONFLICT (battle_id, user_id) DO UPDATE
            SET charged_micro_usd = proxy_spend.charged_micro_usd + EXCLUDED.charged_micro_usd,
                updated_at = now();
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS proxy_charges_spend ON proxy_charges;
CREATE TRIGGER proxy_charges_spend
    AFTER INSERT OR DELETE OR UPDATE OF amount_micro_usd ON proxy_charges
    FOR EACH ROW EXECUTE FUNCTION proxy_spend_apply();

DELETE FROM proxy_spend;

INSERT INTO proxy_spend (battle_id, user_id, charged_micro_usd)
SELECT battle_id, '00000000-0000-0000-0000-000000000000', SUM(amount_micro_usd)
FROM proxy_charges
GROUP BY battle_id;

INSERT INTO proxy_spend (battle_id, user_id, charged_micro_usd)
SELECT battle_id, user_id, SUM(amount_micro_usd)
FROM proxy_charges
GROUP BY battle_id, user_id;

-- Requests logged before every call reserved a charge are billed at the
-- service's flat rate. That set never grows (every call since reserves a
-- charge pointing at its log row), so its per-service counts are frozen
-- here; the gate multiplies them by the current spec's flat rate.
CREATE TABLE IF NOT EXISTS proxy_spend_legacy (
    battle_id UUID NOT NULL REFERENCES battles(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    service_name TEXT NOT NULL,
    uncharged_requests BIGINT NOT NULL,
    PRIMARY KEY (battle_id, user_id, service_name)
);

DELETE FROM proxy_spend_legacy;

INSERT INTO proxy_spend_legacy (battle_id, user_id, service_name, uncharged_requests)
SELECT l.battle_id, l.user_id, l.service_name, COUNT(*)
FROM proxy_logs l
WHERE NOT EXISTS (SELECT 1 FROM proxy_charges c WHERE c.proxy_log_id = l.id)
GROUP BY l.battle_id, l.user_id, l.service_name;

INSERT INTO proxy_spend_legacy (battle_id, user_id, service_name, uncharged_requests)
SELECT battle_id, '00000000-0000-0000-0000-000000000000', service_name, SUM(uncharged_requests)
FROM proxy_spend_legacy
GROUP BY battle_id, service_name;
