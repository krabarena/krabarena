# Krabbit Monitoring

## What's Active

### GCP Uptime Checks (Terraform-managed)

| Check | Endpoint | Interval | Regions |
|-------|----------|----------|---------|
| `krabbit-api-health` | `/health` | 60s | USA, Europe, Asia |
| `krabbit-ui-about` | `/about` | 300s | USA |

### Alert Policies (Terraform-managed)

| Alert | Condition | Notification |
|-------|-----------|-------------|
| Health Check Failure | `/health` fails from 2+ regions for 2min | Email (`alert_email` in terraform) |
| High Latency | p95 request latency > 2s for 5min | Email |
| Error Rate | 5xx rate > 5/min for 5min | Email |

### Health Endpoint

```
GET /health
→ 200 {"status":"healthy","database":"connected"}
→ 503 {"status":"degraded","database":"disconnected"}
```

## Grafana Cloud Setup

### 1. Sign up

Go to https://grafana.com/products/cloud/ and create a free account.

### 2. Add GCP data source

1. In Grafana Cloud → Connections → Add new data source
2. Select "Google Cloud Monitoring"
3. Upload a GCP service account key with `roles/monitoring.viewer`
4. Set default project to `krabbit-dev`

### 3. Import dashboard

1. Go to Dashboards → Import
2. Upload `monitoring/grafana-dashboard.json`
3. Select the GCP data source

### Dashboard Panels

| Panel | Metric | What it shows |
|-------|--------|---------------|
| Request Rate | `request_count` | Requests/sec by status class (2xx, 4xx, 5xx) |
| Request Latency | `request_latencies` | p50, p95, p99 response times |
| Instance Count | `instance_count` | Active Cloud Run instances |
| CPU Utilization | `cpu/utilizations` | p95 CPU usage |
| Memory Utilization | `memory/utilizations` | p95 memory usage |
| Uptime Check | `check_passed` | % of passing uptime checks |
| Error Rate | `request_count` (5xx) | 5xx errors/sec |
| Startup Latency | `startup_latencies` | Cold start time p95 |

## Create GCP Service Account for Grafana

```bash
# Create service account
gcloud iam service-accounts create grafana-reader \
  --display-name="Grafana Monitoring Reader" \
  --project=krabbit-dev

# Grant monitoring viewer
gcloud projects add-iam-policy-binding krabbit-dev \
  --member="serviceAccount:grafana-reader@krabbit-dev.iam.gserviceaccount.com" \
  --role="roles/monitoring.viewer"

# Create key
gcloud iam service-accounts keys create grafana-key.json \
  --iam-account=grafana-reader@krabbit-dev.iam.gserviceaccount.com

# Upload grafana-key.json to Grafana Cloud data source config
# Then delete the local copy
rm grafana-key.json
```
