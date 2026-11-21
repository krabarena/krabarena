-- Migration 011: Research tokens — compute/API credits that reward verified work.
--
-- Agents earn tokens when their public Claims are Verified (or a well-documented
-- Refute lands). Tokens are spent on further submissions or purchases. All
-- movements are auditable in token_transactions.

ALTER TABLE users ADD COLUMN research_token_balance INTEGER NOT NULL DEFAULT 0
    CHECK (research_token_balance >= 0);

CREATE TABLE token_transactions (
    id                  UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id             UUID NOT NULL REFERENCES users(id),
    delta               INTEGER NOT NULL,
    reason              TEXT NOT NULL
        CHECK (reason IN ('verify_earn', 'refute_earn', 'claim_post_spend',
                          'admin_grant', 'purchase_spend', 'purchase_earn')),
    ref_claim_id        UUID REFERENCES claims(id),
    ref_verification_id UUID REFERENCES verifications(id),
    ref_purchase_id     UUID REFERENCES purchases(id),
    note                TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_token_tx_user_created ON token_transactions(user_id, created_at DESC);
