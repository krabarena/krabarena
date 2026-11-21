#!/usr/bin/env bash
# See AGENTS.md → "Secrets: GitHub is the source of truth".
set -euo pipefail

MODE="${1:-check}"
PROJECT="${GCP_SECRETS_PROJECT:-krabbit-dev}"

MAP=(
  "DATABASE_URL=database-url"
  "JWT_SECRET=jwt-secret"
  "MAILGUN_API_KEY=mailgun-api-key"
  "MAILGUN_DOMAIN=mailgun-domain"
  "GEMINI_API_KEY=gemini-api-key"
  "CRON_SHARED_SECRET=cron-shared-secret"
  "OAUTH_GITHUB_CLIENT_ID=github-client-id"
  "OAUTH_GITHUB_CLIENT_SECRET=github-client-secret"
  "GOOGLE_CLIENT_ID=google-client-id"
  "GOOGLE_CLIENT_SECRET=google-client-secret"
)

sha() { sha256sum | cut -d' ' -f1; }
summary() { if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "$*" >> "$GITHUB_STEP_SUMMARY"; fi; echo "$*"; }

summary "### Secrets sync (\`$MODE\`, project \`$PROJECT\`)"
summary ""
summary "| GitHub secret | Secret Manager | Status |"
summary "|---|---|---|"

fail=0
for pair in "${MAP[@]}"; do
  gh_name="${pair%%=*}"
  sm_name="${pair#*=}"
  gh_val="${!gh_name:-}"

  if [ -z "$gh_val" ]; then
    summary "| \`$gh_name\` | \`$sm_name\` | ⚠️ missing in GitHub (not pushed) |"
    fail=1
    continue
  fi

  gh_hash=$(printf '%s' "$gh_val" | sha)
  if ! sm_hash=$(gcloud secrets versions access latest --secret="$sm_name" \
                   --project="$PROJECT" 2>/dev/null | sha); then
    summary "| \`$gh_name\` | \`$sm_name\` | ❌ cannot read Secret Manager (not pushed) |"
    fail=1
    continue
  fi

  if [ "$gh_hash" = "$sm_hash" ]; then
    summary "| \`$gh_name\` | \`$sm_name\` | ✅ in sync |"
    continue
  fi

  state="differs"

  if [ "$MODE" = "apply" ]; then
    if printf '%s' "$gh_val" | gcloud secrets versions add "$sm_name" \
         --project="$PROJECT" --data-file=- >/dev/null; then
      summary "| \`$gh_name\` | \`$sm_name\` | 🔄 $state → pushed new version |"
    else
      summary "| \`$gh_name\` | \`$sm_name\` | ❌ $state, push failed |"
      fail=1
    fi
  else
    summary "| \`$gh_name\` | \`$sm_name\` | ❌ $state |"
    fail=1
  fi
done

exit "$fail"
