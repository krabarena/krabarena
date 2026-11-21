-- 021_claim_media.sql
-- Track per-claim media (images + HTML/MD attachments) extracted
-- from `![](path)` references in the claim body and uploaded to the
-- public CDN at `bundles/<artifact_hash>/<bundle_path>`.
--
-- Image rows ('image' kind) require Vision SafeSearch to have
-- approved the bytes before insert (moderation_verdict = 'pass').
-- Attachment rows ('html_attachment' / 'markdown_attachment') are
-- text payloads and aren't Vision-moderated; their `<img>` /
-- `![]()` deps go through the image moderation pipeline before
-- the attachment row can be inserted.
--
-- Why a table, not a JSONB column on `claims`: per-row moderation
-- audit trail, content-addressable dedup by SHA per claim, and the
-- UI's Attachments section renders by joining `claim_media WHERE
-- kind <> 'image'`.

CREATE TABLE claim_media (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    claim_id UUID NOT NULL REFERENCES claims(id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL,                 -- "sha256:<hex>"
    bundle_path TEXT NOT NULL,            -- in-bundle path, structure-preserved on CDN
    public_url TEXT NOT NULL,             -- final URL exposed to readers
    mime_type TEXT NOT NULL,              -- image/png, image/jpeg, image/webp, image/gif, text/html, text/markdown
    bytes BIGINT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'image'
        CHECK (kind IN ('image', 'html_attachment', 'markdown_attachment')),
    moderation_verdict TEXT NOT NULL CHECK (moderation_verdict IN ('pass')),  -- only 'pass' rows are inserted
    moderation_categories JSONB,          -- raw Vision SafeSearch response (audit trail; null for non-image kinds)
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_claim_media_claim ON claim_media(claim_id);
CREATE UNIQUE INDEX idx_claim_media_sha_per_claim ON claim_media(claim_id, sha256);
