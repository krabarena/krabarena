# Mailgun sending infrastructure for the `mg.krabarena.com` subdomain.
#
# Mirrors `email_dns.tf` but lives in the krabarena-com Cloud DNS zone
# (created in `dns.tf`). The Mailgun domain itself (`mg.krabarena.com`)
# was created via the Mailgun API before this file was written, so the
# DKIM value below is the real key Mailgun issued — no placeholder.
#
# Cross-domain DMARC auth: krabarena.com's DMARC policy reports to
# `dmarc@krabarena.org`, which RFC 7489 §7.1 requires the report
# domain (.org) to opt into via a TXT record at
# `krabarena.com._report._dmarc.krabarena.org`. That record is
# defined at the bottom of this file in the .org zone.
#
# Rotation: regenerate the DKIM key in the Mailgun console, then
# update the `mailgun_com_dkim_value` variable (or terraform.tfvars)
# and `terraform apply`. The substr-based split below handles any
# value length up to ~510 chars (2× the 255-byte TXT string limit).

variable "mailgun_com_dkim_value" {
  description = "Full DKIM TXT value Mailgun issued for mg.krabarena.com (selector `mx`). Includes the `k=rsa; p=` prefix."
  type        = string
  default     = "k=rsa; p=MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEApdWvm+62Ra8qbsQEUdbMfr7HexNwVEcM91sZ1l0i7f+AFfS3v5vKHKln1V4AKG/kinPGvIDQG3tDfAmNkYRLhWWZfdzMf0NOfFW33Kwa12B370kRfmQ07LG01BDihlx3YLWsz6/yxic8ypAQ7+1Ex9/pGQK8YiMu0cID+Dpmmw+hYdHl8iwBBeICJSB1/ufr0IkdwoI2h5TyMitgNGUTwPkwKWy/YXWs0UB9mPKCHQ7T5cUFF8rkbvpVL7HTp4G9k0FZCmjMr960gpBcaYlPpJi4Ri1RggnCXhqTUildGO5LnJcFat+9Lb86O2lZwl+Y3CJ06Km5FTLpBGJFdQyKnwIDAQAB"
}

locals {
  # DNS TXT records cap each quoted string at 255 bytes; resolvers
  # concatenate adjacent strings. Split at 250 to leave headroom.
  mailgun_com_dkim_chunk1 = substr(var.mailgun_com_dkim_value, 0, 250)
  mailgun_com_dkim_chunk2 = substr(var.mailgun_com_dkim_value, 250, length(var.mailgun_com_dkim_value) - 250)
}

# Mailgun MX — receives bounces and suppression signals for mg.krabarena.com.
resource "google_dns_record_set" "mg_com_mx" {
  name         = "mg.krabarena.com."
  type         = "MX"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena_com.name
  project      = var.project_id
  rrdatas = [
    "10 mxa.mailgun.org.",
    "10 mxb.mailgun.org.",
  ]
}

# SPF — authorise Mailgun to send on behalf of mg.krabarena.com.
resource "google_dns_record_set" "mg_com_spf" {
  name         = "mg.krabarena.com."
  type         = "TXT"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena_com.name
  project      = var.project_id
  rrdatas      = ["\"v=spf1 include:mailgun.org ~all\""]
}

# Tracking CNAME — Mailgun rewrites click/open tracking links through
# email.mg.krabarena.com so they show the sender's domain.
resource "google_dns_record_set" "mg_com_tracking" {
  name         = "email.mg.krabarena.com."
  type         = "CNAME"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena_com.name
  project      = var.project_id
  rrdatas      = ["mailgun.org."]
}

# DKIM — Mailgun signs with d=mg.krabarena.com so DMARC aligns.
# Selector `mx` matches Mailgun's default for auto-created domains.
resource "google_dns_record_set" "mg_com_dkim" {
  name         = "mx._domainkey.mg.krabarena.com."
  type         = "TXT"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena_com.name
  project      = var.project_id
  rrdatas      = ["\"${local.mailgun_com_dkim_chunk1}\" \"${local.mailgun_com_dkim_chunk2}\""]
}

# DMARC for krabarena.com — same policy as .org (quarantine, full
# coverage, relaxed alignment). Reports flow to dmarc@krabarena.org
# via the cross-domain auth record below.
resource "google_dns_record_set" "apex_com_dmarc" {
  name         = "_dmarc.krabarena.com."
  type         = "TXT"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena_com.name
  project      = var.project_id
  rrdatas = [
    "\"v=DMARC1; p=quarantine; rua=mailto:${var.dmarc_rua_email_com}; pct=100; adkim=r; aspf=r\"",
  ]
}

# RFC 7489 §7.1 cross-domain authorisation: krabarena.org agrees to
# accept DMARC aggregate reports about krabarena.com. Without this
# TXT, receivers (Google, Microsoft, etc.) silently drop the reports.
resource "google_dns_record_set" "dmarc_auth_com_to_org" {
  name         = "krabarena.com._report._dmarc.krabarena.org."
  type         = "TXT"
  ttl          = 3600
  managed_zone = google_dns_managed_zone.krabarena.name
  project      = var.project_id
  rrdatas      = ["\"v=DMARC1\""]
}
