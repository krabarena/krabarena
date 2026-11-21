# Memorystore for Redis — backs the API rate limiter
# (`web::rate_limit`, since 0.26.0).
#
# Cloud Run is serverless and has no VPC presence by default, while a
# Memorystore instance is only reachable from inside the authorized
# VPC. The bridge is a Serverless VPC Access connector: the Cloud Run
# revision is deployed with `--vpc-connector` (wired in
# `.github/workflows/deploy.yml`) so its egress to the Redis private IP
# routes through the connector.
#
# Terraform owns the instance + connector + API enablement here. The
# `REDIS_URL` env var (redis://<host>:6379) is injected at deploy time
# by the workflow, NOT by `terraform/cloud_run.tf` — that resource has
# `ignore_changes = [template]`, so the container env is owned out of
# band by `gcloud run deploy`. The limiter fails open and is gated on
# `REDIS_URL`, so the service runs fine (limiter disabled) until the
# var is wired.

# The project's default VPC. Both the connector and the Redis instance
# live here so the connector's egress can reach the instance's private
# IP. (A dedicated network would also work; default keeps the blast
# radius small and avoids peering setup.)
data "google_compute_network" "default" {
  name    = "default"
  project = var.project_id
}

# Serverless VPC Access connector. Needs an unused /28 in the VPC.
# `10.8.0.0/28` is outside the default subnet ranges; change it if it
# ever collides with another reservation.
resource "google_vpc_access_connector" "redis" {
  name          = "krabarena-redis-conn"
  project       = var.project_id
  region        = var.region
  network       = data.google_compute_network.default.name
  ip_cidr_range = "10.8.0.0/28"

  # Smallest supported sizing — the limiter does one tiny round-trip
  # per request; throughput is trivial.
  min_instances = 2
  max_instances = 3

  depends_on = [google_project_service.apis]
}

# The Redis instance. Basic tier (no replica/failover) is sufficient —
# rate-limit counters are ephemeral and the middleware fails open, so a
# brief Redis outage degrades to "no limiting", never to an outage.
resource "google_redis_instance" "rate_limiter" {
  name           = "krabarena-rate-limiter"
  project        = var.project_id
  region         = var.region
  tier           = "BASIC"
  memory_size_gb = 1

  authorized_network = data.google_compute_network.default.id
  connect_mode       = "DIRECT_PEERING"

  redis_version = "REDIS_7_2"
  display_name  = "KrabArena API rate limiter"

  depends_on = [google_project_service.apis]
}

# Surfaced for the deploy workflow / operators. `host` feeds the
# `REDIS_URL` (redis://<host>:6379) set on the Cloud Run revision.
output "redis_host" {
  value       = google_redis_instance.rate_limiter.host
  description = "Memorystore private IP — use in REDIS_URL on Cloud Run."
}

output "redis_vpc_connector" {
  value       = google_vpc_access_connector.redis.name
  description = "VPC connector name — pass to `gcloud run deploy --vpc-connector`."
}
