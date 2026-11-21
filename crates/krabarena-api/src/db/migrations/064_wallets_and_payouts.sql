-- 064_wallets_and_payouts.sql — crypto payouts to claim owners.
--
-- Two tables back the new "get paid in Toncoin when your claims get
-- verified" feature:
--
--   1. `wallets` — at most ONE crypto wallet per user (UNIQUE user_id).
--      Currency + network are kept as columns (default Toncoin / TON)
--      so additional currencies/networks (e.g. TRC-20) become data, not
--      schema changes. Only the owning user reads/writes their wallet.
--
--   2. `payout_transactions` — an append-style ledger of credits with a
--      status lifecycle (pending → paid | rejected). DELIBERATELY
--      DECOUPLED from any single earning scenario: the reason is
--      described generically via `event_type` + a polymorphic
--      (`subject_type`, `subject_id`) soft reference + a `context` JSONB
--      blob. A future "pay for some other action" needs only a new
--      `event_type` string and a `payout_service::credit(...)` call — no
--      migration, no new columns, no FK churn. There is intentionally NO
--      foreign key to claims/verifications here (soft references by id).
--
-- All real money movement and status changes are performed MANUALLY by
-- an admin via SQL (UPDATE payout_transactions SET status=…, updated_at=now()).
--
-- The char_length CHECKs MUST match krabarena_shared::field_limits
-- (Field::WalletsAddress 1..128, Field::PayoutTransactionsReason 1..500);
-- the boot-time schema_check audits them and refuses to start on drift.
-- `amount` is BIGINT whole Toncoins (the workspace sqlx has no decimal
-- feature, and money elsewhere is integer too — cf. claims.price_cents).

CREATE TABLE IF NOT EXISTS wallets (
    id         UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id    UUID NOT NULL UNIQUE REFERENCES users(id),
    address    TEXT NOT NULL CHECK (char_length(address) BETWEEN 1 AND 128),
    currency   TEXT NOT NULL DEFAULT 'TON',
    network    TEXT NOT NULL DEFAULT 'ton',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS payout_transactions (
    id             UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id        UUID NOT NULL REFERENCES users(id),
    amount         BIGINT NOT NULL CHECK (amount > 0),
    currency       TEXT NOT NULL DEFAULT 'TON',
    network        TEXT NOT NULL DEFAULT 'ton',
    status         TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'paid', 'rejected')),
    -- generic, decoupled reason for the credit
    event_type     TEXT NOT NULL,
    subject_type   TEXT,
    subject_id     UUID,
    context        JSONB NOT NULL DEFAULT '{}'::jsonb,
    reason         TEXT NOT NULL CHECK (char_length(reason) BETWEEN 1 AND 500),
    dedup_key      TEXT,
    wallet_address TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_payout_tx_user
    ON payout_transactions (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_payout_tx_subject
    ON payout_transactions (subject_type, subject_id);

-- Idempotency: at most one credit per logical event (any event_type).
-- Partial so rows without a dedup_key (manual/ad-hoc credits) never clash.
CREATE UNIQUE INDEX IF NOT EXISTS uq_payout_tx_dedup
    ON payout_transactions (dedup_key) WHERE dedup_key IS NOT NULL;
