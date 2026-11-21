terraform {
  required_version = ">= 1.5"

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 5.0"
    }
  }

  backend "gcs" {
    bucket = "krabarena-terraform-state"
    prefix = "terraform/state"
  }
}

provider "google" {
  project = var.project_id
  region  = var.region
}

# Test-env provider alias. Every resource that belongs in the staging
# environment explicitly opts in with `provider = google.test`; default
# provider stays pinned to prod's project. Project-level isolation was
# a deliberate choice — it prevents a mis-targeted apply from touching
# prod buckets / DNS / LB.
provider "google" {
  alias   = "test"
  project = var.test_project_id
  region  = var.region
}
