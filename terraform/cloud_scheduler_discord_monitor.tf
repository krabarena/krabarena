#############################################################################
# Cloud Scheduler job for the Discord claim monitor (subtask 5).
#
# Fires POST /internal/discord-monitor/run every 15 minutes.
# Auth: x-cron-secret header using the existing cron-shared-secret.
#
# The Discord bot token is NOT read here — the scheduler only authenticates
# with the cron shared secret. The bot token and channel id reach the Cloud
# Run service as env vars from GitHub Actions secrets
# (DISCORD_MONITOR_BOT_TOKEN / DISCORD_MONITOR_CHANNEL_ID) via
# --update-env-vars in deploy.yml. No Secret Manager container is used.
#
# Apply order: set the DISCORD_MONITOR_* GitHub Actions secrets → deploy the
# service (deploy.yml wires the env vars) → `terraform apply` creates this
# scheduler job.
#############################################################################

data "google_secret_manager_secret_version" "cron_shared_secret" {
  project = var.project_id
  secret  = "cron-shared-secret"
  version = "latest"
}

resource "google_service_account" "discord_monitor_scheduler" {
  project      = var.project_id
  account_id   = "discord-monitor-scheduler"
  display_name = "Discord monitor Cloud Scheduler caller"
  description  = "OIDC identity the discord-claim-monitoring scheduler invokes krabarena-api with."

  depends_on = [google_project_service.apis]
}

resource "google_cloud_run_v2_service_iam_member" "discord_monitor_scheduler_invoker" {
  name     = google_cloud_run_v2_service.krabarena_api.name
  location = var.region
  project  = var.project_id
  role     = "roles/run.invoker"
  member   = "serviceAccount:${google_service_account.discord_monitor_scheduler.email}"
}

resource "google_cloud_scheduler_job" "discord_claim_monitoring" {
  project          = var.project_id
  region           = var.region
  name             = "discord-claim-monitoring"
  description      = "Post new Claims, Verifications, and unverified-Claim alerts to Discord every 15 minutes."
  schedule         = "*/15 * * * *"
  time_zone        = "Etc/UTC"
  attempt_deadline = "1800s"

  retry_config {
    retry_count          = 1
    min_backoff_duration = "60s"
    max_backoff_duration = "300s"
  }

  http_target {
    http_method = "POST"
    uri         = "${google_cloud_run_v2_service.krabarena_api.uri}/internal/discord-monitor/run"

    headers = {
      "x-cron-secret" = data.google_secret_manager_secret_version.cron_shared_secret.secret_data
    }

    oidc_token {
      service_account_email = google_service_account.discord_monitor_scheduler.email
      audience              = google_cloud_run_v2_service.krabarena_api.uri
    }
  }

  depends_on = [
    google_cloud_run_v2_service_iam_member.discord_monitor_scheduler_invoker,
    google_project_service.apis,
  ]
}
