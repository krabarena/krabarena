resource "google_project_service" "monitoring_apis" {
  for_each = toset([
    "monitoring.googleapis.com",
  ])

  project                    = var.project_id
  service                    = each.value
  disable_dependent_services = false
  disable_on_destroy         = false
}

# Uptime check: GET /health every 60s from multiple regions
resource "google_monitoring_uptime_check_config" "krabarena_health" {
  display_name = "krabarena-api-health"
  project      = var.project_id
  timeout      = "10s"
  period       = "60s"

  http_check {
    path         = "/health"
    port         = 443
    use_ssl      = true
    validate_ssl = true

    accepted_response_status_codes {
      status_class = "STATUS_CLASS_2XX"
    }
  }

  monitored_resource {
    type = "uptime_url"
    labels = {
      project_id = var.project_id
      host       = trimprefix(google_cloud_run_v2_service.krabarena_api.uri, "https://")
    }
  }

  selected_regions = [
    "USA",
    "EUROPE",
    "ASIA_PACIFIC",
  ]

  depends_on = [google_project_service.monitoring_apis]
}

# Uptime check: GET /about (UI smoke test)
resource "google_monitoring_uptime_check_config" "krabarena_ui" {
  display_name = "krabarena-ui-about"
  project      = var.project_id
  timeout      = "10s"
  period       = "300s"

  http_check {
    path         = "/about"
    port         = 443
    use_ssl      = true
    validate_ssl = true

    accepted_response_status_codes {
      status_class = "STATUS_CLASS_2XX"
    }
  }

  monitored_resource {
    type = "uptime_url"
    labels = {
      project_id = var.project_id
      host       = trimprefix(google_cloud_run_v2_service.krabarena_api.uri, "https://")
    }
  }

  selected_regions = ["USA"]

  depends_on = [google_project_service.monitoring_apis]
}

# Notification channel: email
resource "google_monitoring_notification_channel" "email" {
  display_name = "krabarena-alerts-email"
  project      = var.project_id
  type         = "email"

  labels = {
    email_address = var.alert_email
  }

  depends_on = [google_project_service.monitoring_apis]
}

# Alert: health check failure
resource "google_monitoring_alert_policy" "health_check_failure" {
  display_name = "KrabArena API Health Check Failure"
  project      = var.project_id
  combiner     = "OR"

  conditions {
    display_name = "Health endpoint down"

    condition_threshold {
      filter          = "resource.type = \"uptime_url\" AND metric.type = \"monitoring.googleapis.com/uptime_check/check_passed\" AND metric.labels.check_id = \"${google_monitoring_uptime_check_config.krabarena_health.uptime_check_id}\""
      comparison      = "COMPARISON_GT"
      threshold_value = 1
      duration        = "120s"

      aggregations {
        alignment_period     = "60s"
        per_series_aligner   = "ALIGN_NEXT_OLDER"
        cross_series_reducer = "REDUCE_COUNT_FALSE"
        group_by_fields      = ["resource.label.project_id"]
      }

      trigger {
        count = 1
      }
    }
  }

  notification_channels = [
    google_monitoring_notification_channel.email.id,
  ]

  alert_strategy {
    auto_close = "1800s"
  }

  depends_on = [google_project_service.monitoring_apis]
}

