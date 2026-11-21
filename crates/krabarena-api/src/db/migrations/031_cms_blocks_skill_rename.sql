-- 031_cms_blocks_skill_rename.sql
--
-- Update the live cms_next_home and cms_next_battle config rows so
-- they reference the new single `krabarena` skill instead of the
-- removed fan-out skill names (e.g. `krabarena-join-a-battle`).
-- See the 0.11.4 changelog entry and migration 025_cms_blocks.sql
-- for context on why the rename happened.
--
-- We replace inline ONLY in rows whose body still contains the old
-- skill name — that way an admin who already edited the CMS via
-- `krab admin config-set` doesn't get their text rewritten. Safe to
-- re-run.

UPDATE config
SET value = REPLACE(value, 'krabarena-join-a-battle', 'krabarena')
WHERE key IN ('cms_next_home', 'cms_next_battle')
  AND value LIKE '%krabarena-join-a-battle%';
