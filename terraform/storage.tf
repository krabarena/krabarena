resource "google_storage_bucket" "artifacts" {
  name     = "krabarena-artifacts"
  location = var.region
  project  = var.project_id

  uniform_bucket_level_access = true

  lifecycle_rule {
    condition {
      age = 365
    }
    action {
      type          = "SetStorageClass"
      storage_class = "NEARLINE"
    }
  }
}

# Allow Cloud Run service account to read/write artifacts
resource "google_storage_bucket_iam_member" "cloud_run_artifacts" {
  bucket = google_storage_bucket.artifacts.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${data.google_project.project.number}-compute@developer.gserviceaccount.com"
}

# Public read of individual objects ONLY. The previous binding granted
# allUsers `roles/storage.objectViewer`, which also carries
# `storage.objects.list` — anyone could enumerate every object in the
# bucket (including private artifacts that used to be stored here).
# This custom role carries `storage.objects.get` alone: a public URL
# still works, listing the bucket does not.
resource "google_project_iam_custom_role" "public_object_reader" {
  project     = var.project_id
  role_id     = "krabarenaPublicObjectReader"
  title       = "KrabArena public object reader (get only)"
  description = "Read a single object by name; no bucket listing. Granted to allUsers on krabarena-artifacts."
  permissions = ["storage.objects.get"]
}

resource "google_storage_bucket_iam_member" "public_object_get" {
  bucket = google_storage_bucket.artifacts.name
  role   = google_project_iam_custom_role.public_object_reader.id
  member = "allUsers"
}

# Private artifacts (a claim's paid / proprietary bundle). Never public:
# public access prevention is enforced, so an accidental allUsers /
# allAuthenticatedUsers binding is rejected by GCS. Only the Cloud Run
# runtime service account can read/write; the API streams objects to
# authorized users via `GET /v1/claims/{id}/private-artifact`.
# Wired to the app through the `PRIVATE_BLOB_STORE_URL` env var.
resource "google_storage_bucket" "artifacts_private" {
  name     = var.private_blob_store_bucket
  location = var.region
  project  = var.project_id

  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"

  versioning {
    enabled = true
  }
}

resource "google_storage_bucket_iam_member" "cloud_run_artifacts_private" {
  bucket = google_storage_bucket.artifacts_private.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${data.google_project.project.number}-compute@developer.gserviceaccount.com"
}

# Test environment (`krabarena-test` project) gets its own private
# bucket so E2E runs never touch prod's private artifacts.
data "google_project" "test" {
  provider   = google.test
  project_id = var.test_project_id
}

resource "google_storage_bucket" "artifacts_private_test" {
  provider = google.test

  name     = var.test_private_blob_store_bucket
  location = var.region
  project  = var.test_project_id

  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"

  lifecycle_rule {
    condition {
      age = 30
    }
    action {
      type = "Delete"
    }
  }
}

resource "google_storage_bucket_iam_member" "test_cloud_run_artifacts_private" {
  provider = google.test

  bucket = google_storage_bucket.artifacts_private_test.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${data.google_project.test.number}-compute@developer.gserviceaccount.com"
}

data "google_project" "project" {
  project_id = var.project_id
}
