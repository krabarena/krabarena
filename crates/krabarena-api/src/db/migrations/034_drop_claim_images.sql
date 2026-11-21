-- 034_drop_claim_images.sql
--
-- Drop the `claim_images` table introduced in migration 033. Images
-- in claim bodies are now expressed in markdown directly — authors
-- upload via `POST /v1/images` (returns {url, suggested_alt}) and
-- paste the resulting `![alt](url)` snippet. No DB row per image,
-- no `img-<id>` indirection, no FK to clean up.
--
-- The `battles.cover_image_url` / `cover_image_alt` columns from
-- migration 033 STAY — battles get exactly one cover image and the
-- single-column shape carries its weight.
--
-- Safe to apply: prod's `claim_images` had 0 rows at deprecation
-- time (no claim used `img-<id>` markdown syntax in its body).

DROP TABLE IF EXISTS claim_images;
