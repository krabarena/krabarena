-- Migration 030: Field-length CHECK constraints — one schema-level
-- enforcement for every bounded text field on the platform.
--
-- Companion file: `crates/krabarena-shared/src/field_limits.rs`. All
-- caps below MUST match `Field::*.spec()` in that module. The API
-- runs a startup self-check (`db::schema_check::verify`) that queries
-- `pg_constraint` after boot; a mismatch panics the server. Drift here
-- is a hard build/boot failure, not a silent corruption risk.
--
-- Why now: a battle's `description` was found persisted at exactly
-- 1999 chars with prose cut mid-sentence ("…Single hardware spec per").
-- Provenance: pre-validation manual creation. The bug class —
-- "user-submitted text silently truncated at fixed offsets" —
-- generalizes to every length-bounded text column. This migration
-- adds CHECK constraints for every previously unbounded column AND
-- tightens existing CHECKs to match `field_limits`.
--
-- Apply procedure (per AGENTS.md "Database" section):
--   1. Run the pre-flight queries below against the target DB. If any
--      return rows, EITHER widen the cap in field_limits.rs (and
--      update this file to match) OR clean the offending rows
--      manually. Do NOT proceed with violations outstanding.
--   2. `/opt/homebrew/opt/libpq/bin/psql "$DATABASE_URL" -f \
--       crates/krabarena-api/src/db/migrations/030_field_length_constraints.sql`
--   3. No Neon pooler stale-plan dance needed — `ADD CONSTRAINT` /
--      `DROP CONSTRAINT` don't change result types, so cached SELECT
--      plans stay valid.
--
-- =========================================================================
-- PRE-FLIGHT — run these FIRST. Every query should return 0 rows
-- before the DDL below is applied. Each line is the cap from
-- field_limits.rs at time of authoring (2026-05-28).
-- =========================================================================
--
-- -- battles.title (max 500)
-- SELECT COUNT(*), MAX(char_length(title)) FROM battles WHERE char_length(title) NOT BETWEEN 1 AND 500;
-- -- battles.description (max 10000) — already has BETWEEN 1 AND 10000; included for diff parity
-- SELECT COUNT(*), MAX(char_length(description)) FROM battles WHERE char_length(description) NOT BETWEEN 1 AND 10000;
-- -- battles.slug (max 120) — column is nullable; NULL passes
-- SELECT COUNT(*), MAX(char_length(slug)) FROM battles WHERE slug IS NOT NULL AND char_length(slug) NOT BETWEEN 1 AND 120;
-- -- claims.claim (max 5000) — already has BETWEEN 1 AND 5000
-- SELECT COUNT(*), MAX(char_length(claim)) FROM claims WHERE char_length(claim) NOT BETWEEN 1 AND 5000;
-- -- claims.agent_backend (max 64) — nullable
-- SELECT COUNT(*), MAX(char_length(agent_backend)) FROM claims WHERE agent_backend IS NOT NULL AND char_length(agent_backend) NOT BETWEEN 1 AND 64;
-- -- claims.agent_model (max 128) — nullable
-- SELECT COUNT(*), MAX(char_length(agent_model)) FROM claims WHERE agent_model IS NOT NULL AND char_length(agent_model) NOT BETWEEN 1 AND 128;
-- -- verifications.* (mig 004 dropped the CHECKs that mig 001 had on digs/buries — restoring)
-- SELECT COUNT(*), MAX(char_length(environment))         FROM verifications WHERE environment        IS NOT NULL AND char_length(environment)        NOT BETWEEN 1 AND 2000;
-- SELECT COUNT(*), MAX(char_length(failure_reason))      FROM verifications WHERE failure_reason     IS NOT NULL AND char_length(failure_reason)     NOT BETWEEN 1 AND 5000;
-- SELECT COUNT(*), MAX(char_length(conditions))          FROM verifications WHERE conditions         IS NOT NULL AND char_length(conditions)         NOT BETWEEN 1 AND 2000;
-- SELECT COUNT(*), MAX(char_length(reproduction_steps))  FROM verifications WHERE reproduction_steps IS NOT NULL AND char_length(reproduction_steps) NOT BETWEEN 1 AND 50000;
-- SELECT COUNT(*), MAX(char_length(agent_backend))       FROM verifications WHERE agent_backend      IS NOT NULL AND char_length(agent_backend)      NOT BETWEEN 1 AND 64;
-- SELECT COUNT(*), MAX(char_length(agent_model))         FROM verifications WHERE agent_model        IS NOT NULL AND char_length(agent_model)        NOT BETWEEN 1 AND 128;
-- -- comments
-- SELECT COUNT(*), MAX(char_length(body))           FROM comments WHERE char_length(body) NOT BETWEEN 1 AND 10000;
-- SELECT COUNT(*), MAX(char_length(agent_backend))  FROM comments WHERE agent_backend IS NOT NULL AND char_length(agent_backend) NOT BETWEEN 1 AND 64;
-- SELECT COUNT(*), MAX(char_length(agent_model))    FROM comments WHERE agent_model   IS NOT NULL AND char_length(agent_model)   NOT BETWEEN 1 AND 128;
-- -- voters (OAuth carve-out: max only, no min — empty values from IdP must pass)
-- SELECT COUNT(*), MAX(char_length(display_name))   FROM voters WHERE char_length(display_name) > 120;
-- SELECT COUNT(*), MAX(char_length(avatar_url))     FROM voters WHERE avatar_url IS NOT NULL AND char_length(avatar_url) > 1500;
-- -- users (OAuth carve-out same as voters — no min)
-- SELECT COUNT(*), MAX(char_length(display_name))   FROM users WHERE char_length(display_name) > 120;
-- -- entities
-- SELECT COUNT(*), MAX(char_length(name))           FROM entities WHERE char_length(name) NOT BETWEEN 1 AND 120;
-- -- battle_candidates — already has CHECKs; tightening rationale min from 0 to 1
-- SELECT COUNT(*), MAX(char_length(title))     FROM battle_candidates WHERE char_length(title)     NOT BETWEEN 8 AND 140;
-- SELECT COUNT(*), MAX(char_length(rationale)) FROM battle_candidates WHERE char_length(rationale) NOT BETWEEN 1 AND 2000;
-- -- sponsorships.note — nullable
-- SELECT COUNT(*), MAX(char_length(note))      FROM sponsorships WHERE note IS NOT NULL AND char_length(note) > 2000;
-- -- token_transactions.note — nullable
-- SELECT COUNT(*), MAX(char_length(note))      FROM token_transactions WHERE note IS NOT NULL AND char_length(note) > 500;
--
-- =========================================================================
-- DDL begins. Wrapped in a single transaction so a constraint that
-- fails to add (because pre-flight wasn't run, or new data landed
-- between the probe and the apply) rolls everything back.
-- =========================================================================

