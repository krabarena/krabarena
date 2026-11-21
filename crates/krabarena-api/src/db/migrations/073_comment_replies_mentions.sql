-- Migration 073: comment replies + @mentions.
--
-- Two NEW side tables; `comments` itself is NOT altered. Every read of
-- `comments` already uses the explicit `COMMENT_COLUMNS` list, but a
-- side table keeps this migration entirely free of the
-- `0A000: cached plan must not change result type` trap (AGENTS.md
-- "Database") regardless — no existing plan changes shape. Fully
-- idempotent (`IF NOT EXISTS` everywhere), safe under
-- `SQLX_AUTO_MIGRATE=true`.
--
-- 1. `comment_replies` — one row per reply: `comment_id` (the reply)
--    → `parent_id` (the comment it answers). Same target is enforced
--    in `comment_service` (a Claim comment can only answer a comment
--    on the same Claim; a Battle comment one on the same Battle).
--    Deliberately no `created_at` column: list queries LEFT JOIN this
--    table onto the unqualified `COMMENT_COLUMNS` list, so it must not
--    share any column name with `comments`.
--
-- 2. `comment_mentions` — one row per (comment, mentioned user),
--    resolved at insert time from `@handle` tokens in the body
--    (`services/mentions.rs`). `handle` is the normalized token as
--    written, used to turn the token into a profile link on render.
--
-- NEVER edit this file after it has run in any environment (sqlx
-- checksums are immutable).

CREATE TABLE IF NOT EXISTS comment_replies (
    comment_id UUID PRIMARY KEY REFERENCES comments(id) ON DELETE CASCADE,
    parent_id UUID NOT NULL REFERENCES comments(id) ON DELETE CASCADE,
    CHECK (comment_id <> parent_id)
);

CREATE INDEX IF NOT EXISTS idx_comment_replies_parent
    ON comment_replies (parent_id);

CREATE TABLE IF NOT EXISTS comment_mentions (
    comment_id UUID NOT NULL REFERENCES comments(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    handle TEXT NOT NULL CHECK (char_length(handle) BETWEEN 1 AND 64),
    PRIMARY KEY (comment_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_comment_mentions_user
    ON comment_mentions (user_id);
