resource "google_project_service" "apis" {
  for_each = toset([
    "run.googleapis.com",
    "artifactregistry.googleapis.com",
    "cloudbuild.googleapis.com",
    "compute.googleapis.com",
    "secretmanager.googleapis.com",
    # Memorystore for Redis + Serverless VPC Access — back the
    # API rate limiter (since 0.35.0). See `redis.tf`.
    "redis.googleapis.com",
    "vpcaccess.googleapis.com",
    # Cloud Scheduler jobs (Discord monitor) and their service accounts.
    "cloudscheduler.googleapis.com",
    "iam.googleapis.com",
    # Score-based sign-in bot gate: the Assessment API the backend calls
    # (services::captcha_service) and where the Enterprise SCORE key lives.
    "recaptchaenterprise.googleapis.com",
    # API-key management (the Enterprise-restricted key used to auth the
    # assessment call is created here / via gcloud).
    "apikeys.googleapis.com",
  ])

  project = var.project_id
  service = each.value

  disable_dependent_services = false
  disable_on_destroy         = false
}

# Test project APIs (originally for the retired KrabScout/KrabChecker
# Cloud Run workers; kept enabled — disabling APIs is disruptive). `run.googleapis.com`
# is enabled here too even though it's already implicitly active
# via the Cloud Run resources, so a future operator running
# `terraform apply` on a fresh project gets a clean bootstrap.
resource "google_project_service" "test_apis" {
  for_each = toset([
    "run.googleapis.com",
    "pubsub.googleapis.com",
    "eventarc.googleapis.com",
    "secretmanager.googleapis.com",
    "iam.googleapis.com",
  ])
  provider = google.test

  project = var.test_project_id
  service = each.value

  disable_dependent_services = false
  disable_on_destroy         = false
}