BEGIN;

-- ── battles ────────────────────────────────────────────────────────
-- title / description CHECKs from migration 001 already match
-- field_limits — no change needed. slug had no CHECK; add one.
ALTER TABLE battles
    ADD CONSTRAINT battles_slug_length_ck
    CHECK (slug IS NULL OR char_length(slug) BETWEEN 1 AND 120);

-- ── battle_candidates ──────────────────────────────────────────────
-- title CHECK from 029 already matches. rationale CHECK was `<= 2000`
-- (allowed 0); field_limits says min=1. Tighten.
ALTER TABLE battle_candidates
    DROP CONSTRAINT IF EXISTS battle_candidates_rationale_check;
ALTER TABLE battle_candidates
    ADD CONSTRAINT battle_candidates_rationale_length_ck
    CHECK (char_length(rationale) BETWEEN 1 AND 2000);

-- ── claims ─────────────────────────────────────────────────────────
-- `claim` already has BETWEEN 1 AND 5000 from migration 001 — no change.
-- agent_backend / agent_model: column added in mig 002 with no CHECK.
ALTER TABLE claims
    ADD CONSTRAINT claims_agent_backend_length_ck
    CHECK (agent_backend IS NULL OR char_length(agent_backend) BETWEEN 1 AND 64);
ALTER TABLE claims
    ADD CONSTRAINT claims_agent_model_length_ck
    CHECK (agent_model IS NULL OR char_length(agent_model) BETWEEN 1 AND 128);

-- ── verifications (formerly `responses`, renamed in mig 007) ───────
-- Migration 004 created `responses` with TEXT columns and no CHECKs;
-- the constraints that lived on the predecessor `digs` / `buries`
-- tables from migration 001 were silently dropped. Restoring them
-- here, matching the original caps.
ALTER TABLE verifications
    ADD CONSTRAINT verifications_environment_length_ck
    CHECK (environment IS NULL OR char_length(environment) BETWEEN 1 AND 2000);
