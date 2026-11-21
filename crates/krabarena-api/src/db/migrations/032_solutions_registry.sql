-- Migration 032: Solutions registry — competitor product favicons in
-- leaderboards.
--
-- Background: claims contain a `structure.json` with a free-text
-- `solution` per leaderboard row ("Browserless", "Lightpanda", …).
-- We rendered a coloured-initial avatar tile, which is fine for cold
-- claims but reads as generic next to real vendor logos. This table
-- is the canonical, admin-curated mapping from a normalised solution
-- name to its website and a cached favicon URL (rehosted in our own
-- GCS bucket so we don't hot-link a vendor's CDN or 404 silently when
-- they re-org their assets).
--
-- Lookup is one-shot per claim render: the route does
-- `SELECT * FROM solutions WHERE name_normalized IN (...)` against
-- the lowercased solution strings on the page. No FK from
-- leaderboard rows (they're inside `claim_structure` JSONB) — the
-- match is by normalised string equality, which lets registry edits
-- light up old claims without rewriting their JSON.
--
-- `favicon_blob_key` is the canonical storage location
-- (`gs://krabarena-artifacts/favicons/<sha>.png`). `favicon_url` is
-- the publicly-served URL the template plugs into `<img src=>`.
-- We keep both so the eventual move from public-read GCS to a CDN
-- or signed URLs doesn't require touching template code.
--
-- `name_normalized` rules (mirrored in
-- `services::solutions_service::normalize_name`):
--   - lowercase
--   - strip all chars that aren't `[a-z0-9]`
-- → "chromedp/headless-shell" and "chromedp headless shell" both
--   collapse to "chromedpheadlessshell", which is the wrong shape
--   for some cases but matches the most common normalisation in the
--   leaderboard's free-text. Editors who want both forms surfaced
--   should pick the canonical display_name and rely on the registry
--   for the canonical favicon.
--
-- Schema check note: this table has no `char_length`-bounded text
-- columns, so `field_limits` / `schema_check.rs` doesn't need any
-- updates.

CREATE TABLE IF NOT EXISTS solutions (
    name_normalized  TEXT PRIMARY KEY,
    display_name     TEXT NOT NULL,
    website_url      TEXT,
    favicon_url      TEXT,
    favicon_blob_key TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_solutions_display ON solutions(display_name);
