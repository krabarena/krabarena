-- 032_battle_images_and_claim_images.sql
-- Adds first-class "uploaded image" support for both Battles and
-- Claims so authors can attach figures / charts / cover art without
-- bundling them inside an artifact tarball.
--
-- This complements the bundle-extraction pipeline (`claim_media`,
-- migration 021): bundle-extracted images stay separate, get
-- Vision-moderated, and live at `bundles/<artifact_hash>/<path>` on
-- GCS. Direct uploads land at `images/<sha>.<ext>` and are
-- referenced from claim bodies via a short public id
-- (`![alt](img-<id>)`) that the markdown render pass resolves.
--
-- Two additions:
--
-- 1. `battles.cover_image_url` + `battles.cover_image_alt` — a
--    single optional hero image for the Battle page. Editors upload
--    via `POST /v1/battles/{id}/cover-image`; the alt text is filled
--    in by Gemini Vision on upload (fallback: filename-derived).
--    Stored as plain TEXT columns rather than a JSONB blob because
--    there's exactly one cover per Battle and most reads need both
--    values.
--
-- 2. `claim_images` — directly uploaded inline images keyed by a
--    short public_id (e.g. `img-3f9a2b1c`) that authors reference
--    from the claim body as `![alt](img-3f9a2b1c)`. The body-render
--    pass rewrites these to the resolved GCS URL.
--
-- Why a separate table and not just reuse `claim_media`:
--   - `claim_media` requires moderation_verdict = 'pass' (CHECK
--     constraint) and assumes every row has a bundle_path. The
--     direct-upload path doesn't have a bundle path; it has the
--     filename. Loosening claim_media's constraint to admit
--     pathless rows would weaken the bundle pipeline's invariants.
--   - The author-friendly public_id is the surface the markdown
--     references; claim_media.sha256 is the wrong shape to expose.

ALTER TABLE battles
    ADD COLUMN IF NOT EXISTS cover_image_url TEXT,
    ADD COLUMN IF NOT EXISTS cover_image_alt TEXT;

CREATE TABLE IF NOT EXISTS claim_images (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    claim_id UUID NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
    public_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    public_url TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    bytes BIGINT NOT NULL,
    alt_text TEXT NOT NULL DEFAULT '',
    alt_source TEXT NOT NULL DEFAULT 'filename'
        CHECK (alt_source IN ('gemini', 'filename', 'manual')),
    position INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_claim_images_claim
    ON claim_images(claim_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_claim_images_public_id_per_claim
    ON claim_images(claim_id, public_id);
