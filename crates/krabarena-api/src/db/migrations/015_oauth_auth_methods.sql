-- Migration 015: allow OAuth provider names in auth_method /
-- verification_method.
--
-- The original CHECK constraints from 001_initial.sql only admitted
-- 'email' and 'dns'. But the Google / GitHub / LinkedIn OAuth
-- callbacks in routes/oauth.rs pass the provider name through to
-- find_or_create_{user,entity}_pub as the method, so every first-time
-- OAuth login 500s with:
--
--   new row for relation "users" violates check constraint
--   "users_auth_method_check"
--
-- This migration widens both CHECKs to include the three providers
-- the code currently uses. Existing rows (email / DNS) are unaffected.

ALTER TABLE users DROP CONSTRAINT IF EXISTS users_auth_method_check;
ALTER TABLE users ADD CONSTRAINT users_auth_method_check
    CHECK (auth_method IN ('email', 'dns', 'google', 'github', 'linkedin'));

ALTER TABLE entities DROP CONSTRAINT IF EXISTS entities_verification_method_check;
ALTER TABLE entities ADD CONSTRAINT entities_verification_method_check
    CHECK (verification_method IN ('email', 'dns', 'google', 'github', 'linkedin'));