# Log-based distribution metric: per-request latency in seconds for
# everything EXCEPT the KrabReviewer paths. Cloud Run's built-in
# `request_latencies` has no path label, so we can't filter
# KrabReviewer out of it — the result was that every legitimate
# multi-second review tripped the general 3s alert. Mirror the
# same `httpRequest.latency` extractor used in
# `krabreviewer_latency_seconds` below but invert the path filter so
# the two metrics partition the request space cleanly.
resource "google_logging_metric" "general_latency_seconds" {
  name    = "general_latency_seconds"
  project = var.project_id

  description = "End-to-end latency (s) for all KrabArena Cloud Run requests EXCEPT the KrabReviewer-driven POST endpoints."

  # Anything that is NOT one of the KrabReviewer paths covered by
  # `krabreviewer_latency_seconds`. Net negative — we want the GET
  # noise (home/battle/claim renders), the non-reviewer POSTs
  # (auth, votes, comments), AND `POST /v1/claims` itself (the
  # bare claim-post handler doesn't run Gemini; it returns <3s).
  filter = <<-EOT
    resource.type = "cloud_run_revision"
    resource.labels.service_name = "krabarena-api"
    NOT (
      httpRequest.requestMethod = "POST"
      AND
      (
        httpRequest.requestUrl =~ "/v1/claims/review($|\\?)"
        OR httpRequest.requestUrl =~ "/v1/battles/[^/]+/review($|\\?)"
        OR httpRequest.requestUrl =~ "/v1/battles/[^/]+/regenerate-summary($|\\?)"
      )
    )
  EOT

  metric_descriptor {
    metric_kind  = "DELTA"
    value_type   = "DISTRIBUTION"
    unit         = "s"
    display_name = "General request latency (s)"
  }

  value_extractor = "REGEXP_EXTRACT(httpRequest.latency, \"^([0-9.]+)s$\")"

  bucket_options {
    exponential_buckets {
      num_finite_buckets = 14
      growth_factor      = 1.6
      scale              = 0.1
    }
  }

  depends_on = [google_project_service.monitoring_apis]
}

# Alert: general (non-reviewer) request latency >3s p95.
#
# Threshold tuned to typical page renders (home is the most expensive
# at ~600ms-1s warm, ~5-8s on a cold replica + cold CMS cache + cold
# Neon compute). 3s p95 over 5 minutes catches a real degradation
# without paging on routine cold-starts. KrabReviewer endpoints are
# carved out via the metric's filter (above) — they have their own
# 120s SLO in the `krabreviewer_latency` alert below.
resource "google_monitoring_alert_policy" "high_latency" {
  display_name = "KrabArena API High Latency (>3s p95)"
  project      = var.project_id
  combiner     = "OR"

  conditions {
    display_name = "Non-reviewer request latency >3s"

    condition_threshold {
      filter          = "resource.type = \"cloud_run_revision\" AND resource.labels.service_name = \"krabarena-api\" AND metric.type = \"logging.googleapis.com/user/${google_logging_metric.general_latency_seconds.name}\""
      comparison      = "COMPARISON_GT"
      threshold_value = 3
      duration        = "300s"

      aggregations {
        alignment_period   = "60s"
        per_series_aligner = "ALIGN_PERCENTILE_95"
      }

      trigger {
        count = 1
      }
    }
  }

  notification_channels = [
    google_monitoring_notification_channel.email.id,
  ]

  alert_strategy {
    auto_close = "1800s"
  }

  depends_on = [google_project_service.monitoring_apis]
}

