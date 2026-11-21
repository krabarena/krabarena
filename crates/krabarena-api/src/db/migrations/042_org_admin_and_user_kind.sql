-- Migration 042: Org-admin model + users.kind for bots (since 0.15.0).
--
-- Three additions, one migration:
--
-- 1. entities.admin_user_id — denormalized "primary admin" pointer
--    kept for back-compat (e.g. `GET /v1/me/org` uses it for the
--    `admin_display_name` quick-look). The canonical "is this user
--    an admin" check is now `users.org_role = 'admin'`, NOT this
--    column — multiple admins per org are supported by the role.
--    Backfill: the oldest user of an entity becomes its primary
--    admin (the person who registered the domain). NULLable + ON
--    DELETE SET NULL so deleting the admin user just clears the
--    pointer instead of cascading the whole org out of existence.
--
-- 2. users.kind — `'human'` (default) or `'bot'` for service accounts.
--    PR B will introduce the bot-creation endpoint + KrabChecker
--    service-account provisioning; landing the column now keeps the
--    schema change isolated from the new auth path. Bots can never
--    be admins (enforced at the route layer in the change-permissions
--    endpoint).
--
-- 3. users.org_role — `'admin'` or `'member'` (default). This is the
--    multi-admin model: any number of org members can hold the
--    `admin` role; the `change-permissions` endpoint promotes /
--    demotes between the two. Last-admin protection is enforced at
--    the route layer (you can't demote the only admin).
--
-- CRITICAL — this is an ALTER TABLE migration on `users` and
-- `entities`. Both tables are read via `SELECT *` in some hot paths
-- today. We're refactoring those sites to explicit column lists in
-- the same PR (auth_service.rs), so the cached-plan-trap (AGENTS.md
-- → "ALTER TABLE on prod") shouldn't bite — but still: plan to apply
-- during a low-traffic window and follow the standard runbook
-- (psql -f → let Neon compute auto-suspend → next traffic spike
-- spins up a fresh backend with no stale plans).

ALTER TABLE entities
    ADD COLUMN admin_user_id UUID REFERENCES users(id) ON DELETE SET NULL;

CREATE INDEX idx_entities_admin_user_id ON entities(admin_user_id);

-- Backfill: oldest user per entity becomes primary admin. Picks
-- deterministically via created_at ASC. If an entity has no users
-- (shouldn't happen but defensive), admin_user_id stays NULL.
UPDATE entities e
SET admin_user_id = (
  SELECT u.id FROM users u
  WHERE u.entity_id = e.id
  ORDER BY u.created_at ASC
  LIMIT 1
);

ALTER TABLE users
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'human'
        CHECK (kind IN ('human', 'bot'));

-- Partial index — bots are the rare case; we only want fast lookup
-- of "all bots in this org" / "is this user a bot". Full-table index
-- on a column that's 99.9% the same value is pure waste.
CREATE INDEX idx_users_kind ON users(kind) WHERE kind = 'bot';

-- org_role — multi-admin model. `'admin'` users can manage the org
-- (today: change other members' permissions; PR B will add bot
-- provisioning, settings, etc.). Default `'member'` so existing rows
-- and brand-new signups start non-privileged; the backfill below
-- promotes the same users that became the primary `admin_user_id`
-- above so the model is consistent on day one.
ALTER TABLE users
    ADD COLUMN org_role TEXT NOT NULL DEFAULT 'member'
        CHECK (org_role IN ('admin', 'member'));

-- Backfill: the same users picked as `entities.admin_user_id` above
-- get `org_role='admin'`. After this point the two stay in sync only
-- by convention — `users.org_role` is the canonical signal, and
-- `entities.admin_user_id` is a denormalized cache of "one primary
-- admin" updated by the change-permissions endpoint when needed.
UPDATE users
SET org_role = 'admin'
WHERE id IN (
  SELECT admin_user_id FROM entities WHERE admin_user_id IS NOT NULL
);

-- Partial index — `WHERE org_role='admin'` queries dominate (count
-- admins for last-admin protection, pick a new primary admin on
-- demotion, list admins on the member view). The vast majority of
-- rows are members, so partial-by-entity_id keeps the index small
-- and writes cheap for the common case (new member signup).
CREATE INDEX idx_users_entity_admin ON users(entity_id) WHERE org_role = 'admin';
