#############################################################################
# Cloud Run test environment in project `krabarena-test`.
#
# Staging service sitting between `Build & Push` and prod `Deploy`:
#
#   Build & Push ─► Deploy to test ─► E2E against test ─► Deploy to prod
#
# Project-level isolation: test lives in a separate GCP project
# (`krabarena-test`) so a mis-targeted Terraform apply on the test
# plane cannot touch prod's buckets, DNS, load balancer, or secrets.
# Same image registry (`krabbit-dev/krabarena-repo`) — we don't want
# "prod passes but test fails because they're running different
# images". The test project's default compute SA was granted
# `roles/artifactregistry.reader` on the prod registry out-of-band.
#
# No custom domain. The default `*.run.app` URL is enough — this is
# a CI gate, not user-facing. Test-only code paths (e.g. `POST
# /v1/test/mint-token`) wake up when `TEST_AUTH_SECRET` is set in
# the runtime env, which the deploy workflow sets here but never in
# prod.
#
# As with prod, Terraform only guarantees the service exists; image
# tag and env vars at runtime are owned by the `deploy-test`
# workflow, so we `ignore_changes` on template/client.
#############################################################################

resource "google_cloud_run_v2_service" "krabarena_api_test" {
  provider = google.test

  name     = "krabarena-api"
  location = var.region
  project  = var.test_project_id

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
        value = "gs://${google_storage_bucket.artifacts_private_test.name}"
      }
    }

    scaling {
      min_instance_count = 0
      max_instance_count = 2
    }
  }

  lifecycle {
    ignore_changes = [
      template,
      client,
      client_version,
    ]
  }
}

resource "google_cloud_run_v2_service_iam_member" "test_public" {
  provider = google.test

  name     = google_cloud_run_v2_service.krabarena_api_test.name
  location = var.region
  project  = var.test_project_id
  role     = "roles/run.invoker"
  member   = "allUsers"
}

output "cloud_run_test_url" {
  value = google_cloud_run_v2_service.krabarena_api_test.uri
}
