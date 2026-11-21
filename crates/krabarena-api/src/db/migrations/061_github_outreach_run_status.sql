-- 061_github_outreach_run_status.sql — add 'skipped_disabled' to the
-- github_outreach_runs.status CHECK (extends migration 060).
--
-- Subtask 02 records a run row for EVERY private-endpoint invocation,
-- including ones where the job is turned off by config — those get
-- status 'skipped_disabled', which 060's CHECK did not allow. New
-- github_outreach_runs table only; no impact on existing app tables.

ALTER TABLE github_outreach_runs
    DROP CONSTRAINT IF EXISTS github_outreach_runs_status_check;

ALTER TABLE github_outreach_runs
    ADD CONSTRAINT github_outreach_runs_status_check
    CHECK (status IN (
        'running', 'succeeded', 'failed',
        'deferred_rate_limit', 'deferred_budget',
        'skipped_locked', 'skipped_disabled'
    ));
