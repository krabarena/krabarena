resource "google_cloud_run_v2_service" "krabarena_api" {
  name     = "krabarena-api"
  location = var.region
  project  = var.project_id

  ingress = "INGRESS_TRAFFIC_ALL"

  template {
    containers {
      image = var.image

      ports {
        container_port = 8080
      }

      resources {
        limits = {
          cpu    = "1"
          memory = "512Mi"
        }
      }

      env {
        name  = "DATABASE_URL"
        value = var.database_url
      }

      env {
        name  = "JWT_SECRET"
        value = var.jwt_secret
      }

      env {
        name  = "BLOB_STORE_URL"
        value = var.blob_store_url
      }

      env {
        name  = "PRIVATE_BLOB_STORE_URL"
        value = "gs://${google_storage_bucket.artifacts_private.name}"
      }
    }

    scaling {
      min_instance_count = 0
      max_instance_count = 10
    }
  }

  # The GitHub Actions deploy workflow (`.github/workflows/deploy.yml`)
  # owns the full container template at deploy time — image tag, env
  # vars (Gemini / Mailgun / OAuth providers / CACHE_BUST), revision
  # traffic. Terraform only guarantees the service exists; runtime
  # config is managed out-of-band via `gcloud run deploy`.
  lifecycle {
    ignore_changes = [
      template,
      client,
      client_version,
    ]
  }

  depends_on = [google_project_service.apis]
}

# Allow unauthenticated access
resource "google_cloud_run_v2_service_iam_member" "public" {
  name     = google_cloud_run_v2_service.krabarena_api.name
  location = var.region
  project  = var.project_id
  role     = "roles/run.invoker"
  member   = "allUsers"
}

output "cloud_run_url" {
  value = google_cloud_run_v2_service.krabarena_api.uri
}
