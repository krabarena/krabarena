# Cloud DNS zones for krabarena.org (current canonical) and
# krabarena.com (incoming canonical — see PR migrating canonical URLs).
#
# Both zones live behind the same load balancer IP. The .com zone is
# created here so GoDaddy NS for krabarena.com can be flipped to GCP;
# until that NS flip propagates the .com A/CNAME records resolve only
# inside Cloud DNS and the managed SSL cert sits in PROVISIONING.
#
# GoDaddy nameservers for BOTH domains must be set to:
#   ns-cloud-d1.googledomains.com
#   ns-cloud-d2.googledomains.com
#   ns-cloud-d3.googledomains.com
#   ns-cloud-d4.googledomains.com

resource "google_dns_managed_zone" "krabarena" {
  name        = "krabarena-org"
  dns_name    = "krabarena.org."
  description = "KrabArena production domain"
  project     = var.project_id
}

# Root domain → load balancer IP
resource "google_dns_record_set" "root" {
  name         = "krabarena.org."
  type         = "A"
  ttl          = 300
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas      = [google_compute_global_address.krabarena.address]
}

# www → root
resource "google_dns_record_set" "www" {
  name         = "www.krabarena.org."
  type         = "CNAME"
  ttl          = 300
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas      = ["krabarena.org."]
}

# Google Search Console site verification (apex TXT).
# Bootstrapped via `gcloud dns record-sets create` then codified here, so a
# fresh reconcile must `terraform import` it (same pattern as email_dns.tf):
#   terraform import google_dns_record_set.google_site_verification \
#     krabbit-dev/krabarena-org/krabarena.org./TXT
resource "google_dns_record_set" "google_site_verification" {
  name         = "krabarena.org."
  type         = "TXT"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas      = ["\"google-site-verification=u7Abkrbzd58kpY6idr-KqDGTh2SRlGseNqnKyYlk8BQ\""]
}

resource "google_dns_managed_zone" "krabarena_com" {
  name        = "krabarena-com"
  dns_name    = "krabarena.com."
  description = "KrabArena production domain (.com — parallel with .org)"
  project     = var.project_id
}

resource "google_dns_record_set" "root_com" {
  name         = "krabarena.com."
  type         = "A"
  ttl          = 300
  managed_zone = google_dns_managed_zone.krabarena_com.name
  project      = var.project_id
  rrdatas      = [google_compute_global_address.krabarena.address]
}

resource "google_dns_record_set" "www_com" {
  name         = "www.krabarena.com."
  type         = "CNAME"
  ttl          = 300
  managed_zone = google_dns_managed_zone.krabarena_com.name
  project      = var.project_id
  rrdatas      = ["krabarena.com."]
}

# Surface the NS records for krabarena.com so they can be pasted into
# GoDaddy without a second `gcloud dns managed-zones describe` round-trip.
output "krabarena_com_nameservers" {
  value = google_dns_managed_zone.krabarena_com.name_servers
}
