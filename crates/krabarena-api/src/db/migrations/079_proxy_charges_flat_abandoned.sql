-- Migration 079: every proxied call records its charge up front.
--
-- Before this, only services with a `billing` block got a
-- `proxy_charges` reservation; flat-rate calls were recorded solely by
-- the `proxy_logs` row written AFTER the upstream responded, inside a
-- detached task. A client disconnect, a cancelled handler or an
-- upstream timeout after the request was sent left no record — the
-- sponsor was billed upstream but the Battle budget never was — and the
-- flat-rate budget check ran without the per-battle lock, so
-- concurrent calls all passed it.
--
-- Now every proxied call inserts a `reserved` `proxy_charges` row under
-- the per-battle advisory lock before the upstream call, and settles it
-- afterwards. Two new `source` values:
--   * `flat`      — a service without a `billing` block, settled at its
--                   flat `cost_per_1k_cents / 1000` rate.
--   * `abandoned` — sent upstream but never completed normally (client
--                   hung up, handler dropped, upstream timeout, body over
--                   the size cap); settled at `max_per_request_cents`
--                   when set, else the flat rate.
--
-- Only the CHECK constraint changes: no column is added or retyped, so
-- the `0A000: cached plan must not change result type` trap on Neon's
-- PgBouncer backends (AGENTS.md "Database") does not apply. Readers of
-- `proxy_charges` use explicit column lists anyway. Idempotent: the
-- constraint is dropped (if present) and re-added with the full set.
--
-- NEVER edit this file after it has run in any environment (sqlx
-- checksums are immutable).

ALTER TABLE proxy_charges DROP CONSTRAINT IF EXISTS proxy_charges_source_check;
ALTER TABLE proxy_charges ADD CONSTRAINT proxy_charges_source_check
    CHECK (source IN ('reserved', 'actual', 'capped', 'fallback', 'duplicate',
                      'flat', 'abandoned'));
