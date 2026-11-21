-- 057_vendor_logo_assets.sql — self-host competitor logos + extract brand
-- colors.
--
-- Adds two nullable columns to `vendor_logos`:
--   stored_url  — our GCS-hosted copy of the vendor's logo (downloaded
--                 from Google's favicon service once, then served from
--                 `storage.googleapis.com/<bucket>/logos/<domain>.png`).
--                 Improves reliability + CORS vs hot-linking Google.
--   brand_color — hex (e.g. `#3572A5`) of the logo's dominant color,
--                 extracted at store time. Lets the wins-matrix pie use a
--                 real brand color instead of the hash-derived avatar
--                 stand-in.
--
-- Both stay NULL until the backfill runs
-- (`POST /v1/admin/vendor-logos/refresh-assets`); until then logo
-- resolution falls back to the Google S2 favicon URL exactly as before, so
-- this is fully backward-compatible.
--
-- Idempotent (`ADD COLUMN IF NOT EXISTS`). NO `0A000` cached-plan trap:
-- `vendor_logos` is only ever read via the explicit-column query in
-- `load_vendor_patterns` (`SELECT pattern, domain, stored_url, brand_color
-- ...`), never `SELECT *`, so adding columns can't invalidate a cached
-- plan mid-flight. Migrations are applied manually (not by this PR).

ALTER TABLE vendor_logos ADD COLUMN IF NOT EXISTS stored_url  TEXT;
ALTER TABLE vendor_logos ADD COLUMN IF NOT EXISTS brand_color TEXT;