ALTER TABLE verifications
    ADD CONSTRAINT verifications_failure_reason_length_ck
    CHECK (failure_reason IS NULL OR char_length(failure_reason) BETWEEN 1 AND 5000);
ALTER TABLE verifications
    ADD CONSTRAINT verifications_conditions_length_ck
    CHECK (conditions IS NULL OR char_length(conditions) BETWEEN 1 AND 2000);
ALTER TABLE verifications
    ADD CONSTRAINT verifications_reproduction_steps_length_ck
    -- 50,000 not 5,000: a pre-existing prod refutation row was 30,925
    -- chars (Dima's "Do you really need a database?" claim e038e4ad —
    -- repro included full Rust + C + Cargo.toml + Makefile for an
    -- exact-reproduction contract). Keep in sync with
    -- field_limits::Field::VerificationsReproductionSteps.
    CHECK (reproduction_steps IS NULL OR char_length(reproduction_steps) BETWEEN 1 AND 50000);
ALTER TABLE verifications
    ADD CONSTRAINT verifications_agent_backend_length_ck
    CHECK (agent_backend IS NULL OR char_length(agent_backend) BETWEEN 1 AND 64);
ALTER TABLE verifications
    ADD CONSTRAINT verifications_agent_model_length_ck
    CHECK (agent_model IS NULL OR char_length(agent_model) BETWEEN 1 AND 128);

-- ── comments ───────────────────────────────────────────────────────
-- Defense-in-depth: API already validates body ≤ 10000 chars, but the
-- column had no CHECK, so a future handler that bypassed validation
-- would silently persist oversized rows. Match the platform cap.
ALTER TABLE comments
    ADD CONSTRAINT comments_body_length_ck
    CHECK (char_length(body) BETWEEN 1 AND 10000);
ALTER TABLE comments
    ADD CONSTRAINT comments_agent_backend_length_ck
    CHECK (agent_backend IS NULL OR char_length(agent_backend) BETWEEN 1 AND 64);
ALTER TABLE comments
    ADD CONSTRAINT comments_agent_model_length_ck
    CHECK (agent_model IS NULL OR char_length(agent_model) BETWEEN 1 AND 128);

-- ── voters (OAuth carve-out — max only, no min) ────────────────────
-- See `field_limits` module docstring for the carve-out rationale.
-- We CHECK the upper bound only because OAuth providers occasionally
-- return empty values and we'd rather render "" than 401 the user.
-- Intake truncates to these caps via `truncate_to_cap`, so the CHECK
-- is defense-in-depth — never the user-visible rejection path.
ALTER TABLE voters
    ADD CONSTRAINT voters_display_name_length_ck
    CHECK (char_length(display_name) <= 120);
ALTER TABLE voters
    ADD CONSTRAINT voters_avatar_url_length_ck
    -- 1,500 not 1,000: pre-existing prod + test-e2e rows already at
    -- 1,126 chars from Google's signed CDN URLs (size/expiry/format
    -- query params). Keep in sync with
    -- field_limits::Field::VotersAvatarUrl.
    CHECK (avatar_url IS NULL OR char_length(avatar_url) <= 1500);

-- ── users ──────────────────────────────────────────────────────────
-- Same OAuth carve-out for display_name (Google / GitHub / etc.).
ALTER TABLE users
    ADD CONSTRAINT users_display_name_length_ck
    CHECK (char_length(display_name) <= 120);

-- ── entities ───────────────────────────────────────────────────────
ALTER TABLE entities
    ADD CONSTRAINT entities_name_length_ck
    CHECK (char_length(name) BETWEEN 1 AND 120);

-- ── sponsorships ───────────────────────────────────────────────────
ALTER TABLE sponsorships
    ADD CONSTRAINT sponsorships_note_length_ck
    CHECK (note IS NULL OR char_length(note) <= 2000);

-- ── token_transactions ─────────────────────────────────────────────
ALTER TABLE token_transactions
    ADD CONSTRAINT token_transactions_note_length_ck
    CHECK (note IS NULL OR char_length(note) <= 500);

COMMIT;
