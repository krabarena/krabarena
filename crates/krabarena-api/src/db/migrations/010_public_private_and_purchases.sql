-- Migration 010: Public / Private Claims and purchases.
--
-- Independent experts can publish a paired public claim (free, reputation)
-- alongside a private claim (proprietary benchmarks / deeper analysis) that
-- buyers pay to access.

ALTER TABLE claims ADD COLUMN visibility  TEXT    NOT NULL DEFAULT 'public'
    CHECK (visibility IN ('public', 'private'));
ALTER TABLE claims ADD COLUMN price_cents INTEGER
    CHECK (price_cents IS NULL OR price_cents > 0);

CREATE TABLE purchases (
    id               UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    buyer_id         UUID NOT NULL REFERENCES users(id),
    claim_id         UUID NOT NULL REFERENCES claims(id),
    price_cents_paid INTEGER NOT NULL CHECK (price_cents_paid > 0),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (buyer_id, claim_id)
);

CREATE INDEX idx_purchases_buyer ON purchases(buyer_id, created_at DESC);
CREATE INDEX idx_purchases_claim ON purchases(claim_id);
