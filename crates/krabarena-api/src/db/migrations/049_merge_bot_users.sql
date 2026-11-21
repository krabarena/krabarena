-- Migration 049: Merge KrabScout + KrabChecker bot users into KrabAgent.
--
-- Background: krabarena.com had two bot users — `krabscout` (posted
-- claims + replied on X) and `krabchecker` (verified / refuted new
-- claims). The long-lived `krabagent-vm` Hermes harness (see the
-- krabarena/krabagent repo) collapses the two roles into ONE identity:
-- a single Hermes process serves whichever role the current todo
-- demands. This migration consolidates the two `users` rows into one
-- (display_name='krabagent', role='editor' so it can create battles
-- per user decision 2026-06-06) and reassigns every FK from the old
-- rows to the new one.
--
-- Idempotent:
--   * Both olds + krabagent already exist → no-op (re-run safe).
--   * One old missing → migrate the other only.
--   * Both olds missing → no-op (fresh DB / already migrated).
--   * krabagent missing → INSERT'd here.
--
-- Cached-plan-trap (AGENTS.md): this migration does NOT alter table
-- schemas (no ADD COLUMN, no DROP COLUMN, no constraint changes). All
-- statements are INSERT/UPDATE/DELETE against existing columns, so
-- the runbook's "let PgBouncer backends recycle" step is unnecessary.
--
-- FK migration list (NOT NULL → users(id) without ON DELETE CASCADE):
--   claims.author_id              · primary, every posted claim
--   verifications.author_id       · primary, every verify/refute row
--   comments.author_id            · battle + claim comments
--   claim_reviews.user_id         · pre-publish review log
--   battles.author_id             · bots haven't authored any, defensive
--   moderation_strikes.user_id    · bots have no strikes, defensive
--   purchases.buyer_id            · bots have no purchases, defensive
--   token_transactions.user_id    · bots have no token earns/spends, defensive
--   proxy_logs.user_id            · bots use proxy, expect rows
--
-- Patched 2026-06-07 (krabarena 0.19.1):
--   · Case-insensitive display_name lookup. Prod stored them as
--     'KrabScout' / 'KrabChecker' (Capitalized) while the original
--     0.19.0 file used lowercase. The actual prod cutover used an
--     inline LOWER() variant; this commit aligns the on-disk file
--     so a future fresh-DB re-run finds bots regardless of casing.
--   · Replaced UPDATE research_tokens (table never existed — the
--     name was a mistake; migration 011 created token_transactions,
--     not research_tokens) with UPDATE token_transactions. Even
--     with the typo, the original 0.19.0 run wouldn't have erred
--     on the prod DB (the inline cutover that actually applied
--     also omitted research_tokens), but the file-on-disk version
--     would have crashed any future re-run on a DB that had it
--     committed.
--
-- Tables with ON DELETE CASCADE → auto-clean on the DELETE at the
-- bottom (no UPDATE needed):
--   email_preferences, cli_telemetry, battle_candidates (proposer +
--   voter), webhook_subscriptions.
--
-- Tables with ON DELETE SET NULL → auto-nullify (no UPDATE needed):
--   entities.admin_user_id (bots can't be org admins anyway),
--   bot_prompts.updated_by_user_id.
--
-- JWT impact: every JWT minted against the old user IDs becomes
-- "valid signature, nonexistent user" the moment this migration runs.
-- The runbook (see krabarena/krabagent → REBUILD.md) requires the new
-- `krabagent-bot-token` to be in Secret Manager + the VM redeployed
-- BEFORE applying this migration so the harness never hits the gap.

DO $$
DECLARE
    v_krabscout   uuid;
    v_krabchecker uuid;
    v_krabagent   uuid;
    v_entity_id   uuid;
    v_old_ids     uuid[];
    v_migrated    int := 0;
BEGIN
    -- Case-insensitive lookup — prod stored these as 'KrabScout' /
    -- 'KrabChecker' (capitalised) but new krabagent rows go in as
    -- lowercase. LOWER() catches either casing.
    SELECT id INTO v_krabscout
    FROM users WHERE LOWER(display_name) = 'krabscout' AND kind = 'bot';

    SELECT id INTO v_krabchecker
    FROM users WHERE LOWER(display_name) = 'krabchecker' AND kind = 'bot';

    SELECT id INTO v_krabagent
    FROM users WHERE LOWER(display_name) = 'krabagent' AND kind = 'bot';

    -- Decide what to do based on what exists.
    IF v_krabagent IS NULL AND v_krabscout IS NULL AND v_krabchecker IS NULL THEN
        RAISE NOTICE 'No bot users found (no krabscout, krabchecker, or krabagent); nothing to merge';
        RETURN;
    END IF;

    -- Create krabagent if it doesn't exist yet. Inherit entity_id +
    -- auth_method from whichever old bot exists (they share the same
    -- entity in practice — the krabarena.com org's primary entity).
    IF v_krabagent IS NULL THEN
        SELECT entity_id INTO v_entity_id
        FROM users
        WHERE id = COALESCE(v_krabscout, v_krabchecker);

        IF v_entity_id IS NULL THEN
            RAISE EXCEPTION
                'cannot mint krabagent: source bot row missing entity_id (krabscout=%, krabchecker=%)',
                v_krabscout, v_krabchecker;
        END IF;

        INSERT INTO users (
            display_name, email, entity_id, auth_method, kind, role, org_role
        )
        VALUES (
            'krabagent', NULL, v_entity_id, 'bot', 'bot', 'editor', 'member'
        )
        RETURNING id INTO v_krabagent;

        RAISE NOTICE 'created new krabagent user (id=%, entity_id=%, role=editor)', v_krabagent, v_entity_id;
    ELSE
        -- krabagent already exists; ensure it has role='editor' so it
        -- can create battles. UPDATE is a no-op if already editor.
        UPDATE users SET role = 'editor'
        WHERE id = v_krabagent AND role <> 'editor';

        RAISE NOTICE 'krabagent already exists (id=%); ensured role=editor', v_krabagent;
    END IF;

    -- Collect the old IDs that actually exist.
    v_old_ids := ARRAY_REMOVE(ARRAY[v_krabscout, v_krabchecker], NULL);

    IF array_length(v_old_ids, 1) IS NULL THEN
        RAISE NOTICE 'no old bot users to migrate; krabagent (id=%) is the sole bot now', v_krabagent;
        RETURN;
    END IF;

    -- Reassign every NOT NULL FK we know about.
    UPDATE claims              SET author_id = v_krabagent WHERE author_id = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  claims:             % row(s) reassigned', v_migrated;

    UPDATE verifications       SET author_id = v_krabagent WHERE author_id = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  verifications:      % row(s) reassigned', v_migrated;

    UPDATE comments            SET author_id = v_krabagent WHERE author_id = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  comments:           % row(s) reassigned', v_migrated;

    UPDATE claim_reviews       SET user_id   = v_krabagent WHERE user_id   = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  claim_reviews:      % row(s) reassigned', v_migrated;

    UPDATE battles             SET author_id = v_krabagent WHERE author_id = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  battles:            % row(s) reassigned', v_migrated;

    UPDATE moderation_strikes  SET user_id   = v_krabagent WHERE user_id   = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  moderation_strikes: % row(s) reassigned', v_migrated;

    UPDATE purchases           SET buyer_id  = v_krabagent WHERE buyer_id  = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  purchases:          % row(s) reassigned', v_migrated;

    UPDATE token_transactions  SET user_id   = v_krabagent WHERE user_id   = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  token_transactions: % row(s) reassigned', v_migrated;

    UPDATE proxy_logs          SET user_id   = v_krabagent WHERE user_id   = ANY(v_old_ids);
    GET DIAGNOSTICS v_migrated = ROW_COUNT;
    RAISE NOTICE '  proxy_logs:         % row(s) reassigned', v_migrated;

    -- All NOT NULL FKs migrated. CASCADE + SET NULL FKs auto-handle
    -- on DELETE. If a row still references an old user_id (an FK
    -- table I missed), this DELETE will raise — and the whole DO
    -- block rolls back, leaving the merge inert. The error message
    -- names the constraint so we can add it to the UPDATE list above.
    DELETE FROM users WHERE id = ANY(v_old_ids);

    RAISE NOTICE
        'Successfully merged % old bot user(s) into krabagent (id=%); old users deleted',
        array_length(v_old_ids, 1), v_krabagent;
END $$;
