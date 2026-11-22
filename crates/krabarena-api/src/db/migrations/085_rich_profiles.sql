-- Migration 085: rich public profiles + OAuth profile snapshots.
--
-- `user_profiles` holds the personal details shown on /agents/{id}
-- (full name, bio, website, x.com / LinkedIn / GitHub links). The
-- avatar stays in `users.avatar_url`. `edited_fields` lists fields
-- the person set by hand (never overwritten by OAuth sync);
-- `oauth_fields` lists fields whose current value came from an OAuth
-- provider (refreshed on later sign-ins). Avatar is tracked as
-- 'avatar' in both arrays.
--
-- `voter_oauth_profiles` is the latest provider snapshot per voter,
-- replaced on every OAuth sign-in.
--
-- New tables only: no ALTER on users / voters, so no cached-plan risk
-- for existing `SELECT *` readers. Both tables are read with explicit
-- column lists (services::profile_service). NEVER edit after it has run.

CREATE TABLE IF NOT EXISTS user_profiles (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    full_name TEXT CHECK (char_length(full_name) <= 120),
    bio TEXT CHECK (char_length(bio) <= 280),
    website_url TEXT CHECK (char_length(website_url) <= 300),
    x_url TEXT CHECK (char_length(x_url) <= 300),
    linkedin_url TEXT CHECK (char_length(linkedin_url) <= 300),
    github_url TEXT CHECK (char_length(github_url) <= 300),
    edited_fields TEXT[] NOT NULL DEFAULT '{}',
    oauth_fields TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS voter_oauth_profiles (
    voter_id UUID PRIMARY KEY REFERENCES voters(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    email TEXT,
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    full_name TEXT,
    given_name TEXT,
    family_name TEXT,
    username TEXT,
    profile_url TEXT,
    avatar_url TEXT,
    website_url TEXT,
    x_handle TEXT,
    bio TEXT,
    company TEXT,
    location TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS voter_oauth_profiles_email_idx
    ON voter_oauth_profiles (LOWER(email));
