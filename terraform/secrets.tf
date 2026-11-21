#############################################################################
# Secret Manager resources for the prod Cloud Run service.
#
# Owns the secret containers + IAM grants for the Cloud Run runtime
# service account. Container values live only inside Secret Manager —
# Terraform deliberately does not see the plaintext, so .tfstate stays
# clean of credentials. `.github/workflows/deploy.yml` binds these via
# `--set-secrets KEY=secret-name:latest`, so Cloud Run resolves values
# at instance start without ever putting them in `gcloud` command
# history or audit logs.
#
# Phase 1 (provisioning) and Phase 2 (deploy.yml swap) both shipped in
# 0.9.14. Audit recommendation #6 closed.
#
# Rotation runbook:
#   printf '%s' "$NEW_VALUE" \
#     | gcloud --project=krabbit-dev secrets versions add <secret-id> \
#         --data-file=-
# The next Cloud Run revision swap (any deploy) picks up `:latest`
# automatically. To roll forward without a code change: re-run the
# `Deploy` workflow on main.
#############################################################################

locals {
  # Map of secret name → description. The Cloud Run service account is
  # granted secretAccessor on each. Values are populated by the operator.
  prod_secrets = {
    "database-url"         = "Neon PostgreSQL connection string (pooler)"
    "jwt-secret"           = "JWT signing secret"
    "mailgun-api-key"      = "Mailgun API key for transactional email"
    "mailgun-domain"       = "Mailgun sending domain"
    "gemini-api-key"       = "Google Gemini API key (KrabReviewer)"
    "cron-shared-secret"   = "Shared secret for Cloud Scheduler → Cloud Run cron auth"
    "github-client-id"     = "GitHub OAuth client ID"
    "github-client-secret" = "GitHub OAuth client secret"
    "google-client-id"     = "Google OAuth client ID"
    "google-client-secret" = "Google OAuth client secret"
    # NOTE: neither the Discord monitor bot token nor reCAPTCHA Enterprise auth
    # live here. Both ride in as Cloud Run env vars from GitHub Actions secrets
    # via `--update-env-vars` in .github/workflows/deploy.yml
    # (DISCORD_MONITOR_BOT_TOKEN / DISCORD_MONITOR_CHANNEL_ID and
    # RECAPTCHA_ENTERPRISE_API_KEY). The reCAPTCHA site key is a GitHub Actions
    # variable. No Secret Manager container for either.
  }
}

resource "google_secret_manager_secret" "prod" {
  for_each = local.prod_secrets

  project   = var.project_id
  secret_id = each.key

  replication {
    auto {}
  }

  depends_on = [google_project_service.apis]
}

# Cloud Run instances run as the default compute service account
# `<project-number>-compute@developer.gserviceaccount.com`. Look it up
# rather than hardcoding so this Terraform stays portable across
# projects.
data "google_project" "prod" {
  project_id = var.project_id
}

locals {
  prod_runtime_sa = "${data.google_project.prod.number}-compute@developer.gserviceaccount.com"
}

resource "google_secret_manager_secret_iam_member" "prod_run_access" {
  for_each = google_secret_manager_secret.prod

  project   = var.project_id
  secret_id = each.value.id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${local.prod_runtime_sa}"
}

resource "google_secret_manager_secret_iam_member" "deployer_access" {
  for_each = google_secret_manager_secret.prod

  project   = var.project_id
  secret_id = each.value.id
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:github-deployer@${var.project_id}.iam.gserviceaccount.com"
}

resource "google_secret_manager_secret_iam_member" "deployer_version_adder" {
  for_each = google_secret_manager_secret.prod

  project   = var.project_id
  secret_id = each.value.id
  role      = "roles/secretmanager.secretVersionAdder"
  member    = "serviceAccount:github-deployer@${var.project_id}.iam.gserviceaccount.com"
}
