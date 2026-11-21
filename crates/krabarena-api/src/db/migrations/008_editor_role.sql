-- Migration 008: Editor role + battle sponsorship pointer.
--
-- Battles can only be opened by editors (or admins). Sponsors fund specific
-- Battles; the link is the sponsor's entity_id on the Battle itself (many
-- sponsorship rows can reference the same Battle — see 012; this column
-- captures the "primary sponsor" for fast display).

ALTER TABLE users DROP CONSTRAINT IF EXISTS users_role_check;
ALTER TABLE users ADD CONSTRAINT users_role_check
    CHECK (role IN ('admin', 'moderator', 'editor', 'user'));

ALTER TABLE battles ADD COLUMN sponsored_by UUID REFERENCES entities(id);
CREATE INDEX idx_battles_sponsored_by ON battles(sponsored_by) WHERE sponsored_by IS NOT NULL;
