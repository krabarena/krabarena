resource "google_compute_region_network_endpoint_group" "krabarena_neg" {
  name                  = "krabarena-neg"
  region                = var.region
  project               = var.project_id
  network_endpoint_type = "SERVERLESS"

  cloud_run {
    service = google_cloud_run_v2_service.krabarena_api.name
  }
}

resource "google_compute_backend_service" "krabarena" {
  name                            = "krabarena-backend"
  project                         = var.project_id
  load_balancing_scheme           = "EXTERNAL_MANAGED"
  protocol                        = "HTTP"
  port_name                       = "http"
  timeout_sec                     = 30
  connection_draining_timeout_sec = 300

  backend {
    group = google_compute_region_network_endpoint_group.krabarena_neg.id
  }
}

resource "google_compute_url_map" "krabarena" {
  name            = "krabarena-lb"
  project         = var.project_id
  default_service = google_compute_backend_service.krabarena.id

  # Canonical host is the apex krabarena.com. The login session lives in
  # per-origin localStorage, so any other host (www.*, the whole .org zone)
  # is a separate origin where the session is invisible — users arriving on
  # e.g. www.krabarena.com from Google appear logged out. 301 those hosts to
  # the apex so there is exactly one session origin (and one SEO canonical).
  host_rule {
    hosts        = ["www.krabarena.com", "krabarena.org", "www.krabarena.org"]
    path_matcher = "redirect-to-apex"
  }

  path_matcher {
    name = "redirect-to-apex"
    default_url_redirect {
      host_redirect          = "krabarena.com"
      https_redirect         = true
      redirect_response_code = "MOVED_PERMANENTLY_DEFAULT"
      strip_query            = false
    }
  }
}

resource "google_compute_global_address" "krabarena" {
  name    = "krabarena-ip"
  project = var.project_id
}

# Managed cert name is bumped every time the domain list changes —
# `name` and `managed.domains` are both immutable, so adding or
# trimming a domain forces replacement. `create_before_destroy`
# requires the new cert to coexist with the old under a different
# name; reusing the name would collide.
#
# v4 drops krabbit.keenable.ai. The Route53 record for that hostname
# in `keenable-infra` had already been repointed away from this LB
# (to 35.244.208.38), so Google's validation prober for v3 returned
# FAILED_NOT_VISIBLE on that SAN, which permanently blocked the cert
# from reaching ACTIVE. Removing the SAN — and the matching Route53
# record in the sibling repo — closes that loop.
resource "google_compute_managed_ssl_certificate" "krabarena" {
  name    = "krabarena-ssl-v4"
  project = var.project_id

  managed {
    domains = [
      "krabarena.org",
      "www.krabarena.org",
      "krabarena.com",
      "www.krabarena.com",
    ]
  }

  lifecycle {
    create_before_destroy = true
  }
}

resource "google_compute_ssl_policy" "tls12_modern" {
  name            = "tls12-modern"
  project         = var.project_id
  profile         = "MODERN"
  min_tls_version = "TLS_1_2"
}

resource "google_compute_target_https_proxy" "krabarena" {
  name             = "krabarena-https-proxy"
  project          = var.project_id
  url_map          = google_compute_url_map.krabarena.id
  ssl_certificates = [google_compute_managed_ssl_certificate.krabarena.id]
  ssl_policy       = google_compute_ssl_policy.tls12_modern.id
}

resource "google_compute_global_forwarding_rule" "krabarena_https" {
  name                  = "krabarena-https-rule"
  project               = var.project_id
  load_balancing_scheme = "EXTERNAL_MANAGED"
  target                = google_compute_target_https_proxy.krabarena.id
  ip_address            = google_compute_global_address.krabarena.id
  port_range            = "443"
}

resource "google_compute_url_map" "krabarena_redirect" {
  name    = "krabarena-http-redirect"
  project = var.project_id

  default_url_redirect {
    https_redirect         = true
    redirect_response_code = "MOVED_PERMANENTLY_DEFAULT"
    strip_query            = false
  }
}

resource "google_compute_target_http_proxy" "krabarena" {
  name    = "krabarena-http-proxy"
  project = var.project_id
  url_map = google_compute_url_map.krabarena_redirect.id
}

resource "google_compute_global_forwarding_rule" "krabarena" {
  name                  = "krabarena-http-rule"
  project               = var.project_id
  load_balancing_scheme = "EXTERNAL_MANAGED"
  target                = google_compute_target_http_proxy.krabarena.id
  ip_address            = google_compute_global_address.krabarena.id
  port_range            = "80"
}

output "lb_ip" {
  value = google_compute_global_address.krabarena.address
}
