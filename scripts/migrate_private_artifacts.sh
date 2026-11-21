#!/usr/bin/env bash
# One-off: move existing PRIVATE claim artifacts out of the public
# `krabarena-artifacts` bucket into the non-public private bucket.
#
# Background
#   Until this fix, `POST /v1/battles/{id}/claims` with a multipart
#   `private_artifact` field uploaded the bytes to the PUBLIC bucket
#   (allUsers had roles/storage.objectViewer, which includes
#   storage.objects.list — the bucket was enumerable). The object key is
#   `sha256:<hex>` and the claim row stores
#     claims.private_artifact_url  = https://storage.googleapis.com/krabarena-artifacts/sha256:<hex>
#     claims.private_artifact_hash = sha256:<hex>
#   New uploads now go to PRIVATE_BLOB_STORE_URL and store
#   `gs://<private-bucket>/sha256:<hex>`; reads go only through the
#   authorized `GET /v1/claims/{id}/private-artifact` route.
#
# What this script does, per claim with a legacy public private-artifact URL:
#   1. Copies gs://$PUBLIC_BUCKET/<obj> -> gs://$PRIVATE_BUCKET/<obj>
#      (gcloud storage cp; skipped if the destination already exists).
#   2. Rewrites claims.private_artifact_url to gs://$PRIVATE_BUCKET/<obj>.
#   3. Deletes gs://$PUBLIC_BUCKET/<obj> — UNLESS the same content-
#      addressed object is still referenced publicly (some claim's
#      public artifact_hash / artifact_url points at it), in which case
#      the public copy is kept and a warning is printed.
#
# Safe to re-run (idempotent): already-migrated rows (gs:// URLs) are
# not selected; copies skip existing objects; deletes tolerate 404.
#
# Rollout order (do NOT run before steps 1-3):
#   1. terraform apply           (creates the private bucket + IAM, swaps
#                                 the public objectViewer binding for the
#                                 get-only custom role)
#   2. PRIVATE_BLOB_STORE_URL set on Cloud Run (deploy.yml / deploy-test.yml)
#   3. deploy the API build that contains the private-store code
#   4. run this script (dry run first, then APPLY=1)
#
# Usage:
#   DATABASE_URL='postgresql://…'  scripts/migrate_private_artifacts.sh          # dry run
#   DATABASE_URL='postgresql://…'  APPLY=1 scripts/migrate_private_artifacts.sh  # do it
#
# Env:
#   DATABASE_URL     required. Use the DIRECT (non -pooler) Neon host if possible.
#   PUBLIC_BUCKET    default krabarena-artifacts
#   PRIVATE_BUCKET   default krabarena-artifacts-private
#   APPLY            1 = perform copies / updates / deletes; anything else = dry run
#   PSQL             psql binary (default: psql on PATH, else Homebrew libpq)
#
# Requires: psql, gcloud (authenticated with storage.objects.{get,create,delete}
# on both buckets).
set -euo pipefail

: "${DATABASE_URL:?DATABASE_URL is required}"
PUBLIC_BUCKET="${PUBLIC_BUCKET:-krabarena-artifacts}"
PRIVATE_BUCKET="${PRIVATE_BUCKET:-krabarena-artifacts-private}"
APPLY="${APPLY:-0}"
PSQL="${PSQL:-$(command -v psql || echo /opt/homebrew/opt/libpq/bin/psql)}"

if [ "$PUBLIC_BUCKET" = "$PRIVATE_BUCKET" ]; then
  echo "PUBLIC_BUCKET and PRIVATE_BUCKET must differ" >&2
  exit 2
fi

run() {
  if [ "$APPLY" = "1" ]; then
    "$@"
  else
    echo "DRY-RUN: $*"
  fi
}

sql() {
  "$PSQL" "$DATABASE_URL" -v ON_ERROR_STOP=1 -At -F $'\t' "$@"
}

PREFIX="https://storage.googleapis.com/${PUBLIC_BUCKET}/"

rows=$(sql -v prefix="$PREFIX" <<'SQL'
SELECT id, substr(private_artifact_url, length(:'prefix') + 1)
  FROM claims
 WHERE private_artifact_url LIKE :'prefix' || '%'
 ORDER BY created_at;
SQL
)

if [ -z "$rows" ]; then
  echo "No legacy private artifacts in gs://${PUBLIC_BUCKET}. Nothing to do."
  exit 0
fi

count=$(printf '%s\n' "$rows" | wc -l | tr -d ' ')
echo "Found ${count} private artifact(s) in the public bucket. APPLY=${APPLY}"

moved=0
kept_public=0
while IFS=$'\t' read -r claim_id object; do
  [ -n "$claim_id" ] || continue
  case "$object" in
    ""|*/*|*..*) echo "SKIP claim ${claim_id}: unexpected object name '${object}'" >&2; continue ;;
  esac
  src="gs://${PUBLIC_BUCKET}/${object}"
  dst="gs://${PRIVATE_BUCKET}/${object}"
  echo "claim ${claim_id}: ${src} -> ${dst}"

  if gcloud storage objects describe "$dst" >/dev/null 2>&1; then
    echo "  destination exists, skipping copy"
  else
    run gcloud storage cp --no-clobber "$src" "$dst"
  fi

  if [ "$APPLY" = "1" ]; then
    gcloud storage objects describe "$dst" >/dev/null
    # psql only interpolates :'vars' in stdin/-f input, not in -c.
    sql -v id="$claim_id" -v url="$dst" >/dev/null <<'SQL'
UPDATE claims SET private_artifact_url = :'url' WHERE id = :'id'::uuid;
SQL
  else
    echo "DRY-RUN: UPDATE claims SET private_artifact_url = '${dst}' WHERE id = '${claim_id}'"
  fi

  # Content-addressed keys: the same bytes may also be some claim's
  # PUBLIC artifact. Keep the public copy in that case.
  still_public=$(sql -v obj="$object" -v url="${PREFIX}${object}" <<'SQL'
SELECT EXISTS (
  SELECT 1 FROM claims
   WHERE artifact_hash = :'obj' OR artifact_url = :'url'
);
SQL
)
  if [ "$still_public" = "t" ]; then
    echo "  WARNING: ${object} is also a public artifact; keeping ${src}"
    kept_public=$((kept_public + 1))
  else
    run gcloud storage rm "$src" || echo "  (already gone)"
  fi
  moved=$((moved + 1))
done <<< "$rows"

echo "Done. processed=${moved} kept_public_copy=${kept_public} APPLY=${APPLY}"
