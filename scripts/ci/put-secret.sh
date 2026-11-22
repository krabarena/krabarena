#!/usr/bin/env bash
set -euo pipefail

secret=${1:?secret id}
project=${2:?project}
value=$(cat)
[ -n "$value" ] || { echo "Refusing to store an empty value in $secret" >&2; exit 1; }

if ! gcloud secrets describe "$secret" --project "$project" >/dev/null 2>&1; then
  gcloud secrets create "$secret" --project "$project" --replication-policy automatic \
    --labels managed-by=github-actions >/dev/null
fi
printf '%s' "$value" | gcloud secrets versions add "$secret" --project "$project" --data-file=- >/dev/null
echo "Stored a new version of $secret in $project" >&2
