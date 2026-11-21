# Mailgun sending infrastructure for the `mg.krabarena.org` subdomain.
#
# These records were bootstrapped via `gcloud dns record-sets create`
# before being codified here, so
# the first reconcile on a fresh state must `terraform import` each
# resource instead of plain `apply` — otherwise TF will try to
# recreate records that already exist and fail.
#
# DKIM selector `mx` is what Mailgun auto-generated when the domain
# was created; rotate via the Mailgun console and re-paste the raw
# p= value into mailgun_dkim_value (split across chunk1/chunk2 because
# the full TXT is ~400 chars and DNS caps a single quoted string at
# 255 bytes).

variable "mailgun_dkim_value_chunk1" {
  description = "First ≤255-byte chunk of the DKIM TXT record (including the 'k=rsa; p=' prefix)."
  type        = string
  default     = "k=rsa; p=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA3Y8zMY6Eh1/39DVdxxwRN8LuXNtiK1SlFD1D4gvzxldbFhJJbk0ctoKvyzYcaEJBLgLh0fPURkp8on23i6awsNABSAwuWD2dxil+w0RU1TMqoXPvYzBuPzpIDJZeedE3GFgBCYtbc4meok/Ylm+XMAxFPer7t7p3s8GI+axikxFUlXhuOwXTuashrVfV92OTt"
}

variable "mailgun_dkim_value_chunk2" {
  description = "Remaining chunk of the DKIM TXT record."
  type        = string
  default     = "8CoVvY2ju8VdIDC3NTXMooPClVT8Ah5d970vTcdAnydwlurYAH7KUcH+sadh6ur2zuVoq2ITXVT3LlFkzydXq7veg9RcrS9bASAfFgDOERNoM/mRkF6IwKBauIh+te2e+iSti299OgusMlu2UauWwIDAQAB"
}

variable "dmarc_rua_email" {
  description = "Where DMARC aggregate reports should be delivered."
  type        = string
  default     = "dmarc@krabarena.org"
}

variable "dmarc_rua_email_com" {
  description = "Where DMARC aggregate reports for krabarena.com should be delivered."
  type        = string
  default     = "dmarc@mg.krabarena.com"
}

# Mailgun MX — receives bounces and suppression signals for mg.krabarena.org.
resource "google_dns_record_set" "mg_mx" {
  name         = "mg.krabarena.org."
  type         = "MX"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas = [
    "10 mxa.mailgun.org.",
    "10 mxb.mailgun.org.",
  ]
}

# SPF — authorise Mailgun to send on behalf of mg.krabarena.org.
resource "google_dns_record_set" "mg_spf" {
  name         = "mg.krabarena.org."
  type         = "TXT"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas      = ["\"v=spf1 include:mailgun.org ~all\""]
}

# Tracking CNAME — Mailgun rewrites click/open tracking links through
# email.mg.krabarena.org so they show the sender's domain.
resource "google_dns_record_set" "mg_tracking" {
  name         = "email.mg.krabarena.org."
  type         = "CNAME"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas      = ["mailgun.org."]
}

# DKIM — Mailgun signs with d=mg.krabarena.org so DMARC aligns.
# Selector `mx` is Mailgun's default for auto-created domains.
resource "google_dns_record_set" "mg_dkim" {
  name         = "mx._domainkey.mg.krabarena.org."
  type         = "TXT"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas      = ["\"${var.mailgun_dkim_value_chunk1}\" \"${var.mailgun_dkim_value_chunk2}\""]
}

# DMARC — quarantine now, tighten to reject after 30 days of clean RUA reports.
resource "google_dns_record_set" "apex_dmarc" {
  name         = "_dmarc.krabarena.org."
  type         = "TXT"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas = [
    "\"v=DMARC1; p=quarantine; rua=mailto:${var.dmarc_rua_email}; pct=100; adkim=r; aspf=r\"",
  ]
}
