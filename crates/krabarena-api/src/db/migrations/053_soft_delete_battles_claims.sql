-- Migration 053: Reversible admin soft-delete for Battles AND Claims.
--
-- Adds a nullable `deleted_at TIMESTAMPTZ` to both `battles` and
-- `claims`. NULL = live (the overwhelmingly common case); a non-NULL
-- timestamp = soft-removed by an admin via
--   DELETE /v1/admin/battles/{id}  /  POST /v1/admin/battles/{id}/restore
--   DELETE /v1/admin/claims/{id}   /  POST /v1/admin/claims/{id}/restore
-- Soft-delete is fully reversible: restore just sets the column back
-- to NULL. The row, its verifications, comments, votes, and artifact
-- bundle all stay in place; only the public read paths hide it.
--
-- Partial indexes `WHERE deleted_at IS NULL` keep the hot-path filters
-- (every public list / detail / feed / search query now appends
-- `AND deleted_at IS NULL`) cheap — the index only covers live rows,
-- which is exactly the set those queries scan.
--
-- ─────────────────────────────────────────────────────────────────────
-- ⚠️  CACHED-PLAN TRAP — READ BEFORE / AFTER APPLYING  (AGENTS.md "Database")
-- ─────────────────────────────────────────────────────────────────────
-- `battles` and `claims` BOTH have `SELECT *` read paths (the OG card
-- renderer and the web claim/battle detail handlers). Per the 0.8.20
-- incident: after an `ALTER TABLE … ADD COLUMN`, any cached prepared
-- plan that did `SELECT *` against the altered table returns
--   0A000: cached plan must not change result type
-- on every Neon PgBouncer backend that still holds the stale plan — a
-- Cloud Run revision swap is NOT enough to clear it, because sqlx in
-- the new container reuses Neon's long-lived pooler backends.
--
-- Runbook (mandatory ordering):
--   1. Apply this migration:
--        /opt/homebrew/opt/libpq/bin/psql "$DATABASE_URL" \
--          -f crates/krabarena-api/src/db/migrations/053_soft_delete_battles_claims.sql
--   2. Recycle Neon backends BEFORE the deploy carrying the new code
--      lands traffic. Easiest: let Neon's compute auto-suspend (~5min
--      idle) so the next traffic spike spins up fresh backends with no
--      cached plans; or force a compute restart via the Neon API.
--   3. Then deploy. (Optional client-side hygiene: bump a
--      `restart-after-migration=$(date +%s)` label on Cloud Run.)
--
-- This migration's own code reads explicit columns everywhere it adds
-- a query (`models::claim::CLAIM_COLUMNS` / `models::battle::BATTLE_COLUMNS`
-- both gain `deleted_at`), so the new build is plan-stable. The trap is
-- only about the *old* build's still-warm `SELECT *` plans on the
-- pooler, which step 2 clears.
--
-- Idempotent: ADD COLUMN IF NOT EXISTS + CREATE INDEX IF NOT EXISTS,
-- safe to re-run. Backfill is intentionally a no-op — every existing
-- row stays live (deleted_at defaults to NULL).

ALTER TABLE battles ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;
ALTER TABLE claims  ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

-- Partial indexes over the live set only — keeps the `deleted_at IS
-- NULL` filter on every public read cheap without bloating the index
-- with tombstoned rows.
CREATE INDEX IF NOT EXISTS idx_battles_live ON battles (id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_claims_live  ON claims  (id) WHERE deleted_at IS NULL;

-- Battle-scoped claim lookups (list claims, summary inputs, stats) all
-- filter `battle_id = $1 AND deleted_at IS NULL` — give them a covering
-- partial index so the live-claims-per-battle scan stays index-only.
CREATE INDEX IF NOT EXISTS idx_claims_battle_live
    ON claims (battle_id) WHERE deleted_at IS NULL;
