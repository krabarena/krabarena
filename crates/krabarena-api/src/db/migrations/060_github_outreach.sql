-- 060_github_outreach.sql — data model for the deterministic GitHub
-- outreach orchestrator (subtask 01 of the github-pr-campaign).
--
-- The campaign runs as four periodic jobs — discover -> verify -> draft
-- -> publish — over two outreach kinds (`battle_recommendation` and
-- `claim_invitation`). This migration adds ONLY the storage layer those
-- jobs share; the jobs, GitHub client, LLM prompts, private endpoints and
-- Terraform land in later subtasks.
--
-- Backward compatible: NEW tables only, no ALTER on existing tables and no
-- `SELECT *` change, so the `0A000 cached plan` PgBouncer recycle (AGENTS.md
-- "Database") does NOT apply here.
--
-- Entities are kept deliberately separate per the spec: candidate (the repo
-- we're considering), evidence (why), the LLM verdict (on the candidate),
-- the draft, the publish queue item, and the publication fact.
--
-- DEDUPE KEY NOTE: the candidate stable key is
--   (outreach_kind, repo_full_name, battle_slug, technology_pair_key, claim_topic)
-- but `battle_slug`, `technology_pair_key` and `claim_topic` are empty for
-- some outreach kinds. Postgres treats NULLs as DISTINCT in a UNIQUE index,
-- which would let duplicates slip through, so those columns are
-- `NOT NULL DEFAULT ''` empty-string sentinels — the application normalizes
-- "absent" to '' before insert. Same reasoning for suppression `value` and
-- the publication dedupe key.
--
-- SCALE NOTE: all ids are BIGSERIAL (monotonic, keyset-pagination friendly).
-- Hot job selections (pending verification, dispatchable queue items) are
-- backed by composite indexes on indexable columns — never a JSONB scan.

-- ---------------------------------------------------------------------------
-- Run history. One row per periodic-job invocation. `partition_key` defaults
-- to 'global' for the first release but is present so discovery/verification/
-- drafting can later run independent partitions without a schema change.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_runs (
    id            BIGSERIAL    PRIMARY KEY,
    job_name      TEXT         NOT NULL,
    partition_key TEXT         NOT NULL DEFAULT 'global',
    status        TEXT         NOT NULL DEFAULT 'running'
                  CHECK (status IN (
                      'running', 'succeeded', 'failed',
                      'deferred_rate_limit', 'deferred_budget', 'skipped_locked'
                  )),
    started_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    finished_at   TIMESTAMPTZ,
    stats_json    JSONB        NOT NULL DEFAULT '{}'::jsonb,
    error         TEXT,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_github_outreach_runs_job
    ON github_outreach_runs (job_name, partition_key, started_at DESC);

-- ---------------------------------------------------------------------------
-- Search profiles. A profile is one configured search: either a battle/
-- technology pair (`battle_recommendation`) or a popularity/project filter
-- (`claim_invitation`). Deterministic filters live in plain columns; list
-- filters (languages, topics, query fragments) live in JSONB.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_search_profiles (
    id                      BIGSERIAL   PRIMARY KEY,
    name                    TEXT        NOT NULL,
    enabled                 BOOLEAN     NOT NULL DEFAULT true,
    outreach_kind           TEXT        NOT NULL
                            CHECK (outreach_kind IN ('battle_recommendation', 'claim_invitation')),
    battle_id               UUID        REFERENCES battles(id) ON DELETE SET NULL,
    battle_slug             TEXT        NOT NULL DEFAULT '',
    winner_technology       TEXT        NOT NULL DEFAULT '',
    loser_technology        TEXT        NOT NULL DEFAULT '',
    claim_invitation_kind   TEXT        NOT NULL DEFAULT 'none'
                            CHECK (claim_invitation_kind IN (
                                'none', 'technology_claim', 'project_claim', 'community_claim'
                            )),
    claim_topic             TEXT        NOT NULL DEFAULT '',
    target_project_name     TEXT,
    target_project_url      TEXT,
    required_languages_json JSONB       NOT NULL DEFAULT '[]'::jsonb,
    required_topics_json    JSONB       NOT NULL DEFAULT '[]'::jsonb,
    include_queries_json    JSONB       NOT NULL DEFAULT '[]'::jsonb,
    exclude_queries_json    JSONB       NOT NULL DEFAULT '[]'::jsonb,
    min_stars               INTEGER,
    max_stars               INTEGER,
    max_forks               INTEGER,
    pushed_after            TIMESTAMPTZ,
    max_repo_age_days        INTEGER,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (name)
);

CREATE INDEX IF NOT EXISTS idx_github_outreach_search_profiles_enabled
    ON github_outreach_search_profiles (enabled, outreach_kind);

-- ---------------------------------------------------------------------------
-- Repository candidates — the central entity. `status` tracks the overall
-- lifecycle position (transition-validated app-side); `deterministic_status`
-- and `llm_review_status` hold the per-stage verdict detail. The verify job
-- selects its work by `llm_review_status = 'llm_review_pending'`.
--
-- Stable key columns (outreach_kind, repo_full_name, battle_slug,
-- technology_pair_key, claim_topic) use empty-string sentinels — see header.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_repository_candidates (
    id                         BIGSERIAL   PRIMARY KEY,
    search_profile_id          BIGINT      REFERENCES github_outreach_search_profiles(id) ON DELETE SET NULL,
    outreach_kind              TEXT        NOT NULL
                               CHECK (outreach_kind IN ('battle_recommendation', 'claim_invitation')),
    -- Stable-key columns (NOT NULL '' sentinels for the optional parts).
    repo_full_name             TEXT        NOT NULL,
    battle_slug                TEXT        NOT NULL DEFAULT '',
    technology_pair_key        TEXT        NOT NULL DEFAULT '',
    claim_topic                TEXT        NOT NULL DEFAULT '',
    claim_invitation_kind      TEXT        NOT NULL DEFAULT 'none'
                               CHECK (claim_invitation_kind IN (
                                   'none', 'technology_claim', 'project_claim', 'community_claim'
                               )),
    -- Repository metadata captured at inspection time.
    owner                      TEXT        NOT NULL,
    name                       TEXT        NOT NULL,
    html_url                   TEXT,
    default_branch             TEXT,
    stars                      INTEGER,
    forks                      INTEGER,
    pushed_at                  TIMESTAMPTZ,
    created_at_github          TIMESTAMPTZ,
    archived                   BOOLEAN     NOT NULL DEFAULT false,
    disabled                   BOOLEAN     NOT NULL DEFAULT false,
    fork                       BOOLEAN     NOT NULL DEFAULT false,
    has_issues                 BOOLEAN     NOT NULL DEFAULT true,
    license_key                TEXT,
    popularity_score           INTEGER,
    discussions_enabled        BOOLEAN,
    preferred_outreach_surface TEXT        CHECK (preferred_outreach_surface IN ('issue', 'discussion', 'none')),
    project_kind               TEXT,
    need_markers_json          JSONB       NOT NULL DEFAULT '[]'::jsonb,
    no_need_markers_json       JSONB       NOT NULL DEFAULT '[]'::jsonb,
    -- Deterministic discovery verdict detail.
    deterministic_status       TEXT        CHECK (deterministic_status IN (
                                   'deterministic_passed', 'deterministic_rejected', 'deterministic_conflict'
                               )),
    deterministic_score        INTEGER,
    deterministic_reason       TEXT,
    -- LLM verification verdict detail (the verify job's work queue column).
    llm_review_status          TEXT        NOT NULL DEFAULT 'none'
                               CHECK (llm_review_status IN (
                                   'none', 'llm_review_pending', 'llm_passed',
                                   'draft_only', 'llm_rejected', 'llm_needs_more_evidence'
                               )),
    llm_review_score           INTEGER,
    llm_review_reason          TEXT,
    llm_review_json            JSONB,
    llm_prompt_version         TEXT,
    -- Overall lifecycle position + the next job action hint.
    status                     TEXT        NOT NULL DEFAULT 'discovered'
                               CHECK (status IN (
                                   'discovered', 'deterministic_rejected', 'deterministic_passed',
                                   'llm_review_pending', 'llm_passed', 'llm_rejected',
                                   'llm_needs_more_evidence', 'draft_only', 'draft_ready',
                                   'queued_for_publish', 'published',
                                   'send_failed_retryable', 'send_failed_final', 'suppressed'
                               )),
    next_action                TEXT,
    first_seen_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_reviewed_at           TIMESTAMPTZ,
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (outreach_kind, repo_full_name, battle_slug, technology_pair_key, claim_topic)
);

-- Verify-job work queue: pending candidates, oldest id first (keyset). The
-- `id` tiebreaker keeps `... ORDER BY id LIMIT n FOR UPDATE SKIP LOCKED` an
-- index range scan rather than a full scan even with hundreds of thousands
-- of rows.
CREATE INDEX IF NOT EXISTS idx_github_outreach_candidates_llm_review
    ON github_outreach_repository_candidates (llm_review_status, id);
CREATE INDEX IF NOT EXISTS idx_github_outreach_candidates_kind
    ON github_outreach_repository_candidates (outreach_kind);
CREATE INDEX IF NOT EXISTS idx_github_outreach_candidates_status_next
    ON github_outreach_repository_candidates (status, next_action);
CREATE INDEX IF NOT EXISTS idx_github_outreach_candidates_profile_seen
    ON github_outreach_repository_candidates (search_profile_id, last_seen_at);

-- ---------------------------------------------------------------------------
-- Evidence snippets supporting a candidate's deterministic verdict.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_repository_evidence (
    id            BIGSERIAL    PRIMARY KEY,
    candidate_id  BIGINT       NOT NULL
                  REFERENCES github_outreach_repository_candidates(id) ON DELETE CASCADE,
    outreach_kind TEXT         NOT NULL
                  CHECK (outreach_kind IN ('battle_recommendation', 'claim_invitation')),
    source_kind   TEXT         NOT NULL,
    file_path     TEXT,
    url           TEXT,
    snippet       TEXT,
    signal        TEXT,
    strength      TEXT         CHECK (strength IN ('weak', 'medium', 'strong')),
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_github_outreach_evidence_candidate
    ON github_outreach_repository_evidence (candidate_id);

-- ---------------------------------------------------------------------------
-- Issue/discussion drafts. One draft per candidate (re-drafting replaces).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_issue_drafts (
    id                  BIGSERIAL    PRIMARY KEY,
    candidate_id        BIGINT       NOT NULL
                        REFERENCES github_outreach_repository_candidates(id) ON DELETE CASCADE,
    outreach_kind       TEXT         NOT NULL
                        CHECK (outreach_kind IN ('battle_recommendation', 'claim_invitation')),
    repo_full_name      TEXT         NOT NULL,
    title               TEXT         NOT NULL,
    body                TEXT         NOT NULL,
    battle_url          TEXT,
    battle_url_with_utm TEXT,
    claim_creation_url  TEXT,
    target_surface      TEXT         NOT NULL DEFAULT 'issue'
                        CHECK (target_surface IN ('issue', 'discussion')),
    discussion_category TEXT,
    dedupe_marker       TEXT         NOT NULL,
    llm_model           TEXT,
    llm_prompt_version  TEXT,
    status              TEXT         NOT NULL DEFAULT 'draft_ready'
                        CHECK (status IN ('draft_ready', 'draft_rejected', 'superseded')),
    rejection_reason    TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    UNIQUE (candidate_id)
);

CREATE INDEX IF NOT EXISTS idx_github_outreach_drafts_status
    ON github_outreach_issue_drafts (status);
CREATE INDEX IF NOT EXISTS idx_github_outreach_drafts_kind_surface
    ON github_outreach_issue_drafts (outreach_kind, target_surface);

-- ---------------------------------------------------------------------------
-- Publish queue. One queue item per draft. Dispatch reads approved, due,
-- not-yet-sent items by (approval_status, send_status, send_after_at).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_publish_queue (
    id                  BIGSERIAL    PRIMARY KEY,
    draft_id            BIGINT       NOT NULL
                        REFERENCES github_outreach_issue_drafts(id) ON DELETE CASCADE,
    outreach_kind       TEXT         NOT NULL
                        CHECK (outreach_kind IN ('battle_recommendation', 'claim_invitation')),
    repo_full_name      TEXT         NOT NULL,
    target_surface      TEXT         NOT NULL
                        CHECK (target_surface IN ('issue', 'discussion')),
    discussion_category TEXT,
    send_after_at       TIMESTAMPTZ  NOT NULL DEFAULT now(),
    approval_status     TEXT         NOT NULL DEFAULT 'pending_approval'
                        CHECK (approval_status IN ('pending_approval', 'approved', 'rejected')),
    send_status         TEXT         NOT NULL DEFAULT 'queued'
                        CHECK (send_status IN (
                            'queued', 'sending', 'sent',
                            'send_failed_retryable', 'send_failed_final'
                        )),
    send_attempts       INTEGER      NOT NULL DEFAULT 0,
    sent_url            TEXT,
    sent_number         INTEGER,
    last_error          TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    UNIQUE (draft_id)
);

CREATE INDEX IF NOT EXISTS idx_github_outreach_queue_dispatch
    ON github_outreach_publish_queue (approval_status, send_status, send_after_at);

-- ---------------------------------------------------------------------------
-- Publication facts. Permanent audit record of a created issue/discussion.
-- `draft_id` is ON DELETE SET NULL so the published fact survives cleanup of
-- the candidate/draft chain. `dedupe_marker` is copied from the draft so the
-- publication unique key can be enforced here.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_published_issues (
    id                  BIGSERIAL    PRIMARY KEY,
    draft_id            BIGINT       REFERENCES github_outreach_issue_drafts(id) ON DELETE SET NULL,
    outreach_kind       TEXT         NOT NULL
                        CHECK (outreach_kind IN ('battle_recommendation', 'claim_invitation')),
    repo_full_name      TEXT         NOT NULL,
    target_surface      TEXT         NOT NULL
                        CHECK (target_surface IN ('issue', 'discussion')),
    published_url       TEXT         NOT NULL,
    published_number    INTEGER,
    discussion_category TEXT,
    dedupe_marker       TEXT         NOT NULL DEFAULT '',
    sender_login        TEXT,
    sent_at             TIMESTAMPTZ  NOT NULL DEFAULT now(),
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    UNIQUE (outreach_kind, repo_full_name, dedupe_marker)
);

CREATE INDEX IF NOT EXISTS idx_github_outreach_published_repo
    ON github_outreach_published_issues (outreach_kind, repo_full_name);

-- ---------------------------------------------------------------------------
-- Suppressions. `value` is '' for the 'global' scope (empty-string sentinel
-- so (scope, value) stays uniquely enforceable).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_suppressions (
    id         BIGSERIAL    PRIMARY KEY,
    scope      TEXT         NOT NULL
               CHECK (scope IN (
                   'repo', 'owner', 'outreach_kind', 'technology',
                   'battle', 'claim_topic', 'sender', 'global'
               )),
    value      TEXT         NOT NULL DEFAULT '',
    reason     TEXT,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
    UNIQUE (scope, value)
);

-- ---------------------------------------------------------------------------
-- API events — GitHub/LLM call telemetry + rate-limit headers. High volume;
-- needs a retention/rollup policy (later subtask). Indexed by time for
-- retention sweeps and by (job_name, time) for per-job inspection.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_api_events (
    id                    BIGSERIAL    PRIMARY KEY,
    job_name              TEXT         NOT NULL,
    provider              TEXT         NOT NULL DEFAULT 'github',
    endpoint_kind         TEXT,
    method                TEXT,
    status_code           INTEGER,
    rate_limit_resource   TEXT,
    rate_limit_remaining  INTEGER,
    rate_limit_reset_at   TIMESTAMPTZ,
    retry_after_seconds   INTEGER,
    duration_ms           INTEGER,
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_github_outreach_api_events_created
    ON github_outreach_api_events (created_at);
CREATE INDEX IF NOT EXISTS idx_github_outreach_api_events_job_created
    ON github_outreach_api_events (job_name, created_at);

-- ---------------------------------------------------------------------------
-- Search cursors — per (profile, query) pagination progress so discovery
-- resumes a large corpus instead of restarting each run.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_search_cursors (
    id                  BIGSERIAL    PRIMARY KEY,
    search_profile_id   BIGINT       NOT NULL
                        REFERENCES github_outreach_search_profiles(id) ON DELETE CASCADE,
    query_hash          TEXT         NOT NULL,
    next_page_or_cursor TEXT,
    next_allowed_at     TIMESTAMPTZ,
    last_success_at     TIMESTAMPTZ,
    last_error          TEXT,
    created_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ  NOT NULL DEFAULT now(),
    UNIQUE (search_profile_id, query_hash)
);

-- ---------------------------------------------------------------------------
-- Shared rate-limit budgets — last-known GitHub/LLM budget per resource, so
-- multiple endpoint invocations spend a single coordinated budget.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS github_outreach_rate_limit_budgets (
    id         BIGSERIAL    PRIMARY KEY,
    provider   TEXT         NOT NULL,
    resource   TEXT         NOT NULL,
    remaining  INTEGER,
    reset_at   TIMESTAMPTZ,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
    UNIQUE (provider, resource)
);
