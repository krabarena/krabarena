-- Migration 041: Webhook subscription system (since 0.14.0).
--
-- New subsystem that lets any authenticated user register an HTTPS
-- endpoint to receive platform events (claim.posted, claim.verified,
-- claim.refuted, battle.created). Dispatch is fire-and-forget from
-- the route handler (see `services::webhook_dispatch`) so claim
-- inserts never block on subscriber latency.
--
-- Storage notes:
--   * `secret_hash` is the Argon2id hash of a 32-byte server-issued
--     HMAC secret. Kept so a future revocation / rotation flow can
--     verify a presented secret without enabling plaintext leak via
--     the API.
--   * `secret_plain` is the 64-hex-char plaintext. Required: the
--     dispatcher signs every outgoing delivery with HMAC-SHA256
--     (secret, body) and the subscriber verifies with the same
--     plaintext they were issued. Argon2 is one-way, so we cannot
--     derive plaintext from the hash at dispatch time. Industry
--     pattern (GitHub, Stripe) is to encrypt-at-rest; we accept
--     same-DB-as-everything-else risk for v1 because the spec is
--     explicit and the threat model already covers the rest of the
--     row. Subscribers are encouraged to rotate by re-subscribing.
--     This is documented in `docs/webhooks.md`.
--   * The API never re-serves `secret_plain` — the create response
--     is the one and only time the subscriber sees it.
--   * `secret_preview` is the last 4 chars of the plaintext, surfaced
--     in the list UI so users can tell their subs apart without
--     leaking the full secret.
--   * `events TEXT[]` carries the set of event names the sub wants;
--     dispatch filters via `events @> ARRAY[event_name]` against the
--     GIN index below. CHECK enforces at least one event.
--   * `delivery_count` / `failure_count` are bookkeeping; updated on
--     every delivery attempt by the dispatcher. `last_delivery_*`
--     fields surface the most recent outcome to the list UI.
--
-- Additive migration — no `ALTER TABLE` on existing tables, so the
-- PgBouncer cached-plan trap (AGENTS.md → "ALTER TABLE on prod")
-- doesn't apply. Safe to deploy without recycling Neon backends.

CREATE TABLE IF NOT EXISTS webhook_subscriptions (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_user_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    url             TEXT NOT NULL CHECK (url ~ '^https://'),
    events          TEXT[] NOT NULL CHECK (array_length(events, 1) > 0),
    secret_hash     TEXT NOT NULL,
    secret_plain    TEXT NOT NULL,
    secret_preview  TEXT NOT NULL,
    description     TEXT,
    active          BOOLEAN NOT NULL DEFAULT TRUE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_delivery_at     TIMESTAMPTZ,
    last_delivery_status TEXT,
    delivery_count       INTEGER NOT NULL DEFAULT 0,
    failure_count        INTEGER NOT NULL DEFAULT 0
);

-- Dispatch lookup: "active subs interested in event X". GIN index on
-- the events array supports the `events @> ARRAY['claim.posted']`
-- predicate the dispatcher runs on every event fire. Partial index on
-- `active = TRUE` skips soft-disabled rows (future use — v1 hard-
-- deletes so the partial filter is also a free hot/cold split).
CREATE INDEX IF NOT EXISTS idx_webhook_sub_active_event
    ON webhook_subscriptions USING GIN (events)
    WHERE active = TRUE;

-- Owner lookup: "list my subscriptions" on GET /v1/webhooks.
CREATE INDEX IF NOT EXISTS idx_webhook_sub_owner
    ON webhook_subscriptions (owner_user_id);
