variable "project_id" {
  description = "GCP project ID"
  type        = string
  default     = "krabbit-dev"
}

variable "test_project_id" {
  description = "GCP project ID for the staging / test environment"
  type        = string
  default     = "krabarena-test"
}

variable "region" {
  description = "GCP region"
  type        = string
  default     = "us-central1"
}

variable "image" {
  description = "Container image for krabarena-api"
  type        = string
  default     = "us-central1-docker.pkg.dev/krabbit-dev/krabarena-repo/krabarena-api:latest"
}

variable "database_url" {
  description = "Neon PostgreSQL connection string"
  type        = string
  sensitive   = true
}

variable "jwt_secret" {
  description = "JWT signing secret"
  type        = string
  sensitive   = true
}

variable "alert_email" {
  description = "Email address for monitoring alerts"
  type        = string
}

variable "blob_store_url" {
  description = "Artifact blob store URL (gs://bucket for GCS, http://... for dev)"
  type        = string
  default     = "gs://krabarena-artifacts"
}

variable "private_blob_store_bucket" {
  description = "Non-public GCS bucket for claims' private artifacts (prod). Exposed to the app as PRIVATE_BLOB_STORE_URL=gs://<name>."
  type        = string
  default     = "krabarena-artifacts-private"
}

variable "test_private_blob_store_bucket" {
  description = "Non-public GCS bucket for private artifacts in the krabarena-test project."
  type        = string
  default     = "krabarena-test-artifacts-private"
}
