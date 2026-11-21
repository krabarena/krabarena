# GitHub Actions → GCP auth via Workload Identity Federation.
#
# Bootstrapped out-of-band with `gcloud`, codified here. On a fresh state these
# resources must be `terraform import`ed (commands at the bottom of this file),
# not plain-applied, or TF will try to recreate live resources and fail.
#
# RENAME HAZARD: repos are allow-listed by exact name in BOTH the provider
# attribute_condition AND the per-repo workloadIdentityUser bindings. When a
# GitHub repo is renamed (e.g. krabarena/krabscout → krabarena/krabagent in
# 2026-06), update BOTH lists here or CI auth breaks:
#   - wrong name vs condition  → "rejected by the attribute condition"
#   - wrong name vs binding     → "iam.serviceAccounts.getAccessToken denied"
# To make this rename-proof, consider mapping/pinning on the immutable
# assertion.repository_id instead of assertion.repository.

data "google_project" "github_wif" {
  project_id = var.project_id
}

resource "google_iam_workload_identity_pool" "github" {
  project                   = var.project_id
  workload_identity_pool_id = "github-pool"
  display_name              = "GitHub Actions Pool"
}

resource "google_iam_workload_identity_pool_provider" "github" {
  project                            = var.project_id
  workload_identity_pool_id          = google_iam_workload_identity_pool.github.workload_identity_pool_id
  workload_identity_pool_provider_id = "github-provider"

  attribute_mapping = {
    "google.subject"       = "assertion.sub"
    "attribute.repository" = "assertion.repository"
  }

  # Only these repos may exchange a GitHub OIDC token for GCP credentials.
  attribute_condition = "assertion.repository == \"krabarena/krabarena-private\" || assertion.repository == \"krabarena/krabagent\""

  oidc {
    issuer_uri = "https://token.actions.githubusercontent.com"
  }
}

# Repos allowed to impersonate the github-deployer service account.
locals {
  github_deployer_repos = [
    "krabarena/krabarena-private",
    "krabarena/krabagent",
  ]
  github_deployer_sa = "github-deployer@${var.project_id}.iam.gserviceaccount.com"
}

resource "google_service_account_iam_member" "github_deployer_wif" {
  for_each           = toset(local.github_deployer_repos)
  service_account_id = "projects/${var.project_id}/serviceAccounts/${local.github_deployer_sa}"
  role               = "roles/iam.workloadIdentityUser"
  member             = "principalSet://iam.googleapis.com/projects/${data.google_project.github_wif.number}/locations/global/workloadIdentityPools/github-pool/attribute.repository/${each.key}"
}

# One-time import on a fresh state (state backend + ADC must be authed):
#   terraform import google_iam_workload_identity_pool.github \
#     projects/krabbit-dev/locations/global/workloadIdentityPools/github-pool
#   terraform import google_iam_workload_identity_pool_provider.github \
#     projects/krabbit-dev/locations/global/workloadIdentityPools/github-pool/providers/github-provider
#   # for each repo R in local.github_deployer_repos:
#   terraform import 'google_service_account_iam_member.github_deployer_wif["R"]' \
#     "projects/krabbit-dev/serviceAccounts/github-deployer@krabbit-dev.iam.gserviceaccount.com roles/iam.workloadIdentityUser principalSet://iam.googleapis.com/projects/186425104524/locations/global/workloadIdentityPools/github-pool/attribute.repository/R"
