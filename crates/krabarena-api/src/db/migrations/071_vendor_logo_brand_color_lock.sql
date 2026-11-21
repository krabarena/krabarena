-- Migration 071: admin brand-color override for vendor logos (since 0.43.3).
--
-- `vendor_logos.brand_color` is auto-extracted from the favicon
-- (`services/logo_assets.rs::dominant_color`). For some icons that picks
-- the background fill instead of the mark (TinyFish: pale-green `#d0ffab`
-- instead of the orange fish). Admins can now pin a color with
-- `POST /v1/admin/vendor-logos {"brand_color": "#rrggbb"}`; this flag
-- records that the color was set by hand so later auto-extraction
-- (vendor upsert asset fetch, `refresh-assets`) never overwrites it.
--
-- Idempotent (`ADD COLUMN IF NOT EXISTS`, constant default — metadata-only
-- on Postgres 11+). No `0A000` cached-plan trap: `vendor_logos` is only
-- ever read through explicit-column queries (`load_vendor_patterns`,
-- the admin list endpoint), never `SELECT *` (see migration 057).

-- Self-healing prelude (hotfix): migrations 055/057 were applied by hand
-- before auto-migrate existed, and the `test-e2e` Neon branch was
-- bootstrapped with them marked as applied but without the table — so a
-- bare ALTER failed there ("relation vendor_logos does not exist") and
-- crashed the test revision at startup. Recreate the 055/057 shape when
-- missing; every statement is a no-op on prod, where the table exists.
CREATE TABLE IF NOT EXISTS vendor_logos (
    id          BIGSERIAL PRIMARY KEY,
    pattern     TEXT NOT NULL,
    domain      TEXT NOT NULL,
    sort_order  INTEGER NOT NULL DEFAULT 100,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (pattern)
);
ALTER TABLE vendor_logos ADD COLUMN IF NOT EXISTS stored_url  TEXT;
ALTER TABLE vendor_logos ADD COLUMN IF NOT EXISTS brand_color TEXT;

ALTER TABLE vendor_logos
    ADD COLUMN IF NOT EXISTS brand_color_locked BOOLEAN NOT NULL DEFAULT false;