# Log-based distribution metric: per-request latency in seconds for
# the KrabReviewer POST endpoints. Cloud Run's built-in
# `request_latencies` metric has no path label, so we extract latency
# from the structured Cloud Run access logs via `httpRequest.latency`
# (formatted as "30.123s") and bucket it as a distribution.
#
# Tracked paths (POST-only — GET noise excluded):
#   /v1/claims                — claim post (runs the agentic reviewer)
#   /v1/claims/review         — dry-run review (same code path)
#   /v1/battles/{id}/review   — battle reviewer
#
# Buckets chosen for KrabReviewer's actual latency distribution:
# audit data shows stage 1 mean ~5s / p99 ~22s, stage 2 mean ~7s /
# p99 ~30s, with a wall-clock cap of 90s. Exponential buckets from
# 1s × 1.5 give us decent resolution from 1s up to ~129s.
resource "google_logging_metric" "krabreviewer_latency_seconds" {
  name    = "krabreviewer_latency_seconds"
  project = var.project_id

  description = "End-to-end latency (s) of KrabReviewer-driven POST endpoints, extracted from Cloud Run httpRequest.latency."

  # POST-only — GET noise excluded. Tracked paths (each calls
  # Gemini end-to-end and routinely takes >3s):
  #   /v1/claims/review                   — dry-run review (22-44s)
  #   /v1/battles/{id}/review             — battle reviewer
  #   /v1/battles/{id}/regenerate-summary — explicit summary regen (10-75s)
  #
  # `POST /v1/claims` (the actual claim post) is NOT here — it
  # doesn't run the reviewer. Callers POST to `/v1/claims/review`
  # first (which runs Gemini and returns a review_id), then POST to
  # `/v1/claims` with the review_id; that handler just persists the
  # claim and spawns the summary regen async, so it returns in <3s.
  filter = <<-EOT
    resource.type = "cloud_run_revision"
    resource.labels.service_name = "krabarena-api"
    httpRequest.requestMethod = "POST"
    (
      httpRequest.requestUrl =~ "/v1/claims/review($|\\?)"
      OR httpRequest.requestUrl =~ "/v1/battles/[^/]+/review($|\\?)"
      OR httpRequest.requestUrl =~ "/v1/battles/[^/]+/regenerate-summary($|\\?)"
    )
  EOT

  metric_descriptor {
    metric_kind  = "DELTA"
    value_type   = "DISTRIBUTION"
    unit         = "s"
    display_name = "KrabReviewer latency (s)"
  }

  # Extract the numeric part of "30.123s" — the trailing 's' is
  # always present on Cloud Run's httpRequest.latency.
  value_extractor = "REGEXP_EXTRACT(httpRequest.latency, \"^([0-9.]+)s$\")"

  bucket_options {
    exponential_buckets {
      num_finite_buckets = 12
      growth_factor      = 1.5
      scale              = 1.0
    }
  }

  depends_on = [google_project_service.monitoring_apis]
}

# Alert: KrabReviewer latency >120s p95 over 5 minutes.
#
# Threshold sits 30s past our 90s agentic_reviewer wall-clock cap —
# anything past 120s p95 means the cap isn't even being respected (a
# code-level regression in the budget gate) or there's a wildly slow
# downstream we haven't characterized. The normal review path
# completes in 10-30s; a tail of slow Gemini days pushes p95 toward
# ~60s but should not exceed 90s. Page if we're chronically near or
# past the cap.
resource "google_monitoring_alert_policy" "krabreviewer_latency" {
  display_name = "KrabReviewer Latency (>120s p95)"
  project      = var.project_id
  combiner     = "OR"

  conditions {
    display_name = "KrabReviewer POST latency >120s"

    condition_threshold {
      filter          = "resource.type = \"cloud_run_revision\" AND resource.labels.service_name = \"krabarena-api\" AND metric.type = \"logging.googleapis.com/user/${google_logging_metric.krabreviewer_latency_seconds.name}\""
      comparison      = "COMPARISON_GT"
      threshold_value = 120
      duration        = "300s"

      aggregations {
        alignment_period   = "60s"
        per_series_aligner = "ALIGN_PERCENTILE_95"
      }

      trigger {
        count = 1
      }
    }
  }

  notification_channels = [
    google_monitoring_notification_channel.email.id,
  ]

  alert_strategy {
    auto_close = "1800s"
  }

  depends_on = [google_project_service.monitoring_apis]
}

# Alert: high error rate (5xx)
resource "google_monitoring_alert_policy" "error_rate" {
  display_name = "KrabArena API Error Rate >5%"
  project      = var.project_id
  combiner     = "OR"

  conditions {
    display_name = "Cloud Run 5xx error rate"

    condition_threshold {
      filter          = "resource.type = \"cloud_run_revision\" AND resource.labels.service_name = \"krabarena-api\" AND metric.type = \"run.googleapis.com/request_count\" AND metric.labels.response_code_class = \"5xx\""
      comparison      = "COMPARISON_GT"
      threshold_value = 5
      duration        = "300s"

      aggregations {
        alignment_period   = "60s"
        per_series_aligner = "ALIGN_RATE"
      }

      trigger {
        count = 1
      }
    }
  }

  notification_channels = [
    google_monitoring_notification_channel.email.id,
  ]

  alert_strategy {
    auto_close = "1800s"
  }

  depends_on = [google_project_service.monitoring_apis]
}
