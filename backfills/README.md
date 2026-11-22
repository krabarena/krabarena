# Prod backfills

A backfill is a one-off data change in the prod database: filling a new
column for old rows, fixing records broken by a bug, re-deriving cached
values. Schema changes are never backfills; they are migrations in
`crates/krabarena-api/src/db/migrations/`.

Backfills run only through the `Prod Backfill` workflow. Nobody needs
database credentials or GCP access to run one, and nobody should run one
any other way.

## Policy

1. **Reviewed first.** Add `backfills/YYYY-MM-DD_short_name.sql` in a
   pull request. It ships to `main` like any other change; the workflow
   refuses scripts that are not on `main`.
2. **Dry run on prod data.** Run the workflow with `apply` off. It forks a
   fresh Neon branch from prod, runs the script there in one transaction,
   prints the output to the job summary, and deletes the branch.
3. **Approved apply.** Run it again with `apply` on. The dry run repeats,
   then the `prod-data` environment waits for a required reviewer to
   approve the deployment in GitHub. The script runs on prod in a single
   transaction together with a row in `_backfills`.
4. **Exactly once.** `_backfills` records the name, script hash, actor and
   run URL. A backfill whose name is already recorded is refused, on the
   dry-run copy and on prod.

## Writing a backfill

- Plain SQL, no transaction control (`BEGIN`/`COMMIT`): the runner wraps
  the whole file in one transaction, so any error rolls everything back.
- Idempotent where possible (`WHERE new_col IS NULL`), so a dry run on a
  copy that already has newer data behaves the same.
- Bounded: touch only the rows you mean to. Prefer `UPDATE … WHERE id IN
  (…)` or a precise predicate over table-wide updates.
- Report what changed: end with a `SELECT count(*) …` so the dry run and
  the apply both show the numbers in the job summary.
- No DDL, no secrets, no `COPY` to or from files.
- Long-running changes (millions of rows) need batching discussed in the
  PR before they run; one transaction holds locks until it commits.

## Running

```sh
gh workflow run "Prod Backfill" -f script=backfills/2026-11-22_fill_claim_slugs.sql
gh workflow run "Prod Backfill" -f script=backfills/2026-11-22_fill_claim_slugs.sql -f apply=true
```

Approve the pending `prod-data` deployment on the run page (or with
`gh api repos/krabarena/krabarena/actions/runs/<run-id>/pending_deployments`).
