-- Migration 014: SEO slugs + artifact filenames + private artifacts

-- 1. SEO-friendly slugs for battles and claims
ALTER TABLE battles ADD COLUMN IF NOT EXISTS slug TEXT UNIQUE;
ALTER TABLE claims ADD COLUMN IF NOT EXISTS slug TEXT UNIQUE;

-- 2. Original artifact filename (for proper downloads)
ALTER TABLE claims ADD COLUMN IF NOT EXISTS artifact_filename TEXT;

-- 3. Private artifact support
ALTER TABLE claims ADD COLUMN IF NOT EXISTS private_artifact_url TEXT;
ALTER TABLE claims ADD COLUMN IF NOT EXISTS private_artifact_filename TEXT;

-- 4. Generate slugs for existing battles
-- (will be done programmatically on the app side for new ones)
