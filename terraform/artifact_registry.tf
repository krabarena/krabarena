resource "google_artifact_registry_repository" "krabarena" {
  location      = var.region
  repository_id = "krabarena-repo"
  format        = "DOCKER"
  project       = var.project_id

  depends_on = [google_project_service.apis]
}
