-- 080_schema_drift_backfill.sql — columns that exist on prod but were
-- never created by any migration (added by hand before migrations were
-- the source of truth). A database built from migrations alone (fresh
-- self-hosted install, local Docker Postgres) was missing them and broke
-- the battle page, claim rendering and proxy logging.
--
-- Found 2026-11-19 by diffing information_schema.columns of a fresh
-- migrate-only Postgres against a Neon fork of prod.
--
-- Fully idempotent: on prod (and test-e2e, forked from prod) every
-- statement is a no-op because the columns / constraint / index already
-- exist — so no table is actually altered there and the "cached plan
-- must not change result type" hazard from AGENTS.md does not apply.
-- Types/nullability copied from prod (all nullable, no defaults).

ALTER TABLE battles ADD COLUMN IF NOT EXISTS executive_summary TEXT;

ALTER TABLE claims ADD COLUMN IF NOT EXISTS title TEXT;
ALTER TABLE claims ADD COLUMN IF NOT EXISTS summary TEXT;
ALTER TABLE claims ADD COLUMN IF NOT EXISTS methodology TEXT;
ALTER TABLE claims ADD COLUMN IF NOT EXISTS conclusion TEXT;

ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS query_params TEXT;
ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS request_headers JSONB;
ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS request_body TEXT;
ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS response_headers JSONB;
ALTER TABLE proxy_logs ADD COLUMN IF NOT EXISTS response_body TEXT;

ALTER TABLE battle_summary_history ADD COLUMN IF NOT EXISTS claim_id UUID;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'battle_summary_history_claim_id_fkey'
          AND conrelid = 'battle_summary_history'::regclass
    ) THEN
        ALTER TABLE battle_summary_history
            ADD CONSTRAINT battle_summary_history_claim_id_fkey
            FOREIGN KEY (claim_id) REFERENCES claims(id);
    END IF;
END $$;
