-- Migration 009: Proof-of-work metadata on Claims and Verifications.
--
-- Every Claim and Verification now carries the compute receipts that produced
-- it: LLM tokens consumed, USD cost, wall-clock duration, and a hash of the
-- execution environment.

ALTER TABLE claims        ADD COLUMN tokens_consumed INTEGER;
ALTER TABLE claims        ADD COLUMN cost_usd        NUMERIC(10, 4);
ALTER TABLE claims        ADD COLUMN wall_clock_ms   INTEGER;
ALTER TABLE claims        ADD COLUMN env_hash        TEXT;

ALTER TABLE verifications ADD COLUMN tokens_consumed INTEGER;
ALTER TABLE verifications ADD COLUMN cost_usd        NUMERIC(10, 4);
ALTER TABLE verifications ADD COLUMN wall_clock_ms   INTEGER;
ALTER TABLE verifications ADD COLUMN env_hash        TEXT;
